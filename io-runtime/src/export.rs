//! Plain-text exports of a session.

use crate::types::Session;

/// Render `session` as Markdown: the compaction summary (if any), then each
/// turn's prompt, the tools it called, and the reply.
pub fn to_markdown(session: &Session) -> String {
    let title = session
        .metadata
        .name
        .clone()
        .unwrap_or_else(|| format!("Session {}", session.id));
    let mut out = format!("# {title}\n\n");
    out.push_str(&format!(
        "- Model: {} ({})\n- Started: {}\n- Updated: {}\n- Turns: {}\n\n",
        session.metadata.model,
        session.metadata.provider,
        session.created_at.format("%Y-%m-%d %H:%M UTC"),
        session.updated_at.format("%Y-%m-%d %H:%M UTC"),
        session.turns.len()
    ));
    if let Some(summary) = &session.summary {
        out.push_str("## Summary of earlier conversation\n\n");
        out.push_str(summary.trim());
        out.push_str("\n\n");
    }
    for (i, turn) in session.turns.iter().enumerate() {
        out.push_str(&format!("## Turn {}\n\n", i + 1));
        out.push_str("**You:**\n\n");
        out.push_str(&quote(&turn.user_message));
        out.push_str("\n\n");
        if !turn.tool_calls.is_empty() {
            out.push_str("**Tools:**\n\n");
            for call in &turn.tool_calls {
                let status = if call.success { "ok" } else { "failed" };
                out.push_str(&format!(
                    "- `{}` {} — {}\n",
                    call.tool_name,
                    one_line(&call.input.to_string(), 120),
                    status
                ));
            }
            out.push('\n');
        }
        if let Some(reply) = &turn.assistant_message {
            out.push_str("**Assistant:**\n\n");
            out.push_str(reply.trim());
            out.push_str("\n\n");
        }
    }
    out
}

fn quote(text: &str) -> String {
    text.trim()
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut s: String = flat.chars().take(max).collect();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ToolCallRecord, Turn};

    #[test]
    fn exports_turns_tools_and_summary() {
        let mut s = Session::new("m".into(), "p".into());
        s.metadata.name = Some("Demo".into());
        s.summary = Some("earlier stuff".into());
        s.add_turn(Turn {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user_message: "fix it\nplease".into(),
            assistant_message: Some("done".into()),
            tool_calls: vec![ToolCallRecord {
                tool_name: "edit".into(),
                input: serde_json::json!({"path": "a.rs"}),
                output: "ok".into(),
                success: false,
                duration_ms: 1,
                pre_edit_snapshot: None,
            }],
            usage: None,
        });
        let md = to_markdown(&s);
        assert!(md.starts_with("# Demo\n"));
        assert!(md.contains("## Summary of earlier conversation\n\nearlier stuff"));
        assert!(md.contains("> fix it\n> please"));
        assert!(md.contains("- `edit` {\"path\":\"a.rs\"} — failed"));
        assert!(md.contains("**Assistant:**\n\ndone"));
    }

    #[test]
    fn an_unnamed_empty_session_still_exports() {
        let md = to_markdown(&Session::new("m".into(), "p".into()));
        assert!(md.starts_with("# Session "));
        assert!(md.contains("Turns: 0"));
    }
}
