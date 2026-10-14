//! `io doctor` and `/doctor`: check that the installation, configuration and
//! environment can do what the user is likely to ask of them.

use io_runtime::settings::Settings;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub level: Level,
    pub name: &'static str,
    pub detail: String,
}

impl Check {
    fn new(level: Level, name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level,
            name,
            detail: detail.into(),
        }
    }
}

fn on_path(program: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|p| p.is_file())
    })
}

pub fn run_checks(root: &Path) -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(Check::new(
        Level::Ok,
        "version",
        format!(
            "io {} ({} {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    ));

    match io_runtime::config::Config::load() {
        Ok(config) => {
            checks.push(Check::new(Level::Ok, "config", "~/.io/config.toml loads"));
            let keys = io_runtime::config::KeyStore::load();
            match io_runtime::provider::missing_api_key(&config, &keys) {
                None => checks.push(Check::new(
                    Level::Ok,
                    "provider",
                    format!("{} is configured", config.provider.default),
                )),
                Some(env) => checks.push(Check::new(
                    Level::Fail,
                    "provider",
                    format!(
                        "no API key for {} — export {env}, or run /connect",
                        config.provider.default
                    ),
                )),
            }
        }
        Err(e) => checks.push(Check::new(Level::Fail, "config", format!("{e:#}"))),
    }

    match io_runtime::memory::SessionStore::new().and_then(|s| s.list_sessions()) {
        Ok(list) => checks.push(Check::new(
            Level::Ok,
            "sessions",
            format!("database opens ({} recent session(s))", list.len()),
        )),
        Err(e) => checks.push(Check::new(Level::Fail, "sessions", format!("{e:#}"))),
    }

    let settings = Settings::load(root);
    if settings.problems.is_empty() {
        let files: Vec<String> = settings
            .files
            .iter()
            .map(|(scope, _)| scope.label().to_string())
            .collect();
        checks.push(Check::new(
            Level::Ok,
            "settings",
            if files.is_empty() {
                "no settings.json files (defaults)".to_string()
            } else {
                format!("loaded {}", files.join(", "))
            },
        ));
    } else {
        for problem in &settings.problems {
            checks.push(Check::new(Level::Warn, "settings", problem.clone()));
        }
    }

    let (entries, problems) = io_runtime::mcp::collect_entries(root, Vec::new());
    if problems.is_empty() {
        checks.push(Check::new(
            Level::Ok,
            "mcp",
            format!("{} server(s) configured", entries.len()),
        ));
    } else {
        for problem in problems {
            checks.push(Check::new(Level::Warn, "mcp", problem));
        }
    }

    if settings.sandbox.enabled {
        match io_runtime::os_sandbox::find_backend() {
            Ok(b) => checks.push(Check::new(Level::Ok, "sandbox", format!("enabled ({b:?})"))),
            Err(e) => checks.push(Check::new(
                Level::Fail,
                "sandbox",
                format!("enabled but unusable — bash will refuse to run: {e}"),
            )),
        }
    } else {
        checks.push(Check::new(
            Level::Ok,
            "sandbox",
            "off (turn on with {\"sandbox\": {\"enabled\": true}} in settings.json)",
        ));
    }

    match on_path("git") {
        Some(_) if root.join(".git").exists() => checks.push(Check::new(
            Level::Ok,
            "git",
            "available, inside a repository",
        )),
        Some(_) => checks.push(Check::new(
            Level::Ok,
            "git",
            "available (this directory is not a repository)",
        )),
        None => checks.push(Check::new(
            Level::Warn,
            "git",
            "not found — /review, /commit and --worktree need it",
        )),
    }

    let editor = std::env::var("VISUAL")
        .ok()
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|e| !e.trim().is_empty());
    checks.push(match editor {
        Some(e) => Check::new(Level::Ok, "editor", format!("{e} (for /memory)")),
        None => Check::new(
            Level::Warn,
            "editor",
            "VISUAL / EDITOR not set; /memory will use vi",
        ),
    });

    let clipboard = ["pngpaste", "wl-paste", "xclip"]
        .into_iter()
        .find(|c| on_path(c).is_some());
    checks.push(match clipboard {
        Some(c) => Check::new(
            Level::Ok,
            "clipboard",
            format!("{c} (Ctrl+V pastes images)"),
        ),
        None => Check::new(
            Level::Warn,
            "clipboard",
            "no pngpaste, wl-paste or xclip — Ctrl+V can't paste images",
        ),
    });

    checks
}

pub fn render(checks: &[Check]) -> Vec<String> {
    checks
        .iter()
        .map(|c| {
            let mark = match c.level {
                Level::Ok => "✓",
                Level::Warn => "!",
                Level::Fail => "✗",
            };
            format!("{mark} {:<10} {}", c.name, c.detail)
        })
        .collect()
}

pub fn failures(checks: &[Check]) -> usize {
    checks.iter().filter(|c| c.level == Level::Fail).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_render_with_a_mark_per_level() {
        let checks = vec![
            Check::new(Level::Ok, "a", "fine"),
            Check::new(Level::Warn, "b", "hmm"),
            Check::new(Level::Fail, "c", "broken"),
        ];
        let lines = render(&checks);
        assert_eq!(lines[0], "✓ a          fine");
        assert!(lines[1].starts_with("! b"));
        assert!(lines[2].starts_with("✗ c"));
        assert_eq!(failures(&checks), 1);
    }

    #[test]
    fn a_real_run_reports_the_basics() {
        let checks = run_checks(&std::env::temp_dir());
        for name in [
            "version",
            "settings",
            "mcp",
            "sandbox",
            "git",
            "editor",
            "clipboard",
        ] {
            assert!(checks.iter().any(|c| c.name == name), "missing {name}");
        }
    }
}
