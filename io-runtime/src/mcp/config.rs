//! MCP server configuration: the Claude-Code-compatible `.mcp.json` format,
//! where it lives (user and project scope), `${VAR}` expansion, and the trust
//! store that gates servers a project file asks us to launch.
//!
//! ```json
//! { "mcpServers": {
//!     "files":  { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
//!     "remote": { "type": "http", "url": "https://mcp.example.com/mcp",
//!                 "headers": { "Authorization": "Bearer ${TOKEN}" } }
//! } }
//! ```

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One server entry, as written in the config file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// `stdio`, `http` or `sse`. Inferred from `command` / `url` when absent.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// stdio: the program to launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// stdio: extra environment for the child.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// http / sse: the server endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// http / sse: extra request headers (e.g. `Authorization`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// How to reach a server, after validation and `${VAR}` expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
    /// The older HTTP+SSE transport.
    Sse {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl Launch {
    pub fn transport_name(&self) -> &'static str {
        match self {
            Launch::Stdio { .. } => "stdio",
            Launch::Http { .. } => "http",
            Launch::Sse { .. } => "sse",
        }
    }
}

impl McpServerConfig {
    /// The transport this entry uses: the explicit `type`, or `url` → http,
    /// `command` → stdio.
    pub fn transport_kind(&self) -> Result<&'static str, String> {
        match self
            .transport
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("stdio") => Ok("stdio"),
            Some("http" | "streamable-http" | "streamable_http") => Ok("http"),
            Some("sse") => Ok("sse"),
            Some(other) => Err(format!(
                "unknown transport '{other}' (use stdio, http or sse)"
            )),
            None if self.url.is_some() => Ok("http"),
            None if self.command.is_some() => Ok("stdio"),
            None => Err("needs a `command` (stdio) or a `url` (http/sse)".to_string()),
        }
    }

    /// Validate and expand `${VAR}` / `${VAR:-default}` everywhere.
    pub fn resolve(&self) -> Result<Launch, String> {
        self.resolve_with(&|name| std::env::var(name).ok())
    }

    fn resolve_with(&self, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Launch, String> {
        let expand = |s: &str| expand_env_with(s, lookup);
        let expand_map =
            |m: &BTreeMap<String, String>| -> Result<BTreeMap<String, String>, String> {
                m.iter().map(|(k, v)| Ok((k.clone(), expand(v)?))).collect()
            };
        match self.transport_kind()? {
            "stdio" => {
                let command = self
                    .command
                    .as_deref()
                    .filter(|c| !c.trim().is_empty())
                    .ok_or("a stdio server needs a `command`")?;
                Ok(Launch::Stdio {
                    command: expand(command)?,
                    args: self
                        .args
                        .iter()
                        .map(|a| expand(a))
                        .collect::<Result<_, _>>()?,
                    env: expand_map(&self.env)?,
                })
            }
            kind => {
                let url = self
                    .url
                    .as_deref()
                    .filter(|u| !u.trim().is_empty())
                    .ok_or_else(|| format!("an {kind} server needs a `url`"))?;
                let url = expand(url)?;
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(format!(
                        "`url` must start with http:// or https:// (got '{url}')"
                    ));
                }
                let headers = expand_map(&self.headers)?;
                Ok(if kind == "sse" {
                    Launch::Sse { url, headers }
                } else {
                    Launch::Http { url, headers }
                })
            }
        }
    }

    /// One-line description for listings; secrets in headers and env stay out.
    pub fn summary(&self) -> String {
        match (&self.command, &self.url) {
            (Some(cmd), _) if self.transport_kind() == Ok("stdio") => {
                if self.args.is_empty() {
                    cmd.clone()
                } else {
                    format!("{cmd} {}", self.args.join(" "))
                }
            }
            (_, Some(url)) => url.clone(),
            _ => "(incomplete)".to_string(),
        }
    }

    /// A stable fingerprint of this entry, so the trust store notices when a
    /// project file changes what an approved server would launch.
    pub fn fingerprint(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        hex::encode(Sha256::digest(json.as_bytes()))
    }
}

