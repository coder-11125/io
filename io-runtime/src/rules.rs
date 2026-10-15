//! Rule-based permissions: `allow` / `ask` / `deny` rules written as
//! `Tool` or `Tool(specifier)`, e.g. `Bash(npm run test:*)`, `Read(./.env)`,
//! `Edit(src/**)`, `WebFetch(domain:example.com)` or `mcp__github__*`.
//!
//! Precedence is deny > ask > allow, applied by the permission checker. Rules
//! match conservatively: a deny or ask rule fires if it matches *any* part of
//! a compound shell command, while an allow rule must cover *every* part and
//! never applies to a command whose real text is hidden behind an expansion.

use globset::GlobBuilder;
use std::path::{Component, Path, PathBuf};

/// Which family of tools a rule talks about.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Bash,
    /// `read`, `glob`, `grep`.
    Read,
    /// `write`, `edit`, `multi_edit`, `notebook_edit`.
    Edit,
    WebFetch,
    WebSearch,
    /// Any other tool by exact name (`spawn_agent`) or MCP rule
    /// (`mcp__server`, `mcp__server__*`).
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    kind: Kind,
    spec: Option<String>,
    raw: String,
}

impl Rule {
    pub fn parse(text: &str) -> Result<Rule, String> {
        let raw = text.trim();
        if raw.is_empty() {
            return Err("empty rule".to_string());
        }
        let (name, spec) = match raw.find('(') {
            Some(open) => {
                if !raw.ends_with(')') {
                    return Err(format!("'{raw}': missing closing ')'"));
                }
                let spec = raw[open + 1..raw.len() - 1].trim();
                (
                    raw[..open].trim(),
                    (!spec.is_empty()).then(|| spec.to_string()),
                )
            }
            None => (raw, None),
        };
        if name.is_empty() {
            return Err(format!("'{raw}': missing tool name"));
        }
        let kind = match name {
            "Bash" | "bash" => Kind::Bash,
            "Read" | "read" | "Glob" | "glob" | "Grep" | "grep" => Kind::Read,
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "edit" | "write" | "multi_edit"
            | "notebook_edit" => Kind::Edit,
            "WebFetch" | "fetch" => Kind::WebFetch,
            "WebSearch" | "web_search" => Kind::WebSearch,
            other => Kind::Other(other.to_string()),
        };
        if spec.is_some() && matches!(kind, Kind::Other(_)) {
            return Err(format!("'{raw}': {name} does not take a specifier"));
        }
        Ok(Rule {
            kind,
            spec,
            raw: raw.to_string(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

/// The three rule lists plus what path rules are resolved against.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    allow: Vec<Rule>,
    ask: Vec<Rule>,
    deny: Vec<Rule>,
    root: Option<PathBuf>,
}

impl RuleSet {
    /// Build from rule strings. Unparseable rules are returned as problems and
    /// otherwise ignored (a typo must not silently become "allow everything",
    /// which is why an unparseable *deny* is reported prominently by callers).
    pub fn from_strings(
        allow: &[String],
        ask: &[String],
        deny: &[String],
        root: Option<PathBuf>,
    ) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        let mut parse = |list: &[String]| -> Vec<Rule> {
            list.iter()
                .filter_map(|r| match Rule::parse(r) {
                    Ok(rule) => Some(rule),
                    Err(e) => {
                        problems.push(e);
                        None
                    }
                })
                .collect()
        };
        let set = RuleSet {
            allow: parse(allow),
            ask: parse(ask),
            deny: parse(deny),
            root,
        };
        (set, problems)
    }

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.ask.is_empty() && self.deny.is_empty()
    }

    pub fn denies(&self, tool: &str, input: &serde_json::Value) -> bool {
        self.deny.iter().any(|r| self.any_match(r, tool, input))
    }

    pub fn asks(&self, tool: &str, input: &serde_json::Value) -> bool {
        self.ask.iter().any(|r| self.any_match(r, tool, input))
    }

    pub fn allows(&self, tool: &str, input: &serde_json::Value) -> bool {
        if tool == "bash" {
            return self.allows_command(input);
        }
        self.allow.iter().any(|r| self.matches(r, tool, input))
    }

    /// Deny/ask semantics: for shell commands, any piece may match.
    fn any_match(&self, rule: &Rule, tool: &str, input: &serde_json::Value) -> bool {
        if rule.kind == Kind::Bash && tool == "bash" {
            let Some(cmd) = input.get("command").and_then(|v| v.as_str()) else {
                return rule.spec.is_none();
            };
            let Some(spec) = &rule.spec else { return true };
            return bash_pieces(cmd, true)
                .iter()
                .any(|piece| bash_spec_matches(spec, piece));
        }
        self.matches(rule, tool, input)
    }

    /// Allow semantics for bash: every piece must be covered by some allow
    /// rule, and nothing may be hidden behind an expansion.
    fn allows_command(&self, input: &serde_json::Value) -> bool {
        let Some(cmd) = input.get("command").and_then(|v| v.as_str()) else {
            return false;
        };
        let bash_rules: Vec<&Rule> = self.allow.iter().filter(|r| r.kind == Kind::Bash).collect();
        if bash_rules.is_empty() {
            return false;
        }
        // A bare `Bash` rule allows everything the deny/ask lists don't catch.
        if bash_rules.iter().any(|r| r.spec.is_none()) {
            return true;
        }
        if cmd.contains('$') || cmd.contains('`') || cmd.contains('(') || cmd.contains('>') {
            return false;
        }
        let pieces = bash_pieces(cmd, false);
        !pieces.is_empty()
            && pieces.iter().all(|piece| {
                bash_rules.iter().any(|r| {
                    r.spec
                        .as_deref()
                        .is_some_and(|s| bash_spec_matches(s, piece))
                })
            })
    }

    /// Single-rule match for everything except compound shell commands.
    fn matches(&self, rule: &Rule, tool: &str, input: &serde_json::Value) -> bool {
        match &rule.kind {
            Kind::Bash => {
                if tool != "bash" {
                    return false;
                }
                match (&rule.spec, input.get("command").and_then(|v| v.as_str())) {
                    (None, _) => true,
                    (Some(spec), Some(cmd)) => bash_spec_matches(spec, cmd.trim()),
                    (Some(_), None) => false,
                }
            }
            Kind::Read => {
                matches!(tool, "read" | "glob" | "grep") && self.path_matches(rule, input)
            }
            Kind::Edit => {
                crate::sandbox::is_file_write_tool(tool) && self.path_matches(rule, input)
            }
            Kind::WebFetch => {
                tool == "fetch"
                    && match &rule.spec {
                        None => true,
                        Some(spec) => domain_matches(spec, input),
                    }
            }
            Kind::WebSearch => tool == "web_search",
            Kind::Other(name) => {
                tool == name
                    || (tool.starts_with("mcp__") && crate::sandbox::mcp_rule_covers(name, tool))
            }
        }
    }

    fn path_matches(&self, rule: &Rule, input: &serde_json::Value) -> bool {
        let Some(spec) = &rule.spec else { return true };
        let target = input
            .get("path")
            .or_else(|| input.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        let root = self.root_dir();
        let Some(glob) = path_glob(spec, &root) else {
            return false;
        };
        let lexical = absolutize(target, &root);
        let mut candidates = vec![lexical.clone()];
        // Resolve symlinks so a link can't smuggle a protected file past a rule.
        if let Ok(real) = std::fs::canonicalize(&lexical) {
            if real != lexical {
                candidates.push(real);
            }
        }
        candidates.iter().any(|p| {
            glob.is_match(p)
                // `Read(./secrets)` also covers everything inside it.
                || p.ancestors().skip(1).any(|a| glob.is_match(a))
        })
    }

    fn root_dir(&self) -> PathBuf {
        self.root
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

// ── Shell command matching ─────────────────────────────────────────────────────

/// Split a shell command into the pieces a rule is matched against: at `&&`,
/// `||`, `;`, `|`, `&` and newlines outside quotes. With `inner`, command
/// substitutions (`$(…)`, backticks, subshell parentheses) also split, so a
/// deny rule sees what is inside them.
fn bash_pieces(command: &str, inner: bool) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut pieces = Vec::new();
    let mut cur = String::new();
    let (mut single, mut double) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            _ => {}
        }
        let quoted = single || double;
        let is_sep = match c {
            ';' | '\n' | '|' => !quoted || (inner && double),
            '&' => {
                let redirect =
                    i > 0 && matches!(chars[i - 1], '>' | '<') || chars.get(i + 1) == Some(&'>');
                !redirect && !quoted
            }
            '(' | ')' | '`' | '{' | '}' => inner && !single,
            _ => false,
        };
        let substitution = c == '$' && inner && !single && chars.get(i + 1) == Some(&'(');
        if is_sep || substitution {
            pieces.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
        i += 1;
    }
    pieces.push(cur);
    pieces
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Strip leading `VAR=value` assignments, so `X=1 rm -rf /` is seen as `rm …`.
fn strip_env_assignments(piece: &str) -> &str {
    let mut rest = piece.trim_start();
    loop {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        let is_assignment = word.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if is_assignment && word_end < rest.len() {
            rest = rest[word_end..].trim_start();
        } else {
            return rest;
        }
    }
}

/// Reduce the command word to its basename (`/bin/rm -rf x` → `rm -rf x`) and
/// drop backslash escapes in it (`r\m` → `rm`).
fn normalize_head(piece: &str) -> String {
    let piece = strip_env_assignments(piece);
    let (head, rest) = match piece.find(char::is_whitespace) {
        Some(i) => (&piece[..i], &piece[i..]),
        None => (piece, ""),
    };
    let head: String = head.chars().filter(|&c| c != '\\').collect();
    let base = Path::new(&head)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&head);
    format!("{base}{rest}")
}

fn bash_spec_matches(spec: &str, piece: &str) -> bool {
    let normalized = normalize_head(piece);
    one_bash_spec(spec, piece) || one_bash_spec(spec, &normalized)
}

fn one_bash_spec(spec: &str, piece: &str) -> bool {
    let piece = piece.trim();
    if let Some(prefix) = spec.strip_suffix(":*") {
        let prefix = prefix.trim_end();
        return piece == prefix
            || piece
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace));
    }
    if spec.contains('*') {
        let mut pattern = String::from("^");
        for (i, part) in spec.split('*').enumerate() {
            if i > 0 {
                pattern.push_str(".*");
            }
            pattern.push_str(&regex::escape(part));
        }
        pattern.push('$');
        return regex::Regex::new(&pattern).is_ok_and(|re| re.is_match(piece));
    }
    piece == spec.trim()
}

// ── Path and URL matching ──────────────────────────────────────────────────────

/// Translate a rule's path specifier into a glob over absolute paths.
///
/// `//p` is absolute, `~/p` is under the home directory, `/p` and `./p` (or
/// any pattern containing a slash) are relative to the project root, and a bare
/// pattern such as `*.env` matches at any depth.
fn path_glob(spec: &str, root: &Path) -> Option<globset::GlobMatcher> {
    let pattern = if let Some(abs) = spec.strip_prefix("//") {
        format!("/{abs}")
    } else if let Some(home) = spec.strip_prefix("~/") {
        format!("{}/{home}", dirs::home_dir()?.display())
    } else if let Some(rel) = spec.strip_prefix("./") {
        format!("{}/{rel}", root.display())
    } else if let Some(rel) = spec.strip_prefix('/') {
        format!("{}/{rel}", root.display())
    } else if spec.contains('/') {
        format!("{}/{spec}", root.display())
    } else {
        format!("**/{spec}")
    };
    GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

fn absolutize(path: &str, root: &Path) -> PathBuf {
    let path = match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    };
    let abs = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

fn domain_matches(spec: &str, input: &serde_json::Value) -> bool {
    let Some(domain) = spec.strip_prefix("domain:") else {
        return false;
    };
    let domain = domain.trim().trim_start_matches("*.").to_ascii_lowercase();
    let Some(url) = input.get("url").and_then(|v| v.as_str()) else {
        return false;
    };
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(allow: &[&str], ask: &[&str], deny: &[&str]) -> RuleSet {
        let own = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (set, problems) = RuleSet::from_strings(
            &own(allow),
            &own(ask),
            &own(deny),
            Some(PathBuf::from("/proj")),
        );
        assert!(problems.is_empty(), "{problems:?}");
        set
    }

    fn bash(cmd: &str) -> serde_json::Value {
        json!({ "command": cmd })
    }

    #[test]
    fn rules_parse_with_and_without_specifiers() {
        assert!(Rule::parse("Bash(npm run test:*)").is_ok());
        assert!(Rule::parse("mcp__github__*").is_ok());
        assert!(Rule::parse("Read").is_ok());
        assert!(Rule::parse("").is_err());
        assert!(Rule::parse("Bash(unclosed").is_err());
        assert!(Rule::parse("(x)").is_err());
        assert!(Rule::parse("spawn_agent(x)").is_err());
    }

    #[test]
    fn bash_prefix_rules_match_on_word_boundaries() {
        let r = set(&["Bash(npm run test:*)"], &[], &[]);
        assert!(r.allows("bash", &bash("npm run test")));
        assert!(r.allows("bash", &bash("npm run test --watch")));
        assert!(!r.allows("bash", &bash("npm run testing")));
        assert!(!r.allows("bash", &bash("npm run build")));
        assert!(!r.allows("write", &bash("npm run test")));
    }

    #[test]
    fn bash_exact_and_glob_rules() {
        let r = set(&["Bash(git status)", "Bash(git log * --oneline)"], &[], &[]);
        assert!(r.allows("bash", &bash("git status")));
        assert!(!r.allows("bash", &bash("git status -s")));
        assert!(r.allows("bash", &bash("git log -5 --oneline")));
    }

    #[test]
    fn allow_rules_must_cover_every_piece_of_a_compound_command() {
        let r = set(&["Bash(git status:*)"], &[], &[]);
        assert!(r.allows("bash", &bash("git status && git status -s")));
        assert!(!r.allows("bash", &bash("git status; rm -rf /")));
        assert!(!r.allows("bash", &bash("git status | sh")));
        assert!(!r.allows("bash", &bash("git status $(rm -rf /)")));
        assert!(!r.allows("bash", &bash("git status > out.txt")));
    }

    #[test]
    fn a_bare_bash_rule_allows_every_command() {
        let r = set(&["Bash"], &[], &[]);
        assert!(r.allows("bash", &bash("anything at all")));
    }

    #[test]
    fn deny_rules_see_every_piece_and_look_inside_expansions() {
        let r = set(&[], &[], &["Bash(rm:*)"]);
        assert!(r.denies("bash", &bash("rm -rf x")));
        assert!(r.denies("bash", &bash("echo hi; rm -rf x")));
        assert!(r.denies("bash", &bash("echo $(rm -rf x)")));
        assert!(r.denies("bash", &bash("echo `rm x`")));
        assert!(r.denies("bash", &bash("(rm x)")));
        assert!(r.denies("bash", &bash("FOO=1 rm x")));
        assert!(r.denies("bash", &bash("/bin/rm x")));
        assert!(r.denies("bash", &bash("r\\m x")));
        assert!(!r.denies("bash", &bash("echo 'rm x'")));
        assert!(!r.denies("bash", &bash("ls")));
    }

    #[test]
    fn ask_rules_match_like_deny_rules() {
        let r = set(&[], &["Bash(git push:*)"], &[]);
        assert!(r.asks("bash", &bash("git add . && git push origin main")));
        assert!(!r.asks("bash", &bash("git status")));
    }

    #[test]
    fn redirections_do_not_split_commands() {
        let r = set(&["Bash(make:*)"], &[], &[]);
        // `2>&1` is not a command separator; `>` still blocks allow rules.
        assert_eq!(bash_pieces("make 2>&1", false), ["make 2>&1"]);
        assert!(!r.allows("bash", &bash("make 2>&1")));
    }

    #[test]
    fn path_rules_resolve_against_the_project_root() {
        let r = set(
            &[],
            &[],
            &["Read(./.env)", "Read(**/*.pem)", "Edit(/secrets)"],
        );
        assert!(r.denies("read", &json!({"path": ".env"})));
        assert!(r.denies("read", &json!({"path": "/proj/.env"})));
        assert!(!r.denies("read", &json!({"path": "sub/.env"})));
        assert!(r.denies("grep", &json!({"path": "a/b/key.pem"})));
        assert!(r.denies("edit", &json!({"path": "secrets/token.txt"})));
        assert!(r.denies("write", &json!({"path": "/proj/secrets"})));
        assert!(!r.denies("read", &json!({"path": "src/main.rs"})));
        assert!(!r.denies("edit", &json!({"path": ".env"})));
    }

    #[test]
    fn the_read_tools_file_path_argument_is_covered_too() {
        let r = set(&[], &[], &["Read(./.env)"]);
        assert!(r.denies("read", &json!({"file_path": "/proj/.env"})));
        assert!(!r.denies("read", &json!({"file_path": "/proj/a.rs"})));
    }

    #[test]
    fn bare_path_patterns_match_at_any_depth() {
        let r = set(&[], &[], &["Read(.env*)"]);
        assert!(r.denies("read", &json!({"path": "a/b/.env.local"})));
        assert!(r.denies("read", &json!({"path": ".env"})));
    }

    #[test]
    fn dotdot_cannot_escape_a_path_rule() {
        let r = set(&[], &[], &["Read(./.env)"]);
        assert!(r.denies("read", &json!({"path": "src/../.env"})));
    }

    #[test]
    fn edit_rules_cover_all_file_writing_tools() {
        let r = set(&["Edit(src/**)"], &[], &[]);
        for tool in ["write", "edit", "multi_edit", "notebook_edit"] {
            assert!(r.allows(tool, &json!({"path": "src/a/b.rs"})), "{tool}");
        }
        assert!(!r.allows("edit", &json!({"path": "build.rs"})));
        assert!(!r.allows("read", &json!({"path": "src/a.rs"})));
    }

    #[test]
    fn webfetch_domain_rules_match_subdomains_only_on_a_dot_boundary() {
        let r = set(&["WebFetch(domain:example.com)"], &[], &[]);
        for ok in [
            "https://example.com/x",
            "http://docs.example.com",
            "https://example.com:8080/a?b#c",
        ] {
            assert!(r.allows("fetch", &json!({"url": ok})), "{ok}");
        }
        for bad in [
            "https://evilexample.com",
            "https://example.com.evil.io",
            "https://evil.io/?u=example.com",
            "https://example.com@evil.io/",
        ] {
            assert!(!r.allows("fetch", &json!({"url": bad})), "{bad}");
        }
        assert!(!r.allows("web_search", &json!({"query": "x"})));
    }

    #[test]
    fn mcp_and_plain_tool_rules() {
        let r = set(&["mcp__github", "spawn_agent"], &[], &[]);
        assert!(r.allows("mcp__github__search", &json!({})));
        assert!(!r.allows("mcp__gitlab__search", &json!({})));
        assert!(r.allows("spawn_agent", &json!({})));
        assert!(set(&["WebSearch"], &[], &[]).allows("web_search", &json!({})));
    }

    #[test]
    fn invalid_rules_are_reported_not_applied() {
        let (set, problems) = RuleSet::from_strings(&["Bash(oops".to_string()], &[], &[], None);
        assert_eq!(problems.len(), 1);
        assert!(set.is_empty());
    }
}
