//! MCP from the CLI side: the `io mcp …` subcommands, the process-wide
//! [`McpManager`] the REPL and agents share, and the startup trust prompt for
//! project (`.mcp.json`) servers.

use clap::{Subcommand, ValueEnum};
use io_runtime::mcp::{
    collect_entries, partition_by_trust, project_config_path, read_servers, remove_server,
    user_config_path, write_server, McpManager, McpServerConfig, ServerEntry, ServerState,
    ServerStatus, Settings, TrustStore,
};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScopeArg {
    /// `~/.io/mcp.json` — available in every project
    User,
    /// `.mcp.json` in the project root — shared with the repo (needs approval)
    Project,
}

#[derive(Subcommand)]
pub enum McpAction {
    /// Add a server: `io mcp add [options] <name> <command> [args…]` or
    /// `io mcp add -t http <name> <url>`. Options go before the name.
    Add {
        /// Where to store the server
        #[arg(long, value_enum, default_value = "user")]
        scope: ScopeArg,
        /// stdio, http or sse (default: http for a URL, stdio otherwise)
        #[arg(short = 't', long)]
        transport: Option<String>,
        /// Environment for a stdio server: KEY=VALUE (repeatable)
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// Header for an http/sse server: "Key: Value" (repeatable)
        #[arg(short = 'H', long = "header")]
        header: Vec<String>,
        name: String,
        /// The command and its arguments, or the URL
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        target: Vec<String>,
    },
    /// List configured servers and whether they connect
    List,
    /// Show one server's configuration (secrets are masked)
    Get { name: String },
    /// Remove a server
    Remove {
        /// Required only if the name exists in both scopes
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        name: String,
    },
}

// ── Building a config from CLI arguments ───────────────────────────────────────

pub fn parse_env_pairs(pairs: &[String]) -> Result<BTreeMap<String, String>, String> {
    pairs
        .iter()
        .map(|p| match p.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.to_string())),
            _ => Err(format!("invalid --env '{p}' (expected KEY=VALUE)")),
        })
        .collect()
}

pub fn parse_headers(headers: &[String]) -> Result<BTreeMap<String, String>, String> {
    headers
        .iter()
        .map(|h| match h.split_once(':') {
            Some((k, v)) if !k.trim().is_empty() => {
                Ok((k.trim().to_string(), v.trim().to_string()))
            }
            _ => Err(format!("invalid --header '{h}' (expected \"Key: Value\")")),
        })
        .collect()
}

pub fn build_config(
    transport: Option<&str>,
    env: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    target: &[String],
) -> Result<McpServerConfig, String> {
    let first = target.first().ok_or("missing command or URL")?;
    let is_url = first.starts_with("http://") || first.starts_with("https://");
    let mut config = McpServerConfig {
        transport: transport.map(str::to_string),
        ..Default::default()
    };
    match config.transport_kind().or_else(|e| {
        if transport.is_some() {
            Err(e)
        } else {
            Ok(if is_url { "http" } else { "stdio" })
        }
    })? {
        "stdio" => {
            if !headers.is_empty() {
                return Err("--header only applies to http/sse servers".to_string());
            }
            config.command = Some(first.clone());
            config.args = target[1..].to_vec();
            config.env = env;
        }
        kind => {
            if !env.is_empty() {
                return Err("--env only applies to stdio servers".to_string());
            }
            if target.len() != 1 {
                return Err(format!("an {kind} server takes just a URL"));
            }
            if !is_url {
                return Err(format!("'{first}' is not an http:// or https:// URL"));
            }
            config.url = Some(first.clone());
            config.headers = headers;
        }
    }
    // Only record the transport when it can't be inferred.
    if transport == Some("stdio") && config.command.is_some()
        || transport == Some("http") && config.url.is_some()
    {
        config.transport = None;
    }
    Ok(config)
}

fn mask(map: &BTreeMap<String, String>, sep: &str) -> Vec<String> {
    map.keys().map(|k| format!("{k}{sep}***")).collect()
}

fn scope_path(scope: ScopeArg, root: &Path) -> std::path::PathBuf {
    match scope {
        ScopeArg::User => user_config_path(),
        ScopeArg::Project => project_config_path(root),
    }
}

// ── `io mcp …` ─────────────────────────────────────────────────────────────────

