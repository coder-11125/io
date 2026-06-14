/// A turn was aborted by the shared cancellation flag (e.g. the user
/// pressed Esc). Callers detect it with `err.is::<Cancelled>()`.
#[derive(Debug, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// User's answer to a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionReply {
    /// Run this tool call only.
    AllowOnce,
    /// Run this tool call and stop asking for this tool for the session.
    AllowSession,
    /// Do not run this tool call.
    Deny,
}

/// Events emitted by `run_turn_streaming`.
pub enum AgentEvent {
    /// Incremental text delta from the model.
    Text(String),
    /// A reasoning/thinking token delta from models that support extended thinking.
    Thinking(String),
    /// A tool call is about to execute.
    ToolStart {
        name: String,
        input: serde_json::Value,
    },
    /// A tool call finished.
    ToolDone {
        name: String,
        output: String,
        success: bool,
    },
    /// The agent is waiting for the user to approve a tool call.
    /// Send the answer through `respond`; dropping it counts as a denial.
    PermissionRequest {
        name: String,
        input: serde_json::Value,
        respond: tokio::sync::oneshot::Sender<PermissionReply>,
    },
    /// Token usage after a completed turn. `input_tokens` reflects the full
    /// conversation context sent to the model (grows each turn as history grows).
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// Emitted when the session was automatically compacted after a turn.
    AutoCompact { turns_compacted: usize },
}

/// Blocking permission prompt used by the non-streaming `run_turn` path
/// (e.g. single-shot mode, where there is no event channel to ask through).
pub type PromptFn = Arc<dyn Fn(&str, &serde_json::Value) -> PermissionReply + Send + Sync>;

use crate::memory::SessionStore;
use crate::provider::{CompletionModel, CompletionRequest, ContentBlock, StreamEvent};
use crate::provider::{Message, Role};
use crate::sandbox::{PermissionChecker, PermissionLevel};
use crate::tools::{ToolInput, ToolRegistry};
use crate::types::{Session, SessionId, ToolCallRecord, Turn, TurnUsage};
use std::sync::Arc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use tokio::sync::Mutex as TokioMutex;

