/// Events emitted by `run_turn_streaming`.
pub enum AgentEvent {
    /// Incremental text delta from the model.
    Text(String),
    /// A reasoning/thinking token delta from models that support extended thinking.
    Thinking(String),
    /// A tool call is about to execute.
    ToolStart { name: String, input: serde_json::Value },
    /// A tool call finished.
    ToolDone { name: String, output: String, success: bool },
    /// Token usage after a completed turn. `input_tokens` reflects the full
    /// conversation context sent to the model (grows each turn as history grows).
    Usage { input_tokens: u32, output_tokens: u32 },
    /// Emitted when the session was automatically compacted after a turn.
    AutoCompact { turns_compacted: usize },
}

use std::sync::Arc;
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
use tokio::sync::Mutex as TokioMutex;
use crate::provider::{CompletionModel, CompletionRequest, ContentBlock, ProviderKind, StreamEvent};
use crate::tools::{ToolRegistry, ToolInput};
use crate::memory::SessionStore;
use crate::sandbox::PermissionChecker;
use crate::types::{Session, SessionId, Turn, ToolCallRecord, TurnUsage};
use crate::provider::{Message, Role};

pub struct Agent {
    provider: Arc<ProviderKind>,
    tools: Arc<ToolRegistry>,
    session: Arc<TokioMutex<Session>>,
    memory: Arc<SessionStore>,
    permissions: Arc<PermissionChecker>,
    system_prompt: String,
    max_tokens: u32,
    auto_compact: bool,
    cancel: Mutex<Arc<AtomicBool>>,
    pub model_id: String,
    pub provider_id: &'static str,
}

impl Agent {
    pub fn new(
        provider: Arc<ProviderKind>,
        tools: ToolRegistry,
        memory: SessionStore,
        permissions: PermissionChecker,
        system_prompt: String,
        session_id: Option<SessionId>,
        model_id: String,
        max_tokens: u32,
        auto_compact: bool,
    ) -> Self {
        let provider_id = provider.name();

        let session = if let Some(sid) = session_id {
            memory.load_session(sid).unwrap_or_else(|_| {
                Session::new(model_id.clone(), provider_id.to_string())
            })
        } else {
            Session::new(model_id.clone(), provider_id.to_string())
        };

        Self {
            provider,
            tools: Arc::new(tools),
            session: Arc::new(TokioMutex::new(session)),
            memory: Arc::new(memory),
            permissions: Arc::new(permissions),
            system_prompt,
            max_tokens,
            auto_compact,
            cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            model_id,
            provider_id,
        }
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
        use crate::provider::CompletionModel;
        self.provider.context_window()
    }