pub async fn handle(action: McpAction) -> anyhow::Result<()> {
    let root = crate::tui::detect_project_root();
    match action {
        McpAction::Add {
            scope,
            transport,
            env,
            header,
            name,
            target,
        } => {
            let config = build_config(
                transport.as_deref(),
                parse_env_pairs(&env).map_err(anyhow::Error::msg)?,
                parse_headers(&header).map_err(anyhow::Error::msg)?,
                &target,
            )
            .map_err(anyhow::Error::msg)?;
            let path = scope_path(scope, &root);
            write_server(&path, &name, &config).map_err(anyhow::Error::msg)?;
            println!("Added MCP server '{name}' to {}", path.display());
            if scope == ScopeArg::Project {
                println!("It runs after you approve it the next time you start io here.");
            }
        }
        McpAction::List => {
            let notes = start(false, &root).await;
            let states = manager().states().to_vec();
            if states.is_empty() && notes.is_empty() {
                println!("No MCP servers configured. Add one with `io mcp add`.");
            }
            for line in state_lines(&states) {
                println!("{line}");
            }
            for note in notes {
                eprintln!("{note}");
            }
        }
        McpAction::Get { name } => {
            let (entries, _) = collect_entries(&root, Vec::new());
            let entry = entries
                .into_iter()
                .find(|e| e.name == name)
                .ok_or_else(|| anyhow::anyhow!("no MCP server named '{name}'"))?;
            let c = &entry.config;
            println!("{name}");
            println!("  scope:     {}", entry.scope.label());
            println!(
                "  transport: {}",
                c.transport_kind().map_err(anyhow::Error::msg)?
            );
            if let Some(cmd) = &c.command {
                println!("  command:   {cmd}");
            }
            if !c.args.is_empty() {
                println!("  args:      {}", c.args.join(" "));
            }
            if let Some(url) = &c.url {
                println!("  url:       {url}");
            }
            if !c.env.is_empty() {
                println!("  env:       {}", mask(&c.env, "=").join(", "));
            }
            if !c.headers.is_empty() {
                println!("  headers:   {}", mask(&c.headers, ": ").join(", "));
            }
        }
        McpAction::Remove { scope, name } => {
            let has = |s: ScopeArg| {
                read_servers(&scope_path(s, &root))
                    .map(|m| m.contains_key(&name))
                    .unwrap_or(false)
            };
            let scope = match scope {
                Some(s) => s,
                None => match (has(ScopeArg::User), has(ScopeArg::Project)) {
                    (true, false) => ScopeArg::User,
                    (false, true) => ScopeArg::Project,
                    (true, true) => anyhow::bail!(
                        "'{name}' exists in both user and project scope; pass --scope"
                    ),
                    (false, false) => anyhow::bail!("no MCP server named '{name}'"),
                },
            };
            let path = scope_path(scope, &root);
            if remove_server(&path, &name).map_err(anyhow::Error::msg)? {
                println!("Removed MCP server '{name}' from {}", path.display());
            } else {
                anyhow::bail!("no MCP server named '{name}' in {}", path.display());
            }
        }
    }
    Ok(())
}

// ── The shared manager ─────────────────────────────────────────────────────────

fn slot() -> &'static Mutex<Option<Arc<McpManager>>> {
    static SLOT: Mutex<Option<Arc<McpManager>>> = Mutex::new(None);
    &SLOT
}

/// The live connections (empty until [`start`] has run).
pub fn manager() -> Arc<McpManager> {
    slot()
        .lock()
        .unwrap()
        .get_or_insert_with(|| Arc::new(McpManager::empty()))
        .clone()
}

enum Answer {
    Approve,
    Skip,
    Deny,
}

fn ask_trust(entry: &ServerEntry) -> Answer {
    println!(
        "This project's .mcp.json wants to run MCP server '{}':\n  {}",
        entry.name,
        entry.config.summary()
    );
    print!("  [y] approve and remember · [n] skip this time · [d] never: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Answer::Skip;
    }
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => Answer::Approve,
        "d" | "never" => Answer::Deny,
        _ => Answer::Skip,
    }
}