/// Expand `${VAR}` and `${VAR:-default}`. An unset variable without a default
/// is an error rather than a silent empty string.
pub fn expand_env_with(
    input: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!("unterminated ${{ in '{input}'"));
        };
        let spec = &after[..end];
        let (name, default) = match spec.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (spec, None),
        };
        match lookup(name)
            .filter(|v| !v.is_empty())
            .or(default.map(str::to_string))
        {
            Some(value) => out.push_str(&value),
            None => return Err(format!("environment variable {name} is not set")),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Server names become part of tool names (`mcp__<server>__<tool>`), so they
/// are limited to characters tool names allow, and `__` is the separator.
pub fn validate_server_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 32 {
        return Err("a server name must be 1-32 characters".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("a server name may only contain letters, digits, '-' and '_'".to_string());
    }
    if name.contains("__") {
        return Err("a server name cannot contain '__' (it separates the tool name)".to_string());
    }
    Ok(())
}

// ── Files ──────────────────────────────────────────────────────────────────────

/// Where a server definition came from. Later scopes override earlier ones
/// with the same name: plugin < user < project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Plugin(String),
    User,
    Project,
}

impl Scope {
    pub fn label(&self) -> String {
        match self {
            Scope::Plugin(p) => format!("plugin:{p}"),
            Scope::User => "user".to_string(),
            Scope::Project => "project".to_string(),
        }
    }
}

pub fn user_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".io")
        .join("mcp.json")
}

pub fn project_config_path(root: &Path) -> PathBuf {
    root.join(".mcp.json")
}

/// Read the `mcpServers` of a config file. A missing file is empty; a file
/// that is not valid JSON, or has a malformed entry, is an error naming it.
pub fn read_servers(path: &Path) -> Result<BTreeMap<String, McpServerConfig>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    parse_servers(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse_servers(text: &str) -> Result<BTreeMap<String, McpServerConfig>, String> {
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let Some(servers) = value.get("mcpServers") else {
        return Ok(BTreeMap::new());
    };
    let obj = servers
        .as_object()
        .ok_or("`mcpServers` must be an object")?;
    obj.iter()
        .map(|(name, v)| {
            serde_json::from_value::<McpServerConfig>(v.clone())
                .map(|c| (name.clone(), c))
                .map_err(|e| format!("server '{name}': {e}"))
        })
        .collect()
}

/// Add or replace one server, keeping everything else in the file.
pub fn write_server(path: &Path, name: &str, config: &McpServerConfig) -> Result<(), String> {
    validate_server_name(name)?;
    let mut doc = read_document(path)?;
    doc["mcpServers"][name] = serde_json::to_value(config).map_err(|e| e.to_string())?;
    write_document(path, &doc)
}

/// Remove one server; `Ok(false)` if it wasn't there.
pub fn remove_server(path: &Path, name: &str) -> Result<bool, String> {
    let mut doc = read_document(path)?;
    let removed = doc
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
        .map(|s| s.remove(name).is_some())
        .unwrap_or(false);
    if removed {
        write_document(path, &doc)?;
    }
    Ok(removed)
}

fn read_document(path: &Path) -> Result<serde_json::Value, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => {
            serde_json::from_str(&t).map_err(|e| format!("{}: invalid JSON: {e}", path.display()))
        }
        Ok(_) => Ok(serde_json::json!({})),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn write_document(path: &Path, doc: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
    std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    // Headers and env often hold tokens.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// A server to start, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    pub name: String,
    pub scope: Scope,
    pub config: McpServerConfig,
}

/// Merge plugin, user and project servers (later scopes win by name). Errors
/// from one unreadable file don't hide the others; they come back separately.
pub fn collect_entries(
    project_root: &Path,
    plugin_servers: Vec<(String, String, McpServerConfig)>,
) -> (Vec<ServerEntry>, Vec<String>) {
    let mut by_name: BTreeMap<String, ServerEntry> = BTreeMap::new();
    let mut problems = Vec::new();
    for (plugin, name, config) in plugin_servers {
        by_name.insert(
            name.clone(),
            ServerEntry {
                name,
                scope: Scope::Plugin(plugin),
                config,
            },
        );
    }
    for (scope, path) in [
        (Scope::User, user_config_path()),
        (Scope::Project, project_config_path(project_root)),
    ] {
        match read_servers(&path) {
            Ok(servers) => {
                for (name, config) in servers {
                    by_name.insert(
                        name.clone(),
                        ServerEntry {
                            name,
                            scope: scope.clone(),
                            config,
                        },
                    );
                }
            }
            Err(e) => problems.push(e),
        }
    }
    (by_name.into_values().collect(), problems)
}

// ── Trust ──────────────────────────────────────────────────────────────────────

/// What the user has decided about a project-scope server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Approved,
    Denied,
    Unknown,
}