    pub async fn run_turn(&self, user_input: &str) -> anyhow::Result<String> {
        // Hold the lock only long enough to snapshot history; release before any async I/O.
        let (prior_turns, summary, tool_specs) = {
            let session = self.session.lock().await;
            (session.turns.clone(), session.summary.clone(), self.tools.specs())
        };

        let system_text = match summary {
            Some(ref s) => format!("{}\n\n## Prior Conversation Summary\n\n{}", self.system_prompt, s),
            None => self.system_prompt.clone(),
        };

        let mut messages = vec![
            Message {
                role: Role::System,
                content: vec![ContentBlock::Text { text: system_text }],
            }
        ];

        for turn in &prior_turns {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: turn.user_message.clone() }],
            });
            if let Some(ref reply) = turn.assistant_message {
                messages.push(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text { text: reply.clone() }],
                });
            }
        }

        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: user_input.to_string() }],
        });

        let mut all_assistant_text = String::new();
        let mut all_tool_calls: Vec<ToolCallRecord> = Vec::new();
        let mut total_input_tokens = 0u32;
        let mut total_output_tokens = 0u32;
        const MAX_ITERATIONS: usize = 20;

        for _ in 0..MAX_ITERATIONS {
            if self.cancel.lock().unwrap().load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!("cancelled"));
            }

            let request = CompletionRequest {
                messages: messages.clone(),
                tools: tool_specs.clone(),
                system_prompt: None,
                max_tokens: Some(self.max_tokens),
                temperature: None,
                stream: false,
            };

            let response = self.provider.complete(request).await?;

            if let Some(ref u) = response.usage {
                total_input_tokens += u.input_tokens;
                total_output_tokens += u.output_tokens;
            }

            let mut turn_tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

            for block in &response.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !all_assistant_text.is_empty() && !text.is_empty() {
                            all_assistant_text.push('\n');
                        }
                        all_assistant_text.push_str(text);
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        turn_tool_uses.push((id.clone(), name.clone(), input.clone()));
                    }
                    _ => {}
                }
            }

            // Append the assistant's full response (including tool-use blocks) to history
            messages.push(Message {
                role: Role::Assistant,
                content: response.content,
            });

            if turn_tool_uses.is_empty() {
                break;
            }

            // Execute each tool and collect results
            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for (id, name, input) in &turn_tool_uses {
                let start = std::time::Instant::now();
                let permitted = self.permissions.check_tool(name, input);
                let (output, success) = if permitted {
                    let tool_input = ToolInput {
                        name: name.clone(),
                        args: input.as_object()
                            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                            .unwrap_or_default(),
                    };
                    match self.tools.get(name).map(|tool| Box::pin(tool.execute(tool_input))) {
                        Some(fut) => { let out = fut.await; (out.data, out.success) }
                        None => (format!("unknown tool: {name}"), false),
                    }
                } else {
                    (format!("tool {name} not permitted"), false)
                };

                let duration = start.elapsed().as_millis() as u64;
                all_tool_calls.push(ToolCallRecord {
                    tool_name: name.clone(),
                    input: input.clone(),
                    output: output.clone(),
                    success,
                    duration_ms: duration,
                });
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

        let usage = if total_input_tokens > 0 || total_output_tokens > 0 {
            Some(TurnUsage::new(total_input_tokens, total_output_tokens)
                .with_cost(self.provider_id, &self.model_id))
        } else {
            None
        };

        let turn = Turn {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user_message: user_input.to_string(),
            assistant_message: Some(all_assistant_text.clone()),
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

        if self.auto_compact && total_input_tokens > 0 {
            let threshold = (self.context_window() as f64 * 0.8) as u32;
            if total_input_tokens >= threshold {
                match crate::compact::run(&self.session, &self.provider, &self.memory).await {
                    Ok(r) if r.turns_compacted > 0 => {
                        tracing::info!("auto-compacted {} turn(s)", r.turns_compacted);
                    }
                    Err(e) => tracing::warn!("auto-compact failed: {e}"),
                    _ => {}
                }
            }
        }

        Ok(all_assistant_text)
    }

    /// Like `run_turn` but streams text deltas to `token_tx` as they arrive.
    /// Tool-execution lines are also sent as plain strings. Dropping `token_tx`
    /// signals the caller that output is complete.
    pub async fn run_turn_streaming(
        &self,
        user_input: &str,
        token_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<String> {
        // Hold the lock only long enough to snapshot history; release before any async I/O.
        let (prior_turns, summary, tool_specs) = {
            let session = self.session.lock().await;
            (session.turns.clone(), session.summary.clone(), self.tools.specs())
        };

        let system_text = match summary {
            Some(ref s) => format!("{}\n\n## Prior Conversation Summary\n\n{}", self.system_prompt, s),
            None => self.system_prompt.clone(),
        };

        let mut messages = vec![
            Message {
                role: Role::System,
                content: vec![ContentBlock::Text { text: system_text }],
            }
        ];

        for turn in &prior_turns {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: turn.user_message.clone() }],
            });
            if let Some(ref reply) = turn.assistant_message {
                messages.push(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text { text: reply.clone() }],
                });
            }
        }

        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: user_input.to_string() }],
        });

        let mut all_text = String::new();
        let mut all_tool_calls: Vec<ToolCallRecord> = Vec::new();
        let mut total_input_tokens = 0u32;
        let mut total_output_tokens = 0u32;
        const MAX_ITERATIONS: usize = 20;

        // Propagate cancellation to tools (e.g. spawn_agent)
        self.tools.set_cancel(self.cancel.lock().unwrap().clone());

        for _ in 0..MAX_ITERATIONS {
            if self.cancel.lock().unwrap().load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!("cancelled"));
            }

            let request = CompletionRequest {
                messages: messages.clone(),
                tools: tool_specs.clone(),
                system_prompt: None,
                max_tokens: Some(self.max_tokens),
                temperature: None,
                stream: true,
            };

            let mut rx = self.provider.complete_stream(request).await?;

            let mut iter_text = String::new();
            let mut iter_tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

            while let Some(event) = rx.recv().await {
                let ev = event?;
                // Capture usage whenever present — may co-occur with stop_reason
                // so we extract it before the dispatch match below.
                if let Some(ref u) = ev.usage {
                    if u.input_tokens > 0 { total_input_tokens = u.input_tokens; }
                    if u.output_tokens > 0 { total_output_tokens += u.output_tokens; }
                }
                match ev {
                    StreamEvent { delta: Some(text), .. } => {
                        let _ = token_tx.send(AgentEvent::Text(text.clone())).await;
                        iter_text.push_str(&text);
                    }
                    StreamEvent { content_block: Some(ContentBlock::ToolUse { id, name, input }), .. } => {
                        iter_tool_uses.push((id, name, input));
                    }
                    StreamEvent { stop_reason: Some(_), .. } => break,
                    _ => {}
                }
            }

            if !iter_text.is_empty() {
                if !all_text.is_empty() { all_text.push('\n'); }
                all_text.push_str(&iter_text);
            }

            let mut assistant_content = Vec::new();
            if !iter_text.is_empty() {
                assistant_content.push(ContentBlock::Text { text: iter_text });
            }
            for (id, name, input) in &iter_tool_uses {
                assistant_content.push(ContentBlock::ToolUse {
                    id: id.clone(), name: name.clone(), input: input.clone(),
                });
            }
            messages.push(Message { role: Role::Assistant, content: assistant_content });

            if iter_tool_uses.is_empty() { break; }

            let mut tool_results: Vec<ContentBlock> = Vec::new();
            for (id, name, input) in &iter_tool_uses {
                let _ = token_tx.send(AgentEvent::ToolStart {
                    name: name.clone(),
                    input: input.clone(),
                }).await;

                let start = std::time::Instant::now();
                let permitted = self.permissions.check_tool(name, input);
                let (output, success) = if permitted {
                    let tool_input = ToolInput {
                        name: name.clone(),
                        args: input.as_object()
                            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                            .unwrap_or_default(),
                    };
                    match self.tools.get(name).map(|tool| Box::pin(tool.execute(tool_input))) {
                        Some(fut) => { let out = fut.await; (out.data, out.success) }
                        None => (format!("unknown tool: {name}"), false),
                    }
                } else {
                    (format!("tool {name} not permitted"), false)
                };

                let _ = token_tx.send(AgentEvent::ToolDone {
                    name: name.clone(),
                    output: output.clone(),
                    success,
                }).await;

                let duration = start.elapsed().as_millis() as u64;
                all_tool_calls.push(ToolCallRecord {
                    tool_name: name.clone(),
                    input: input.clone(),
                    output: output.clone(),
                    success,
                    duration_ms: duration,
                });
                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: output,
                    is_error: if success { None } else { Some(true) },
                });
            }

            messages.push(Message { role: Role::User, content: tool_results });

        }

        if total_input_tokens > 0 || total_output_tokens > 0 {
            let _ = token_tx.send(AgentEvent::Usage {
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
            }).await;
        }

        let usage = if total_input_tokens > 0 || total_output_tokens > 0 {
            Some(TurnUsage::new(total_input_tokens, total_output_tokens)
                .with_cost(self.provider_id, &self.model_id))
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

        if self.auto_compact && total_input_tokens > 0 {
            let threshold = (self.context_window() as f64 * 0.8) as u32;
            if total_input_tokens >= threshold {
                match crate::compact::run(&self.session, &self.provider, &self.memory).await {
                    Ok(r) if r.turns_compacted > 0 => {
                        let _ = token_tx.send(AgentEvent::AutoCompact {
                            turns_compacted: r.turns_compacted,
                        }).await;
                    }
                    Err(e) => tracing::warn!("auto-compact failed: {e}"),
                    _ => {}
                }
            }
        }

        Ok(all_text)
    }

    pub async fn compact(&self) -> anyhow::Result<crate::compact::CompactResult> {
        crate::compact::run(&self.session, &self.provider, &self.memory).await
    }
}