/// (Re)connect to every configured server and make the result the shared
/// manager. Project servers nobody has approved are asked about when
/// `interactive` (plain stdin — call before the TUI takes the screen) and
/// skipped otherwise. Returns notes worth showing the user.
pub async fn start(interactive: bool, root: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    let (entries, problems) = collect_entries(root, Vec::new());
    notes.extend(problems.into_iter().map(|p| format!("mcp: {p}")));

    let mut trust = TrustStore::load();
    let partition = partition_by_trust(entries, root, &trust);
    let mut to_start = partition.start;
    let mut skipped: Vec<(ServerEntry, ServerStatus)> = partition
        .denied
        .into_iter()
        .map(|e| (e, ServerStatus::Denied))
        .collect();

    for entry in partition.unknown {
        let answer = if interactive {
            ask_trust(&entry)
        } else {
            notes.push(format!(
                "mcp: project server '{}' needs approval — start io interactively in this project to review it",
                entry.name
            ));
            Answer::Skip
        };
        match answer {
            Answer::Approve => {
                if let Err(e) = trust.set(root, &entry.name, &entry.config, true) {
                    notes.push(format!("mcp: could not remember approval: {e}"));
                }
                to_start.push(entry);
            }
            Answer::Deny => {
                if let Err(e) = trust.set(root, &entry.name, &entry.config, false) {
                    notes.push(format!("mcp: could not remember choice: {e}"));
                }
                skipped.push((entry, ServerStatus::Denied));
            }
            Answer::Skip => skipped.push((entry, ServerStatus::NeedsApproval)),
        }
    }

    if interactive && !to_start.is_empty() {
        println!("Connecting to {} MCP server(s)…", to_start.len());
    }
    let mgr = McpManager::start(to_start, skipped, Settings::from_env()).await;
    for state in mgr.states() {
        if let ServerStatus::Failed(why) = &state.status {
            notes.push(format!("mcp: '{}' failed: {why}", state.name));
        }
    }
    *slot().lock().unwrap() = Some(Arc::new(mgr));
    notes
}

/// One line per server for `io mcp list` and `/mcp`.
pub fn state_lines(states: &[ServerState]) -> Vec<String> {
    states
        .iter()
        .map(|s| {
            format!(
                "{}  [{}, {}]  {}  — {}",
                s.name,
                s.scope.label(),
                s.transport,
                s.summary,
                s.status.label()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn env_pairs_split_on_the_first_equals() {
        let m = parse_env_pairs(&s(&["A=1", "B=x=y", "C="])).unwrap();
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "x=y");
        assert_eq!(m["C"], "");
        assert!(parse_env_pairs(&s(&["nope"])).is_err());
        assert!(parse_env_pairs(&s(&["=v"])).is_err());
    }

    #[test]
    fn headers_trim_around_the_colon() {
        let m = parse_headers(&s(&["Authorization: Bearer a:b", "X-Y:z"])).unwrap();
        assert_eq!(m["Authorization"], "Bearer a:b");
        assert_eq!(m["X-Y"], "z");
        assert!(parse_headers(&s(&["no colon"])).is_err());
    }

    #[test]
    fn a_command_becomes_a_stdio_server() {
        let c = build_config(
            None,
            parse_env_pairs(&s(&["K=v"])).unwrap(),
            BTreeMap::new(),
            &s(&["npx", "-y", "server"]),
        )
        .unwrap();
        assert_eq!(c.command.as_deref(), Some("npx"));
        assert_eq!(c.args, s(&["-y", "server"]));
        assert_eq!(c.env["K"], "v");
        assert_eq!(c.transport_kind(), Ok("stdio"));
        assert!(c.transport.is_none());
    }

    #[test]
    fn a_url_becomes_an_http_server_unless_told_otherwise() {
        let h = parse_headers(&s(&["A: b"])).unwrap();
        let c = build_config(None, BTreeMap::new(), h.clone(), &s(&["https://x.dev/mcp"])).unwrap();
        assert_eq!(c.transport_kind(), Ok("http"));
        assert_eq!(c.headers["A"], "b");
        let c = build_config(Some("sse"), BTreeMap::new(), h, &s(&["https://x.dev/sse"])).unwrap();
        assert_eq!(c.transport_kind(), Ok("sse"));
        assert_eq!(c.transport.as_deref(), Some("sse"));
    }

    #[test]
    fn mismatched_options_are_rejected() {
        let env = parse_env_pairs(&s(&["K=v"])).unwrap();
        let hdr = parse_headers(&s(&["A: b"])).unwrap();
        assert!(build_config(None, env.clone(), BTreeMap::new(), &s(&["https://x.dev"])).is_err());
        assert!(build_config(None, BTreeMap::new(), hdr, &s(&["server"])).is_err());
        assert!(build_config(
            Some("http"),
            BTreeMap::new(),
            BTreeMap::new(),
            &s(&["server"])
        )
        .is_err());
        assert!(build_config(
            Some("http"),
            BTreeMap::new(),
            BTreeMap::new(),
            &s(&["https://x.dev", "extra"])
        )
        .is_err());
        assert!(build_config(Some("carrier-pigeon"), env, BTreeMap::new(), &s(&["x"])).is_err());
        assert!(build_config(None, BTreeMap::new(), BTreeMap::new(), &[]).is_err());
    }

    #[test]
    fn secrets_are_masked() {
        let m = parse_env_pairs(&s(&["TOKEN=hunter2"])).unwrap();
        assert_eq!(mask(&m, "="), s(&["TOKEN=***"]));
    }
}