/// Remembers, per project and per server definition, whether the user allowed
/// a `.mcp.json` server to launch. A project file is untrusted input — it can
/// name any command — so nothing from it runs until approved, and changing a
/// server's definition asks again.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(flatten)]
    projects: BTreeMap<String, BTreeMap<String, bool>>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl TrustStore {
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".io")
            .join("mcp_trust.json")
    }

    pub fn load() -> Self {
        Self::load_from(Self::default_path())
    }

    pub fn load_from(path: PathBuf) -> Self {
        let mut store: TrustStore = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        store.path = Some(path);
        store
    }

    fn key(name: &str, config: &McpServerConfig) -> String {
        format!("{name}:{}", config.fingerprint())
    }

    pub fn status(&self, root: &Path, name: &str, config: &McpServerConfig) -> Trust {
        match self
            .projects
            .get(&root.display().to_string())
            .and_then(|p| p.get(&Self::key(name, config)))
        {
            Some(true) => Trust::Approved,
            Some(false) => Trust::Denied,
            None => Trust::Unknown,
        }
    }

    pub fn set(
        &mut self,
        root: &Path,
        name: &str,
        config: &McpServerConfig,
        approved: bool,
    ) -> Result<(), String> {
        self.projects
            .entry(root.display().to_string())
            .or_default()
            .insert(Self::key(name, config), approved);
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        write_document(
            path,
            &serde_json::to_value(self).map_err(|e| e.to_string())?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("io_mcp_cfg_{}_{name}", uuid::Uuid::new_v4()))
    }

    fn stdio(cmd: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            command: Some(cmd.to_string()),
            args: args.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_stdio_http_and_sse_entries() {
        let servers = parse_servers(
            r#"{ "mcpServers": {
                "files": { "command": "npx", "args": ["-y", "srv"], "env": {"A": "1"} },
                "remote": { "type": "http", "url": "https://x.dev/mcp",
                            "headers": {"Authorization": "Bearer t"} },
                "legacy": { "type": "sse", "url": "http://localhost:1/sse" }
            } }"#,
        )
        .unwrap();
        assert_eq!(servers.len(), 3);
        assert_eq!(servers["files"].transport_kind(), Ok("stdio"));
        assert_eq!(servers["remote"].transport_kind(), Ok("http"));
        assert_eq!(servers["legacy"].transport_kind(), Ok("sse"));
        assert_eq!(servers["files"].env["A"], "1");
    }

    #[test]
    fn the_transport_is_inferred_and_validated() {
        let url_only = McpServerConfig {
            url: Some("https://a.dev".into()),
            ..Default::default()
        };
        assert_eq!(url_only.transport_kind(), Ok("http"));
        assert!(McpServerConfig::default().transport_kind().is_err());
        let bad = McpServerConfig {
            transport: Some("carrier-pigeon".into()),
            ..stdio("x", &[])
        };
        assert!(bad
            .transport_kind()
            .unwrap_err()
            .contains("unknown transport"));
        assert!(McpServerConfig {
            url: Some("ftp://nope".into()),
            ..Default::default()
        }
        .resolve()
        .unwrap_err()
        .contains("http://"));
    }

    #[test]
    fn malformed_files_report_which_server_is_wrong() {
        assert!(parse_servers("not json")
            .unwrap_err()
            .contains("invalid JSON"));
        assert!(parse_servers(r#"{"mcpServers": []}"#)
            .unwrap_err()
            .contains("object"));
        assert!(parse_servers(r#"{"mcpServers": {"x": {"args": "nope"}}}"#)
            .unwrap_err()
            .contains("server 'x'"));
        assert!(parse_servers("").unwrap().is_empty());
        assert!(parse_servers("{}").unwrap().is_empty());
    }

    #[test]
    fn env_expansion_handles_defaults_and_missing_variables() {
        let vars = |n: &str| match n {
            "TOKEN" => Some("s3cret".to_string()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        let ex = |s: &str| expand_env_with(s, &vars);
        assert_eq!(ex("Bearer ${TOKEN}").unwrap(), "Bearer s3cret");
        assert_eq!(ex("${NOPE:-fallback}").unwrap(), "fallback");
        assert_eq!(ex("${EMPTY:-fallback}").unwrap(), "fallback");
        assert_eq!(ex("no vars $here").unwrap(), "no vars $here");
        assert_eq!(ex("${TOKEN}-${TOKEN}").unwrap(), "s3cret-s3cret");
        assert!(ex("${NOPE}").unwrap_err().contains("NOPE is not set"));
        assert!(ex("${oops").unwrap_err().contains("unterminated"));
    }

    #[test]
    fn resolution_expands_every_field() {
        let cfg = McpServerConfig {
            command: Some("${BIN:-srv}".into()),
            args: vec!["--token".into(), "${TOKEN}".into()],
            env: [("K".to_string(), "${TOKEN}".to_string())].into(),
            ..Default::default()
        };
        let launch = cfg
            .resolve_with(&|n| (n == "TOKEN").then(|| "abc".to_string()))
            .unwrap();
        assert_eq!(
            launch,
            Launch::Stdio {
                command: "srv".into(),
                args: vec!["--token".into(), "abc".into()],
                env: [("K".to_string(), "abc".to_string())].into(),
            }
        );
        assert!(cfg.resolve_with(&|_| None).is_err(), "TOKEN is unset");
    }

    #[test]
    fn server_names_are_restricted_to_what_tool_names_allow() {
        for ok in ["github", "my-server_2", "a"] {
            assert!(validate_server_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "has space", "dots.dots", "a__b", &"x".repeat(33)] {
            assert!(validate_server_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn writing_and_removing_preserves_the_rest_of_the_file() {
        let path = temp_path("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"keep":{"command":"a"}},"other":{"x":1}}"#,
        )
        .unwrap();
        write_server(&path, "new", &stdio("b", &["--flag"])).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"other\""), "unrelated keys survive: {text}");
        let servers = read_servers(&path).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers["new"].args, ["--flag"]);

        assert!(remove_server(&path, "new").unwrap());
        assert!(!remove_server(&path, "new").unwrap());
        assert_eq!(read_servers(&path).unwrap().len(), 1);
        std::fs::remove_file(&path).ok();
        assert!(
            read_servers(&path).unwrap().is_empty(),
            "missing file is empty"
        );
        assert!(write_server(&path, "bad name", &stdio("x", &[])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn written_config_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("private.json");
        write_server(&path, "s", &stdio("x", &[])).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn trust_is_per_project_and_per_definition() {
        let path = temp_path("trust.json");
        let root = Path::new("/work/app");
        let cfg = stdio("srv", &["a"]);
        let mut store = TrustStore::load_from(path.clone());
        assert_eq!(store.status(root, "s", &cfg), Trust::Unknown);

        store.set(root, "s", &cfg, true).unwrap();
        assert_eq!(store.status(root, "s", &cfg), Trust::Approved);
        // Another project, another name, or a changed definition: ask again.
        assert_eq!(store.status(Path::new("/other"), "s", &cfg), Trust::Unknown);
        assert_eq!(store.status(root, "t", &cfg), Trust::Unknown);
        assert_eq!(
            store.status(root, "s", &stdio("srv", &["b"])),
            Trust::Unknown
        );

        store.set(root, "t", &cfg, false).unwrap();
        // Survives a reload.
        let again = TrustStore::load_from(path.clone());
        assert_eq!(again.status(root, "s", &cfg), Trust::Approved);
        assert_eq!(again.status(root, "t", &cfg), Trust::Denied);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn summaries_show_the_launch_without_secrets() {
        assert_eq!(stdio("npx", &["-y", "srv"]).summary(), "npx -y srv");
        let http = McpServerConfig {
            url: Some("https://x.dev/mcp".into()),
            headers: [("Authorization".to_string(), "Bearer secret".to_string())].into(),
            ..Default::default()
        };
        assert_eq!(http.summary(), "https://x.dev/mcp");
        assert_ne!(stdio("a", &[]).fingerprint(), stdio("b", &[]).fingerprint());
    }

    #[test]
    fn project_servers_override_user_and_plugin_ones() {
        let root = std::env::temp_dir().join(format!("io_mcp_root_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        write_server(
            &project_config_path(&root),
            "dup",
            &stdio("project-cmd", &[]),
        )
        .unwrap();
        let (entries, problems) = collect_entries(
            &root,
            vec![
                ("pl".into(), "dup".into(), stdio("plugin-cmd", &[])),
                ("pl".into(), "only-plugin".into(), stdio("p", &[])),
            ],
        );
        assert!(problems.is_empty(), "{problems:?}");
        let dup = entries.iter().find(|e| e.name == "dup").unwrap();
        assert_eq!(dup.scope, Scope::Project);
        assert_eq!(dup.config.command.as_deref(), Some("project-cmd"));
        assert!(entries
            .iter()
            .any(|e| e.name == "only-plugin" && e.scope == Scope::Plugin("pl".into())));
        std::fs::remove_dir_all(&root).ok();
    }
}
