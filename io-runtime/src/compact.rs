use std::sync::Arc;
use tokio::sync::Mutex;

use crate::memory::SessionStore;
use crate::provider::{CompletionModel, CompletionRequest, ContentBlock, Message, Role};
use crate::types::Session;

pub struct CompactResult {
    pub turns_compacted: usize,
    pub summary: String,
}

pub async fn run(
    session: &Arc<Mutex<Session>>,
    provider: &Arc<dyn CompletionModel>,
    memory: &Arc<SessionStore>,
) -> anyhow::Result<CompactResult> {
    let turns = {
        let s = session.lock().await;
        s.turns.clone()
    };

    if turns.is_empty() {
        return Ok(CompactResult {
            turns_compacted: 0,
            summary: String::new(),
        });
    }

    let mut history = String::new();
    for (i, turn) in turns.iter().enumerate() {
        history.push_str(&format!("=== Turn {} ===\n", i + 1));
        history.push_str(&format!("User: {}\n", turn.user_message));
        if let Some(ref reply) = turn.assistant_message {
            history.push_str(&format!("Assistant: {}\n", reply));
        }
        if !turn.tool_calls.is_empty() {
            history.push_str("Tools:\n");
            for tc in &turn.tool_calls {
                let status = if tc.success { "ok" } else { "err" };
                let args = serde_json::to_string(&tc.input).unwrap_or_default();
                history.push_str(&format!("  [{}] {}({})\n", status, tc.tool_name, args));
                if !tc.output.is_empty() {
                    // Truncate very long outputs so the summarization prompt stays reasonable.
                    let out = if tc.output.len() > 500 {
                        format!("{}… (truncated)", &tc.output[..500])
                    } else {
                        tc.output.clone()
                    };
                    history.push_str(&format!("  => {}\n", out));
                }
            }
        }
        history.push('\n');
    }

    let prompt = format!(
        "You are summarizing a coding session between a user and an AI assistant.\n\
        Produce a dense, structured summary that preserves ALL context a reader \
        would need to continue this work without having seen the original conversation.\n\n\
        Include:\n\
        - The overall goal / task being worked on\n\
        - Every file read, created, or modified (with key details)\n\
        - Decisions made and the reasoning behind them\n\
        - Any errors encountered and how they were resolved\n\
        - Current state: what is done and what still needs to be done\n\
        - Any important technical details (function names, types, config values, etc.)\n\n\
        Be comprehensive. Omit nothing important. Use concise prose or bullet points.\n\n\
        --- CONVERSATION ---\n{history}\n--- END ---\n\nSummary:"
    );

    let request = CompletionRequest {
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: prompt }],
        }],
        tools: vec![],
        system_prompt: None,
        max_tokens: Some(4096),
        temperature: None,
        stream: false,
    };

    let response = provider
        .complete(request)
        .await
        .map_err(|e| anyhow::anyhow!("compact summarization failed: {e}"))?;

    let summary = response
        .content
        .iter()
        .filter_map(|b| {
            if let ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if summary.trim().is_empty() {
        anyhow::bail!("model returned an empty summary — compact aborted");
    }

    let turns_compacted = turns.len();

    let session_to_save = {
        let mut s = session.lock().await;
        s.summary = Some(summary.clone());
        s.turns.clear();
        s.updated_at = chrono::Utc::now();
        s.clone()
    };

    if let Err(e) = memory.save_session(&session_to_save).await {
        tracing::warn!("failed to save compacted session: {e}");
    }

    Ok(CompactResult {
        turns_compacted,
        summary,
    })
}