pub struct Agent {
    provider: Arc<dyn CompletionModel>,
    tools: Arc<ToolRegistry>,
    session: Arc<TokioMutex<Session>>,
    memory: Arc<SessionStore>,
    permissions: Arc<PermissionChecker>,
    system_prompt: String,
    project_context: Option<String>,
    max_tokens: u32,
    auto_compact: bool,
    cancel: Mutex<Arc<AtomicBool>>,
    prompt_fn: Mutex<Option<PromptFn>>,
    pub model_id: String,
    pub provider_id: &'static str,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn CompletionModel>,
        tools: ToolRegistry,
        memory: SessionStore,
        permissions: Arc<PermissionChecker>,
        system_prompt: String,
        project_context: Option<String>,
        session_id: Option<SessionId>,
        model_id: String,
        max_tokens: u32,
        auto_compact: bool,
    ) -> Self {
        let provider_id = provider.provider_name();

        let mut session = if let Some(sid) = session_id {
            memory
                .load_session(sid)
                .unwrap_or_else(|_| Session::new(model_id.clone(), provider_id.to_string()))
        } else {
            Session::new(model_id.clone(), provider_id.to_string())
        };
        // Keep metadata current when resuming under a different provider/model
        // (e.g. after /model or /connect mid-session).
        session.metadata.model = model_id.clone();
        session.metadata.provider = provider_id.to_string();

        Self {
            provider,
            tools: Arc::new(tools),
            session: Arc::new(TokioMutex::new(session)),
            memory: Arc::new(memory),
            permissions,
            system_prompt,
            project_context,
            max_tokens,
            auto_compact,
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            prompt_fn: Mutex::new(None),
            model_id,
            provider_id,
        }
    }

    /// Set a blocking permission prompt for the non-streaming `run_turn` path.
    /// Without one, tool calls that would prompt are denied.
    pub fn set_prompt_fn(&self, f: PromptFn) {
        *self.prompt_fn.lock().unwrap() = Some(f);
    }

    /// Set a shared cancellation flag. When true (e.g. user pressed Esc),
    /// the agent loop and any spawned sub-agents abort early.
    pub fn set_cancel(&self, cancel: Arc<AtomicBool>) {
        *self.cancel.lock().unwrap() = cancel;
    }

    pub async fn session_id(&self) -> SessionId {
        self.session.lock().await.id
    }

    pub fn context_window(&self) -> u64 {
        self.provider.context_window()
    }

    /// Execute a single conversation turn without streaming. Permission
    /// prompts go through the `set_prompt_fn` callback if one is set.
    pub async fn run_turn(&self, user_input: &str) -> anyhow::Result<String> {
        self.run_turn_inner(user_input, None).await
    }

    /// Like `run_turn` but streams events to `token_tx` as they arrive:
    /// text deltas, tool start/done, permission requests, and usage.
    /// Dropping `token_tx` signals the caller that output is complete.
    pub async fn run_turn_streaming(
        &self,
        user_input: &str,
        token_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<String> {
        self.run_turn_inner(user_input, Some(&token_tx)).await
    }

    /// Shared turn loop. `token_tx` selects the mode: `Some` streams the
    /// model response and emits UI events; `None` uses blocking completions.
    async fn run_turn_inner(
        &self,
        user_input: &str,
        token_tx: Option<&tokio::sync::mpsc::Sender<AgentEvent>>,
    ) -> anyhow::Result<String> {
        // Hold the lock only long enough to snapshot history; release before any async I/O.
        let (prior_turns, summary, tool_specs) = {
            let session = self.session.lock().await;
            (
                session.turns.clone(),
                session.summary.clone(),
                self.tools.specs(),
            )
        };

        let system_text = match summary {
            Some(ref s) => format!(
                "{}\n\n## Prior Conversation Summary\n\n{}",
                self.system_prompt, s
            ),
            None => self.system_prompt.clone(),
        };

        let mut messages = vec![Message {
            role: Role::System,
            content: vec![ContentBlock::Text { text: system_text }],
        }];

        // Inject project context (AGENTS.md / CLAUDE.md) as a synthetic
        // user/assistant exchange so it occupies a real context slot and is
        // visible alongside the conversation history rather than buried in
        // the system prompt.
        if let Some(ref ctx) = self.project_context {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("<project-context>\n{ctx}\n</project-context>"),
                }],
            });
            messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "Understood. I'll use this project context to guide my work.".to_string(),
                }],
            });
        }

        // Replay policy: prior turns are replayed as user/assistant text only.
        // Tool calls and results are deliberately dropped from history — they
        // are token-heavy, and providers reject tool blocks whose IDs don't
        // match a real in-flight call. Within a single turn the model sees
        // full tool traffic; across turns it relies on its own prose summary.
        for turn in &prior_turns {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: turn.user_message.clone(),
                }],
            });
            if let Some(ref reply) = turn.assistant_message {
                messages.push(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: reply.clone(),
                    }],
                });
            }
        }

        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: user_input.to_string(),
            }],
        });

        let mut all_text = String::new();
        let mut all_tool_calls: Vec<ToolCallRecord> = Vec::new();
        // Billing-accurate sums across loop iterations (each API call bills
        // its own input), vs. the last reported input which reflects the
        // current full context and drives the context bar and auto-compact.
        let mut summed_input_tokens = 0u32;
        let mut summed_output_tokens = 0u32;
        let mut last_input_tokens = 0u32;
        // A provider failure mid-turn. Set instead of returning immediately so
        // partial progress (text, executed tools) is still saved to the session.
        let mut turn_error: Option<anyhow::Error> = None;
        const MAX_ITERATIONS: usize = 20;
        // Track files written during this turn to detect write-then-execute sequences.
        let mut written_this_turn: Vec<String> = Vec::new();

        // Propagate cancellation to tools (e.g. spawn_agent)
        self.tools.set_cancel(self.cancel.lock().unwrap().clone());

        for _ in 0..MAX_ITERATIONS {
            if self.cancel.lock().unwrap().load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }

            let request = CompletionRequest {
                messages: messages.clone(),
                tools: tool_specs.clone(),
                system_prompt: None,
                max_tokens: Some(self.max_tokens),
                temperature: None,
                stream: token_tx.is_some(),
            };

            let (assistant_content, iter_text, iter_tool_uses) = match token_tx {
                Some(tx) => {
                    let mut rx = match self.provider.complete_stream(request).await {
                        Ok(rx) => rx,
                        Err(e) => {
                            turn_error = Some(e);
                            break;
                        }
                    };

                    let mut iter_text = String::new();
                    let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

                    while let Some(event) = rx.recv().await {
                        let ev = match event {
                            Ok(ev) => ev,
                            Err(e) => {
                                turn_error = Some(e);
                                break;
                            }
                        };
                        // Capture usage whenever present — may co-occur with
                        // stop_reason so extract it before the dispatch match.
                        if let Some(ref u) = ev.usage {
                            if u.input_tokens > 0 {
                                last_input_tokens = u.input_tokens;
                                summed_input_tokens += u.input_tokens;
                            }
                            if u.output_tokens > 0 {
                                summed_output_tokens += u.output_tokens;
                            }
                        }
                        match ev {
                            StreamEvent {
                                delta: Some(text), ..
                            } => {
                                let _ = tx.send(AgentEvent::Text(text.clone())).await;
                                iter_text.push_str(&text);
                            }
                            StreamEvent {
                                content_block: Some(ContentBlock::ToolUse { id, name, input }),
                                ..
                            } => {
                                tool_uses.push((id, name, input));
                            }
                            StreamEvent {
                                stop_reason: Some(_),
                                ..
                            } => break,
                            _ => {}
                        }
                    }

                    let mut content = Vec::new();
                    if !iter_text.is_empty() {
                        content.push(ContentBlock::Text {
                            text: iter_text.clone(),
                        });
                    }
                    for (id, name, input) in &tool_uses {
                        content.push(ContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    (content, iter_text, tool_uses)
                }
                None => {
                    let response = match self.provider.complete(request).await {
                        Ok(r) => r,
                        Err(e) => {
                            turn_error = Some(e);
                            break;
                        }
                    };

                    if let Some(ref u) = response.usage {
                        if u.input_tokens > 0 {
                            last_input_tokens = u.input_tokens;
                            summed_input_tokens += u.input_tokens;
                        }
                        summed_output_tokens += u.output_tokens;
                    }

                    let mut iter_text = String::new();
                    let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

                    for block in &response.content {
                        match block {
                            ContentBlock::Text { text } => {
                                if !iter_text.is_empty() && !text.is_empty() {
                                    iter_text.push('\n');
                                }
                                iter_text.push_str(text);
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                tool_uses.push((id.clone(), name.clone(), input.clone()));
                            }
                            _ => {}
                        }
                    }

                    (response.content, iter_text, tool_uses)
                }
            };

            if !iter_text.is_empty() {
                if !all_text.is_empty() {
                    all_text.push('\n');
                }
                all_text.push_str(&iter_text);
            }

            // Append the assistant's full response (including tool-use blocks) to history
            messages.push(Message {
                role: Role::Assistant,
                content: assistant_content,
            });

            // A stream that died mid-response: keep the partial text but do
            // not execute any tool calls parsed from a truncated stream.
            if turn_error.is_some() {
                break;
            }

            if iter_tool_uses.is_empty() {
                break;
            }

            // Execute each tool and collect results
            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for (id, name, input) in &iter_tool_uses {
                if let Some(tx) = token_tx {
                    let _ = tx
                        .send(AgentEvent::ToolStart {
                            name: name.clone(),
                            input: input.clone(),
                        })
                        .await;
                }

                let start = std::time::Instant::now();
                // Force a prompt if this bash command references a file written earlier this turn.
                let force_prompt = name == "bash"
                    && input
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(|cmd| written_this_turn.iter().any(|p| cmd.contains(p.as_str())))
                        .unwrap_or(false);
                let (output, success) = match self
                    .resolve_permission(name, input, token_tx, force_prompt)
                    .await
                {
                    Err(denial) => (denial, false),
                    Ok(()) => self.execute_tool(name, input).await,
                };
                if success && (name == "write" || name == "edit") {
                    if let Some(path) = input.get("path").and_then(|v| v.as_str()) {
                        written_this_turn.push(path.to_string());
                    }
                }

                if let Some(tx) = token_tx {
                    let _ = tx
                        .send(AgentEvent::ToolDone {
                            name: name.clone(),
                            output: output.clone(),
                            success,
                        })
                        .await;
                }

                let duration = start.elapsed().as_millis() as u64;
                let record = ToolCallRecord {
                    tool_name: name.clone(),
                    input: input.clone(),
                    output: output.clone(),
                    success,
                    duration_ms: duration,
                };
                append_audit_log(&record);
                all_tool_calls.push(record);
                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: output,
                    is_error: if success { None } else { Some(true) },
                });
            }

            // Feed results back into the conversation
            messages.push(Message {
                role: Role::User,
                content: tool_results,
            });
        }

        // A failure before anything was produced: fail the turn outright
        // rather than persisting an empty record.
        if all_text.is_empty() && all_tool_calls.is_empty() {
            if let Some(e) = turn_error {
                return Err(e);
            }
        }

        if let Some(tx) = token_tx {
            if last_input_tokens > 0 || summed_output_tokens > 0 {
                let _ = tx
                    .send(AgentEvent::Usage {
                        input_tokens: last_input_tokens,
                        output_tokens: summed_output_tokens,
                    })
                    .await;
            }
        }

        let usage = if summed_input_tokens > 0 || summed_output_tokens > 0 {
            Some(
                TurnUsage::new(summed_input_tokens, summed_output_tokens)
                    .with_cost(self.provider_id, &self.model_id),
            )
        } else {
            None
        };

        let turn = Turn {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user_message: user_input.to_string(),
            assistant_message: Some(all_text.clone()),
            tool_calls: all_tool_calls,
            usage,
        };

        // Lock briefly to commit the turn, then save outside the lock.
        let session_to_save = {
            let mut session = self.session.lock().await;
            session.add_turn(turn);
            session.clone()
        };

        if let Err(e) = self.memory.save_session(&session_to_save).await {
            tracing::warn!("failed to save session: {e}");
        }

        // Surface the provider failure now that partial progress is saved.
        if let Some(e) = turn_error {
            return Err(e);
        }

        if self.auto_compact && last_input_tokens > 0 {
            let threshold = (self.context_window() as f64 * 0.8) as u32;
            if last_input_tokens >= threshold {
                match crate::compact::run(&self.session, &self.provider, &self.memory).await {
                    Ok(r) if r.turns_compacted > 0 => match token_tx {
                        Some(tx) => {
                            let _ = tx
                                .send(AgentEvent::AutoCompact {
                                    turns_compacted: r.turns_compacted,
                                })
                                .await;
                        }
                        None => tracing::info!("auto-compacted {} turn(s)", r.turns_compacted),
                    },
                    Err(e) => tracing::warn!("auto-compact failed: {e}"),
                    _ => {}
                }
            }
        }

        Ok(all_text)
    }

    /// Resolve the permission decision for one tool call. `Prompt` decisions
    /// ask through the event channel when streaming, or the blocking
    /// `prompt_fn` otherwise; with no way to ask, the call is denied.
    /// Returns the denial message the model should see on `Err`.
    async fn resolve_permission(
        &self,
        name: &str,
        input: &serde_json::Value,
        token_tx: Option<&tokio::sync::mpsc::Sender<AgentEvent>>,
        force_prompt: bool,
    ) -> Result<(), String> {
        let decision = self.permissions.decide_tool(name, input);
        let effective = if force_prompt && decision == PermissionLevel::Allow {
            PermissionLevel::Prompt
        } else {
            decision
        };
        match effective {
            PermissionLevel::Allow => Ok(()),
            PermissionLevel::Deny => Err(format!("tool {name} not permitted")),
            PermissionLevel::Prompt => {
                let reply = if let Some(tx) = token_tx {
                    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                    let sent = tx
                        .send(AgentEvent::PermissionRequest {
                            name: name.to_string(),
                            input: input.clone(),
                            respond: reply_tx,
                        })
                        .await;
                    // A closed channel or dropped responder means there is
                    // no UI to ask — treat both as a denial.
                    match sent {
                        Ok(()) => reply_rx.await.unwrap_or(PermissionReply::Deny),
                        Err(_) => PermissionReply::Deny,
                    }
                } else {
                    let prompt_fn = self.prompt_fn.lock().unwrap().clone();
                    match prompt_fn {
                        Some(f) => f(name, input),
                        None => {
                            return Err(format!(
                                "tool {name} requires user permission but no prompt is available"
                            ))
                        }
                    }
                };
                match reply {
                    PermissionReply::AllowOnce => Ok(()),
                    PermissionReply::AllowSession => {
                        let command = if name == "bash" {
                            input.get("command").and_then(|v| v.as_str())
                        } else if name == "write" || name == "edit" {
                            // Per-path approval: "always" for write/edit approves only this path
                            input.get("path").and_then(|v| v.as_str())
                        } else if name == "spawn_agent" {
                            // Per-agent-id approval: "always" approves only this agent role
                            input.get("agent_id").and_then(|v| v.as_str())
                        } else {
                            None
                        };
                        self.permissions.allow_for_session(name, command);
                        Ok(())
                    }
                    PermissionReply::Deny => Err(format!("tool {name} denied by user")),
                }
            }
        }
    }

    async fn execute_tool(&self, name: &str, input: &serde_json::Value) -> (String, bool) {
        let tool_input = ToolInput {
            name: name.to_string(),
            args: input
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
        };
        match self
            .tools
            .get(name)
            .map(|tool| Box::pin(tool.execute(tool_input)))
        {
            Some(fut) => {
                let out = fut.await;
                (out.data, out.success)
            }
            None => (format!("unknown tool: {name}"), false),
        }
    }

    pub async fn compact(&self) -> anyhow::Result<crate::compact::CompactResult> {
        crate::compact::run(&self.session, &self.provider, &self.memory).await
    }
}

fn audit_log_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".io").join("audit.log"))
}

fn append_audit_log(record: &crate::types::ToolCallRecord) {
    let Some(path) = audit_log_path() else { return };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let entry = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "tool": record.tool_name,
        "success": record.success,
        "duration_ms": record.duration_ms,
    });
    let line = format!("{}\n", entry);
    let _ = std::io::Write::write_all(&mut file, line.as_bytes());
}
