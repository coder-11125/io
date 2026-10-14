//! Headless mode: `io -p "prompt"` runs one turn without a TUI and prints the
//! result, for scripts and pipelines. The prompt can come from the argument,
//! from piped stdin, or both (stdin first, then the argument).
//!
//! Nothing can answer a permission prompt here, so a call that would ask is
//! denied; pre-approve what the run needs with `--allowedTools`, settings.json
//! rules, or `--permission-mode`.

use crate::input::resolve_at_mentions_with_images;
use crate::tui::{build_agent, detect_project_root, SessionChoice};
use clap::ValueEnum;
use io_runtime::{Agent, AgentEvent, PermissionReply};
use std::io::{IsTerminal, Read};

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum OutputFormat {
    /// Just the final reply
    Text,
    /// One JSON object with the reply, session id, usage and cost
    Json,
    /// One JSON object per line as events happen, ending with the result
    StreamJson,
}

pub struct Args {
    pub prompt: Option<String>,
    pub format: OutputFormat,
    pub model: Option<String>,
    pub continue_session: bool,
    pub resume: Option<String>,
}

/// The prompt to run: piped input first, then the argument.
pub fn compose_prompt(piped: Option<&str>, arg: Option<&str>) -> Option<String> {
    let parts: Vec<&str> = [piped, arg]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// How long a piped stdin may stay silent before `io -p "prompt"` decides
/// nothing is being piped (cron and CI often leave an open, empty pipe).
const STDIN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Piped input, if any. With a prompt argument the first byte must arrive
/// within [`STDIN_GRACE`]; without one there is nothing else to run, so wait.
fn read_piped_stdin(have_arg: bool) -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel::<Option<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        let mut stdin = std::io::stdin().lock();
        loop {
            match stdin.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
                Ok(n) => {
                    if tx.send(Some(chunk[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let first = if have_arg {
        rx.recv_timeout(STDIN_GRACE).ok()?
    } else {
        rx.recv().ok()?
    };
    let mut bytes = first?;
    while let Ok(Some(more)) = rx.recv() {
        bytes.extend(more);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// How a finished turn is summarised in `json` / `stream-json` output.
pub async fn result_json(
    agent: &Agent,
    reply: Result<&str, &anyhow::Error>,
    elapsed: std::time::Duration,
) -> serde_json::Value {
    let session_id = agent.session_id().await.to_string();
    let turns = agent.session_turns().await;
    let last = turns.last();
    let usage = last.and_then(|t| t.usage.clone());
    let (subtype, is_error, text) = match reply {
        Ok(text) if agent.hit_iteration_limit() => ("error_max_turns", true, text.to_string()),
        Ok(text) => ("success", false, text.to_string()),
        Err(e) => ("error_during_execution", true, format!("{e:#}")),
    };
    serde_json::json!({
        "type": "result",
        "subtype": subtype,
        "is_error": is_error,
        "result": text,
        "session_id": session_id,
        "duration_ms": elapsed.as_millis() as u64,
        "num_tool_calls": last.map(|t| t.tool_calls.len()).unwrap_or(0),
        "total_cost_usd": usage.as_ref().and_then(|u| u.cost),
        "usage": {
            "input_tokens": usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
            "output_tokens": usage.as_ref().map(|u| u.output_tokens).unwrap_or(0),
            "cache_read_tokens": usage.as_ref().map(|u| u.cache_read_tokens).unwrap_or(0),
            "cache_write_tokens": usage.as_ref().map(|u| u.cache_write_tokens).unwrap_or(0),
        },
    })
}

/// The `stream-json` line for one agent event (`None` for events with no
/// place in the stream).
pub fn event_json(event: &AgentEvent) -> Option<serde_json::Value> {
    Some(match event {
        AgentEvent::Text(text) => serde_json::json!({"type": "text", "text": text}),
        AgentEvent::Thinking(text) => serde_json::json!({"type": "thinking", "text": text}),
        AgentEvent::ToolStart { name, input } => {
            serde_json::json!({"type": "tool_use", "name": name, "input": input})
        }
        AgentEvent::ToolDone {
            name,
            output,
            success,
        } => serde_json::json!({
            "type": "tool_result", "name": name, "output": output, "success": success
        }),
        AgentEvent::PermissionRequest { name, input, .. } => serde_json::json!({
            "type": "permission_denied", "name": name, "input": input,
            "reason": "headless runs cannot ask; allow it with --allowedTools"
        }),
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
        } => serde_json::json!({
            "type": "usage", "input_tokens": input_tokens, "output_tokens": output_tokens
        }),
        AgentEvent::AutoCompact { turns_compacted } => {
            serde_json::json!({"type": "auto_compact", "turns_compacted": turns_compacted})
        }
    })
}

/// Run the prompt; returns the process exit code.
pub async fn run(args: Args) -> anyhow::Result<i32> {
    let piped = read_piped_stdin(args.prompt.is_some());
    let Some(prompt) = compose_prompt(piped.as_deref(), args.prompt.as_deref()) else {
        anyhow::bail!("no prompt: pass one as an argument or pipe it in on stdin");
    };

    let config = io_runtime::config::Config::load()?;
    let keys = io_runtime::config::KeyStore::load();
    if let Some(env) = io_runtime::provider::missing_api_key(&config, &keys) {
        anyhow::bail!(
            "no API key found for provider \"{}\". Export {env}, or run `io` and use /connect.",
            config.provider.default
        );
    }
    for note in crate::tui::apply_startup_settings(&detect_project_root()) {
        eprintln!("{note}");
    }
    for note in crate::mcp_cmd::start(false, &detect_project_root()).await {
        eprintln!("{note}");
    }

    let session = match (&args.resume, args.continue_session) {
        (Some(id), _) => SessionChoice::Existing(crate::session_cmd::resolve_id(
            &io_runtime::memory::SessionStore::new()?,
            id,
        )?),
        (None, true) => SessionChoice::Continue,
        (None, false) => SessionChoice::New,
    };
    let build = io_agents::builtin::by_id("build").expect("build agent must exist");
    let agent = build_agent(session, &build, args.model.as_deref()).await?;
    let (prompt, images) = resolve_at_mentions_with_images(&prompt);

    let started = std::time::Instant::now();
    let outcome = if args.format == OutputFormat::StreamJson {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
        println!(
            "{}",
            serde_json::json!({
                "type": "system", "subtype": "init",
                "session_id": agent.session_id().await.to_string(),
                "model": agent.model_id, "provider": agent.provider_id,
            })
        );
        let printer = async {
            while let Some(event) = rx.recv().await {
                if let Some(line) = event_json(&event) {
                    println!("{line}");
                }
                if let AgentEvent::PermissionRequest { respond, .. } = event {
                    let _ = respond.send(PermissionReply::Deny);
                }
            }
        };
        let turn = agent.run_turn_streaming_with_images(&prompt, images, tx);
        let (result, ()) = tokio::join!(turn, printer);
        result
    } else {
        agent.run_turn_with_images(&prompt, images).await
    };
    let elapsed = started.elapsed();
    agent.end_session("headless").await;

    let code = match (&outcome, args.format) {
        (Ok(text), OutputFormat::Text) => {
            println!("{text}");
            if agent.hit_iteration_limit() {
                eprintln!("note: stopped at the --max-turns limit");
            }
            0
        }
        (Err(e), OutputFormat::Text) => {
            eprintln!("error: {e:#}");
            1
        }
        (reply, _) => {
            let summary = result_json(&agent, reply.as_ref().map(String::as_str), elapsed).await;
            println!("{summary}");
            i32::from(reply.is_err())
        }
    };
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piped_input_comes_before_the_argument() {
        assert_eq!(
            compose_prompt(Some("log lines\n"), Some("explain")).as_deref(),
            Some("log lines\n\nexplain")
        );
        assert_eq!(compose_prompt(None, Some("hi")).as_deref(), Some("hi"));
        assert_eq!(
            compose_prompt(Some("only stdin"), None).as_deref(),
            Some("only stdin")
        );
        assert_eq!(compose_prompt(Some("  \n"), Some("")), None);
        assert_eq!(compose_prompt(None, None), None);
    }

    #[test]
    fn events_become_json_lines() {
        let line = event_json(&AgentEvent::ToolStart {
            name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        })
        .unwrap();
        assert_eq!(line["type"], "tool_use");
        assert_eq!(line["input"]["command"], "ls");
        let done = event_json(&AgentEvent::ToolDone {
            name: "bash".into(),
            output: "x".into(),
            success: false,
        })
        .unwrap();
        assert_eq!(
            (done["type"].as_str(), done["success"].as_bool()),
            (Some("tool_result"), Some(false))
        );
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let denied = event_json(&AgentEvent::PermissionRequest {
            name: "write".into(),
            input: serde_json::json!({}),
            respond: tx,
        })
        .unwrap();
        assert_eq!(denied["type"], "permission_denied");
    }
}
