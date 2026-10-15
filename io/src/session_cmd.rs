//! Sessions from the user's side: listing, picking one to resume, short ids,
//! export file names, and replaying a resumed conversation into the scrollback.

use io_runtime::memory::{SessionStore, SessionSummary};
use io_runtime::types::{Session, SessionId, Turn};
use io_tui::picker;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Resolve a full session id or a unique prefix of one (as `io session list`
/// shows) against the stored sessions.
pub fn resolve_id(store: &SessionStore, text: &str) -> anyhow::Result<SessionId> {
    let text = text.trim();
    if let Ok(id) = SessionId::from_str(text) {
        return Ok(id);
    }
    if text.len() < 4 {
        anyhow::bail!("'{text}' is too short to identify a session (give at least 4 characters)");
    }
    let matches: Vec<SessionId> = store
        .list_sessions()?
        .into_iter()
        .map(|s| s.id)
        .filter(|id| id.to_string().starts_with(&text.to_lowercase()))
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => anyhow::bail!("no session starts with '{text}'"),
        _ => anyhow::bail!(
            "'{text}' matches {} sessions; give more characters",
            matches.len()
        ),
    }
}

/// Eight-character id for lists.
pub fn short_id(id: SessionId) -> String {
    id.to_string().chars().take(8).collect()
}

/// `2025-01-05 14:03` from an RFC 3339 timestamp.
pub fn short_time(rfc3339: &str) -> String {
    rfc3339
        .get(..16)
        .map(|t| t.replace('T', " "))
        .unwrap_or_else(|| rfc3339.to_string())
}

/// One line per session for `io session list`.
pub fn list_line(s: &SessionSummary) -> String {
    format!(
        "{}  {}  {}  {} turn{}  {}",
        short_id(s.id),
        short_time(&s.updated_at),
        s.title(),
        s.turns,
        if s.turns == 1 { "" } else { "s" },
        s.model.as_deref().unwrap_or("")
    )
    .trim_end()
    .to_string()
}

/// Let the user pick a stored session. `Ok(None)` if they back out or there
/// are none. The session `skip` (the current one) is left out.
pub fn pick_session(store: &SessionStore, skip: SessionId) -> anyhow::Result<Option<SessionId>> {
    let sessions: Vec<SessionSummary> = store
        .list_sessions()?
        .into_iter()
        .filter(|s| s.id != skip && s.turns > 0)
        .collect();
    if sessions.is_empty() {
        return Ok(None);
    }
    let labels: Vec<(String, String)> = sessions
        .iter()
        .map(|s| {
            (
                s.title(),
                format!(
                    "{} turn{} · {} · {}",
                    s.turns,
                    if s.turns == 1 { "" } else { "s" },
                    short_time(&s.updated_at),
                    short_id(s.id)
                ),
            )
        })
        .collect();
    let items: Vec<(&str, &str)> = labels
        .iter()
        .map(|(l, h)| (l.as_str(), h.as_str()))
        .collect();
    println!();
    match picker::pick_with_hint(&items, Some(0)) {
        Ok(i) => Ok(Some(sessions[i].id)),
        Err(e) if e.is::<picker::Dismissed>() => Ok(None),
        Err(e) => Err(e),
    }
}

/// Where `/export [file]` writes: the given path, else a file in `dir` named
/// after the session.
pub fn export_path(arg: &str, session: &Session, dir: &Path) -> PathBuf {
    let arg = arg.trim();
    if !arg.is_empty() {
        return dir.join(arg);
    }
    let stem = session
        .metadata
        .name
        .as_deref()
        .map(slug)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| short_id(session.id));
    dir.join(format!("io-session-{stem}.md"))
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(40).collect()
}

/// Every turn in full — prompts, each tool call with its (trimmed) result, and
/// replies — for the Ctrl+O transcript view.
pub fn transcript_lines(turns: &[Turn]) -> Vec<String> {
    const MAX_RESULT_LINES: usize = 25;
    let mut lines = Vec::new();
    if turns.is_empty() {
        lines.push("  (nothing has been said yet)".to_string());
    }
    for (i, turn) in turns.iter().enumerate() {
        lines.push(format!("── turn {} ─────────────────────────────", i + 1));
        for (n, l) in turn.user_message.lines().enumerate() {
            lines.push(if n == 0 {
                format!("> {l}")
            } else {
                format!("  {l}")
            });
        }
        for call in &turn.tool_calls {
            let mark = if call.success { "●" } else { "✗" };
            let args = call.input.to_string();
            let args: String = args.chars().take(200).collect();
            lines.push(format!("{mark} {} {args}", call.tool_name));
            let out: Vec<&str> = call.output.lines().collect();
            for l in out.iter().take(MAX_RESULT_LINES) {
                lines.push(format!("    {l}"));
            }
            if out.len() > MAX_RESULT_LINES {
                lines.push(format!(
                    "    … {} more line(s)",
                    out.len() - MAX_RESULT_LINES
                ));
            }
        }
        if let Some(reply) = &turn.assistant_message {
            lines.push(String::new());
            lines.extend(reply.trim().lines().map(|l| format!("  {l}")));
        }
        lines.push(String::new());
    }
    lines
}

