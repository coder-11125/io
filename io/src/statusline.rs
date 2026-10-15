//! The user's status-line command (`statusLine` in settings.json): run before
//! each prompt, with a JSON description of the session on stdin; the first
//! line it prints replaces the `agent · model · provider` part of the status
//! row. A slow, failing or silent command just leaves the default row.

use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const TIMEOUT: Duration = Duration::from_secs(2);

pub struct Snapshot<'a> {
    pub session_id: &'a str,
    pub agent: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub mode: &'a str,
    pub cwd: &'a Path,
    pub project_dir: &'a Path,
    pub context_tokens: u32,
    pub context_window: u64,
}

pub fn input_json(s: &Snapshot) -> String {
    serde_json::json!({
        "session_id": s.session_id,
        "agent": s.agent,
        "model": { "id": s.model, "provider": s.provider },
        "mode": s.mode,
        "cwd": s.cwd,
        "workspace": { "project_dir": s.project_dir },
        "context": { "used_tokens": s.context_tokens, "window": s.context_window },
        "version": env!("CARGO_PKG_VERSION"),
    })
    .to_string()
}

/// The first non-blank line of `output`, made safe to print.
pub fn first_line(output: &str) -> String {
    output
        .lines()
        .map(io_tui::render::sanitize_status)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

/// Run `command` and return the status text, or an empty string on any
/// failure.
pub async fn run(command: &str, snapshot: &Snapshot<'_>, cwd: &Path) -> String {
    let attempt = async {
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut stdin = child.stdin.take()?;
        let payload = input_json(snapshot);
        // A command that never reads stdin must not block us.
        let _ = stdin.write_all(payload.as_bytes()).await;
        drop(stdin);
        let out = child.wait_with_output().await.ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    match tokio::time::timeout(TIMEOUT, attempt).await {
        Ok(Some(text)) => first_line(&text),
        _ => String::new(),
    }
}

/// Re-run the configured command (if any) and update the status row.
pub async fn refresh(root: &Path, snapshot: &Snapshot<'_>) {
    let command = io_runtime::settings::Settings::load(root).status_line;
    let text = match command {
        Some(cmd) => run(&cmd, snapshot, root).await,
        None => String::new(),
    };
    io_tui::render::set_custom_status(&text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap<'a>(cwd: &'a Path) -> Snapshot<'a> {
        Snapshot {
            session_id: "s1",
            agent: "build",
            provider: "anthropic",
            model: "m",
            mode: "plan",
            cwd,
            project_dir: cwd,
            context_tokens: 5,
            context_window: 100,
        }
    }

    #[test]
    fn the_command_sees_the_session_as_json() {
        let json: serde_json::Value =
            serde_json::from_str(&input_json(&snap(Path::new("/p")))).unwrap();
        assert_eq!(json["model"]["id"], "m");
        assert_eq!(json["mode"], "plan");
        assert_eq!(json["workspace"]["project_dir"], "/p");
        assert_eq!(json["context"]["used_tokens"], 5);
    }

    #[test]
    fn only_the_first_printable_line_is_used() {
        assert_eq!(
            first_line("\n  \nhello \x1b[31mred\x1b[0m\nsecond"),
            "hello red"
        );
        assert_eq!(first_line(""), "");
    }

    #[tokio::test]
    async fn a_command_can_read_the_json_and_print_a_line() {
        let cwd = std::env::temp_dir();
        let out = run(
            "python3 -c \"import json,sys; d=json.load(sys.stdin); print(d['agent'], d['model']['id'])\"",
            &snap(&cwd),
            &cwd,
        )
        .await;
        assert_eq!(out, "build m");
    }

    #[tokio::test]
    async fn failing_slow_and_silent_commands_give_nothing() {
        let cwd = std::env::temp_dir();
        assert_eq!(run("exit 3", &snap(&cwd), &cwd).await, "");
        assert_eq!(run("true", &snap(&cwd), &cwd).await, "");
        assert_eq!(run("nonexistent-cmd-xyz", &snap(&cwd), &cwd).await, "");
        let started = std::time::Instant::now();
        assert_eq!(run("sleep 30; echo late", &snap(&cwd), &cwd).await, "");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