/// Scrollback lines that show the last turns of a resumed conversation.
pub fn replay_lines(turns: &[Turn], max_turns: usize) -> Vec<String> {
    let start = turns.len().saturating_sub(max_turns);
    let mut lines = Vec::new();
    if start > 0 {
        lines.push(format!("  … {start} earlier turn(s) not shown"));
        lines.push(String::new());
    }
    for turn in &turns[start..] {
        for (i, l) in turn.user_message.lines().enumerate() {
            lines.push(if i == 0 {
                format!("  > {l}")
            } else {
                format!("    {l}")
            });
        }
        lines.push(String::new());
        if !turn.tool_calls.is_empty() {
            lines.push(format!("  ({} tool call(s))", turn.tool_calls.len()));
        }
        if let Some(reply) = &turn.assistant_message {
            for l in reply.trim().lines() {
                lines.push(format!("  {l}"));
            }
        }
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(user: &str, reply: &str, tools: usize) -> Turn {
        Turn {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user_message: user.into(),
            assistant_message: Some(reply.into()),
            tool_calls: (0..tools)
                .map(|_| io_runtime::types::ToolCallRecord {
                    tool_name: "bash".into(),
                    input: serde_json::json!({}),
                    output: String::new(),
                    success: true,
                    duration_ms: 0,
                    pre_edit_snapshot: None,
                })
                .collect(),
            usage: None,
        }
    }

    #[test]
    fn short_times_and_ids() {
        assert_eq!(
            short_time("2025-01-05T14:03:22.123+00:00"),
            "2025-01-05 14:03"
        );
        assert_eq!(short_time("x"), "x");
        let id = SessionId::new();
        assert_eq!(short_id(id).len(), 8);
        assert!(id.to_string().starts_with(&short_id(id)));
    }

    #[test]
    fn export_names_come_from_the_argument_or_the_session() {
        let mut s = Session::new("m".into(), "p".into());
        let dir = Path::new("/work");
        assert_eq!(
            export_path("out.md", &s, dir),
            PathBuf::from("/work/out.md")
        );
        assert_eq!(
            export_path("", &s, dir),
            PathBuf::from(format!("/work/io-session-{}.md", short_id(s.id)))
        );
        s.metadata.name = Some("Fix the Login!! bug".into());
        assert_eq!(
            export_path("  ", &s, dir),
            PathBuf::from("/work/io-session-fix-the-login-bug.md")
        );
        s.metadata.name = Some("!!!".into());
        assert!(export_path("", &s, dir)
            .to_string_lossy()
            .contains(&short_id(s.id)));
    }

    #[test]
    fn the_transcript_includes_tool_calls_and_trims_long_results() {
        let mut t = turn("do it\nplease", "all done", 1);
        t.tool_calls[0].output = (0..40)
            .map(|i| format!("row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        t.tool_calls[0].success = false;
        let lines = transcript_lines(&[t]);
        assert!(lines[0].contains("turn 1"));
        assert_eq!(lines[1], "> do it");
        assert_eq!(lines[2], "  please");
        assert!(lines.iter().any(|l| l.starts_with("✗ bash")));
        assert!(lines.contains(&"    row 24".to_string()));
        assert!(!lines.contains(&"    row 25".to_string()));
        assert!(lines.contains(&"    … 15 more line(s)".to_string()));
        assert!(lines.contains(&"  all done".to_string()));
        assert!(transcript_lines(&[])[0].contains("nothing has been said"));
    }

    #[test]
    fn replay_shows_the_tail_of_the_conversation() {
        let turns: Vec<Turn> = (1..=4)
            .map(|i| turn(&format!("q{i}"), &format!("a{i}\nmore"), i % 2))
            .collect();
        let lines = replay_lines(&turns, 2);
        assert_eq!(lines[0], "  … 2 earlier turn(s) not shown");
        assert!(lines.contains(&"  > q3".to_string()));
        assert!(lines.contains(&"  (1 tool call(s))".to_string()));
        assert!(lines.contains(&"  more".to_string()));
        assert!(!lines.contains(&"  > q2".to_string()));
        assert!(replay_lines(&turns, 10)[0].starts_with("  > q1"));
        assert!(replay_lines(&[], 3).is_empty());
    }

    #[tokio::test]
    async fn ids_resolve_by_unique_prefix() {
        let path = std::env::temp_dir().join(format!("io_sess_{}.db", uuid::Uuid::new_v4()));
        let store = SessionStore::with_path(path).unwrap();
        let a = Session::new("m".into(), "p".into());
        store.save_session(&a).await.unwrap();
        let prefix: String = a.id.to_string().chars().take(6).collect();
        assert_eq!(resolve_id(&store, &prefix).unwrap(), a.id);
        assert_eq!(resolve_id(&store, &a.id.to_string()).unwrap(), a.id);
        assert!(resolve_id(&store, "zzzz").is_err());
        assert!(resolve_id(&store, "ab").is_err());
    }
}
