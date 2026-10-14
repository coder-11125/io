/// A turn was aborted by the shared cancellation flag (e.g. the user
/// pressed Esc). Callers detect it with `err.is::<Cancelled>()`.
#[derive(Debug, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// A `user_prompt_submit` hook rejected the prompt, so the turn never
/// started. The message is the hook's stated reason. Callers detect it with
/// `err.is::<PromptBlocked>()`.
#[derive(Debug, thiserror::Error)]
#[error("prompt blocked by hook: {0}")]
pub struct PromptBlocked(pub String);

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

use crate::config::ThinkingConfig;
use crate::hooks::Hooks;
use crate::memory::SessionStore;
use crate::pricing::ModelPricing;
use crate::provider::{
    CompletionModel, CompletionRequest, ContentBlock, StreamEvent, ThinkingRequest, Usage,
};
use crate::provider::{Message, Role};
use crate::sandbox::{is_file_write_tool, InteractionMode, PermissionChecker, PermissionLevel};
use crate::tools::{ToolImage, ToolInput, ToolRegistry};
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
    /// Per-token pricing discovered from a provider catalog (`ProviderConfig::pricing_override_for`),
    /// taking precedence over the static `pricing.rs` table when set.
    pricing_override: Option<ModelPricing>,
    /// User-configured lifecycle hooks; empty (a no-op) unless set with
    /// [`Agent::with_hooks`].
    hooks: Hooks,
    /// Extended-thinking settings, adjustable mid-session (`/think`).
    thinking: Mutex<ThinkingConfig>,
    /// Project root and the memory files already in context, so memory files
    /// in subdirectories can be surfaced the first time a file there is used.
    nested_memory: Mutex<
        Option<(
            std::path::PathBuf,
            std::collections::HashSet<std::path::PathBuf>,
        )>,
    >,
    /// Most model calls in one turn (`--max-turns`); 0 means the default.
    max_iterations: std::sync::atomic::AtomicUsize,
    /// Whether the last turn stopped because it ran out of iterations.
    hit_iteration_limit: AtomicBool,
}

/// Rough size of what each part of the next request costs, in characters
/// (about four per token). For `/context`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextBreakdown {
    pub system_prompt: usize,
    pub memory_files: usize,
    pub tool_definitions: usize,
    pub summary: usize,
    pub history: usize,
}

impl ContextBreakdown {
    pub fn total_chars(&self) -> usize {
        self.system_prompt + self.memory_files + self.tool_definitions + self.summary + self.history
    }

    /// The usual rough conversion; good enough to see what is taking the room.
    pub fn tokens(chars: usize) -> usize {
        chars.div_ceil(4)
    }
}

/// Appended to the system prompt while plan mode is active.
const PLAN_MODE_PROMPT: &str = "\n\n## Plan mode is active\n\n\
    You are in plan mode: a read-only research phase. You cannot modify files or run \
    state-changing commands — those calls will be refused. Explore the codebase with read, glob, \
    grep and read-only shell commands, then call `exit_plan_mode` with a concrete markdown plan \
    (what changes, where, in what order, and any risks). The user reviews it and decides whether \
    to proceed. Do not start implementing before the plan is approved. If the user rejects it, \
    stay in plan mode and revise.";

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
        pricing_override: Option<ModelPricing>,
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
            pricing_override,
            hooks: Hooks::default(),
            thinking: Mutex::new(ThinkingConfig::default()),
            nested_memory: Mutex::new(None),
            max_iterations: std::sync::atomic::AtomicUsize::new(0),
            hit_iteration_limit: AtomicBool::new(false),
        }
    }

    /// Cap the number of model calls per turn (default 20).
    pub fn with_max_iterations(self, n: usize) -> Self {
        self.max_iterations.store(n, Ordering::Relaxed);
        self
    }

    /// Whether the most recent turn was cut short by the iteration cap.
    pub fn hit_iteration_limit(&self) -> bool {
        self.hit_iteration_limit.load(Ordering::Relaxed)
    }

    /// How much of the context each part of the conversation takes.
    pub async fn context_breakdown(&self) -> ContextBreakdown {
        let tool_definitions = self
            .tools
            .specs()
            .iter()
            .map(|t| t.name.len() + t.description.len() + t.input_schema.to_string().len())
            .sum();
        let session = self.session.lock().await;
        ContextBreakdown {
            system_prompt: self.system_prompt.len(),
            memory_files: self.project_context.as_deref().map(str::len).unwrap_or(0),
            tool_definitions,
            summary: session.summary.as_deref().map(str::len).unwrap_or(0),
            history: session
                .turns
                .iter()
                .map(|t| {
                    t.user_message.len() + t.assistant_message.as_deref().map(str::len).unwrap_or(0)
                })
                .sum(),
        }
    }

    /// A copy of the current session (for export).
    pub async fn session_snapshot(&self) -> Session {
        self.session.lock().await.clone()
    }

    /// Name the current session (shown in `/resume` and `io session list`).
    pub async fn rename_session(&self, name: &str) -> anyhow::Result<()> {
        let snapshot = {
            let mut s = self.session.lock().await;
            let name = name.trim();
            s.metadata.name = (!name.is_empty()).then(|| name.to_string());
            s.clone()
        };
        self.memory.save_session(&snapshot).await
    }

    /// Copy the current session under a new id and return it, leaving this
    /// one untouched, so the conversation can branch.
    pub async fn fork_session(&self) -> anyhow::Result<SessionId> {
        let mut copy = self.session.lock().await.clone();
        copy.id = SessionId::new();
        copy.updated_at = chrono::Utc::now();
        copy.metadata.name = copy.metadata.name.as_ref().map(|n| format!("{n} (fork)"));
        self.memory.save_session(&copy).await?;
        Ok(copy.id)
    }

    /// Surface `CLAUDE.md` / `AGENTS.md` files in subdirectories of `root` the
    /// first time a tool touches a file below them. `loaded` holds the memory
    /// files already in the context (see [`crate::instructions::loaded_set`]).
    pub fn with_nested_memory(
        self,
        root: std::path::PathBuf,
        loaded: std::collections::HashSet<std::path::PathBuf>,
    ) -> Self {
        *self.nested_memory.lock().unwrap() = Some((root, loaded));
        self
    }

    /// Instructions from not-yet-loaded memory files covering `input`'s path.
    fn nested_memory_for(&self, input: &serde_json::Value) -> String {
        let Some(path) = input
            .get("path")
            .or_else(|| input.get("file_path"))
            .and_then(|v| v.as_str())
        else {
            return String::new();
        };
        let mut guard = self.nested_memory.lock().unwrap();
        let Some((root, loaded)) = guard.as_mut() else {
            return String::new();
        };
        let found = crate::instructions::nested_for(root, std::path::Path::new(path), loaded);
        crate::instructions::render_nested(&found)
    }

    /// Attach lifecycle hooks (see [`crate::hooks`]).
    pub fn with_hooks(mut self, hooks: Hooks) -> Self {
        self.hooks = hooks;
        self
    }

    /// Start with these extended-thinking settings (from `[thinking]`).
    pub fn with_thinking(self, thinking: ThinkingConfig) -> Self {
        *self.thinking.lock().unwrap() = thinking;
        self
    }

    /// The current extended-thinking settings.
    pub fn thinking(&self) -> ThinkingConfig {
        self.thinking.lock().unwrap().clone()
    }

    /// Change the extended-thinking settings; applies from the next turn.
    pub fn set_thinking(&self, thinking: ThinkingConfig) {
        *self.thinking.lock().unwrap() = thinking;
    }

    /// The current working mode (default / accept edits / plan).
    pub fn interaction_mode(&self) -> InteractionMode {
        self.permissions.interaction_mode()
    }

    pub fn set_interaction_mode(&self, mode: InteractionMode) {
        self.permissions.set_interaction_mode(mode);
    }

    /// Advance to the next mode (the Shift+Tab cycle) and return it.
    pub fn cycle_interaction_mode(&self) -> InteractionMode {
        let next = self.interaction_mode().next();
        self.set_interaction_mode(next);
        next
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

    /// Whether the active model supports extended thinking.
    pub fn supports_thinking(&self) -> bool {
        self.provider.supports_thinking()
    }

    /// Execute a single conversation turn without streaming. Permission
    /// prompts go through the `set_prompt_fn` callback if one is set.
    pub async fn run_turn(&self, user_input: &str) -> anyhow::Result<String> {
        self.run_turn_inner(user_input, Vec::new(), None).await
    }

    /// Like [`run_turn`](Self::run_turn), with images attached to the user
    /// message (vision input). Images are sent for this turn only; the saved
    /// session keeps a note that they were attached.
    pub async fn run_turn_with_images(
        &self,
        user_input: &str,
        images: Vec<ToolImage>,
    ) -> anyhow::Result<String> {
        self.run_turn_inner(user_input, images, None).await
    }

    /// Like `run_turn` but streams events to `token_tx` as they arrive:
    /// text deltas, tool start/done, permission requests, and usage.
    /// Dropping `token_tx` signals the caller that output is complete.
    pub async fn run_turn_streaming(
        &self,
        user_input: &str,
        token_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<String> {
        self.run_turn_inner(user_input, Vec::new(), Some(&token_tx))
            .await
    }

    /// Streaming variant of [`run_turn_with_images`](Self::run_turn_with_images).
    pub async fn run_turn_streaming_with_images(
        &self,
        user_input: &str,
        images: Vec<ToolImage>,
        token_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> anyhow::Result<String> {
        self.run_turn_inner(user_input, images, Some(&token_tx))
            .await
    }

    /// Shared turn loop. `token_tx` selects the mode: `Some` streams the
    /// model response and emits UI events; `None` uses blocking completions.
    async fn run_turn_inner(
        &self,
        user_input: &str,
        images: Vec<ToolImage>,
        token_tx: Option<&tokio::sync::mpsc::Sender<AgentEvent>>,
    ) -> anyhow::Result<String> {
        // Hold the lock only long enough to snapshot history; release before any async I/O.
        let (prior_turns, summary, tool_specs, session_id) = {
            let session = self.session.lock().await;
            (
                session.turns.clone(),
                session.summary.clone(),
                self.tools.specs(),
                session.id.to_string(),
            )
        };

        // A user_prompt_submit hook can reject the prompt before anything is
        // sent or saved, or contribute extra context for the model. The
        // context rides along on this request only; the saved turn keeps the
        // user's text as typed.
        let prompt_context = self
            .hooks
            .user_prompt_submit(&session_id, user_input)
            .await
            .map_err(PromptBlocked)?;
        // session_start fires once, ahead of the first turn (after the
        // prompt hooks, so a rejected prompt doesn't use it up).
        let source = if prior_turns.is_empty() && summary.is_none() {
            "startup"
        } else {
            "resume"
        };
        let hook_context = match (
            self.hooks.session_start(&session_id, source).await,
            prompt_context,
        ) {
            (Some(start), Some(prompt)) => Some(format!("{start}\n{prompt}")),
            (start, prompt) => start.or(prompt),
        };
        let model_input = match hook_context {
            Some(ctx) => format!("{user_input}\n\n<hook-context>\n{ctx}\n</hook-context>"),
            None => user_input.to_string(),
        };

        let mut messages = vec![self.system_message(summary.as_deref())];

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

        // Attached images lead the message, as providers expect.
        let attached = images.len();
        let mut user_content: Vec<ContentBlock> = images
            .into_iter()
            .map(|i| ContentBlock::Image {
                media_type: i.media_type,
                data: i.data,
            })
            .collect();
        user_content.push(ContentBlock::Text { text: model_input });
        messages.push(Message {
            role: Role::User,
            content: user_content,
        });

        // Extended thinking for this turn: the configured setting and/or a
        // keyword in the prompt, with output room reserved for the answer.
        // A model that can't reason gets neither a budget nor extra output room.
        let thinking_budget = if self.provider.supports_thinking() {
            crate::thinking::budget_for_turn(&self.thinking.lock().unwrap(), user_input)
        } else {
            None
        };
        let request_max_tokens = match thinking_budget {
            Some(budget) => crate::thinking::max_tokens_with_thinking(self.max_tokens, budget),
            None => self.max_tokens,
        };

        let mut all_text = String::new();
        let mut all_tool_calls: Vec<ToolCallRecord> = Vec::new();
        // Billing-accurate sums across loop iterations (each API call bills
        // its own input), vs. the last reported context size which reflects
        // the current full context and drives the context bar and
        // auto-compact. Cache reads/writes count toward the context size.
        let mut tally = UsageTally::default();
        // A provider failure mid-turn. Set instead of returning immediately so
        // partial progress (text, executed tools) is still saved to the session.
        let mut turn_error: Option<anyhow::Error> = None;
        const DEFAULT_MAX_ITERATIONS: usize = 20;
        let max_iterations = match self.max_iterations.load(Ordering::Relaxed) {
            0 => DEFAULT_MAX_ITERATIONS,
            n => n,
        };
        // Cleared by the two ways a turn ends on its own; still set after the
        // loop means the cap cut it short.
        let mut exhausted = true;
        // Track files written during this turn to detect write-then-execute sequences.
        let mut written_this_turn: Vec<String> = Vec::new();

        // Propagate cancellation to tools (e.g. spawn_agent)
        self.tools.set_cancel(self.cancel.lock().unwrap().clone());

        for _ in 0..max_iterations {
            if self.cancel.lock().unwrap().load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }

            // The plan-mode notice follows the live mode, which can change
            // during the loop (the user approves the plan).
            messages[0] = self.system_message(summary.as_deref());

            let request = CompletionRequest {
                messages: messages.clone(),
                tools: tool_specs.clone(),
                system_prompt: None,
                max_tokens: Some(request_max_tokens),
                temperature: None,
                stream: token_tx.is_some(),
                thinking: thinking_budget.map(|budget_tokens| ThinkingRequest { budget_tokens }),
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
                    // Finished reasoning blocks, which the next request must
                    // carry back verbatim when this response calls tools.
                    let mut thinking_blocks: Vec<ContentBlock> = Vec::new();

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
                            tally.add(u);
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
                            // A signature-less block is a display delta; a
                            // signed one is the finished block to replay.
                            StreamEvent {
                                content_block:
                                    Some(ContentBlock::Thinking {
                                        thinking,
                                        signature: None,
                                    }),
                                ..
                            } => {
                                if !thinking.is_empty() {
                                    let _ = tx.send(AgentEvent::Thinking(thinking)).await;
                                }
                            }
                            StreamEvent {
                                content_block: Some(block @ ContentBlock::Thinking { .. }),
                                ..
                            }
                            | StreamEvent {
                                content_block: Some(block @ ContentBlock::RedactedThinking { .. }),
                                ..
                            } => thinking_blocks.push(block),
                            StreamEvent {
                                stop_reason: Some(_),
                                ..
                            } => break,
                            _ => {}
                        }
                    }

                    let mut content = thinking_blocks;
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
                        tally.add(u);
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

                    // Display-only (unsigned) reasoning is never replayed.
                    let content = response
                        .content
                        .into_iter()
                        .filter(|b| {
                            !matches!(
                                b,
                                ContentBlock::Thinking {
                                    signature: None,
                                    ..
                                }
                            )
                        })
                        .collect();
                    (content, iter_text, tool_uses)
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
                exhausted = false;
                break;
            }

            if iter_tool_uses.is_empty() {
                exhausted = false;
                break;
            }

            // Execute the tool calls, in order. Each call is gated first (start
            // event, pre_tool_use hooks, permission — which may prompt the
            // user), then run and reported.
            let mut tool_results: Vec<ContentBlock> = Vec::new();
            // Images tools returned (e.g. `read` on a screenshot), sent after
            // the tool results in the same user message.
            let mut tool_images: Vec<ContentBlock> = Vec::new();
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
                // pre_tool_use hooks run before the permission check: they can
                // veto a call or force a prompt, but never grant permission
                // that the checker withholds.
                let verdict = match self.hooks.pre_tool_use(&session_id, name, input).await {
                    Err(reason) => Err(format!("blocked by pre_tool_use hook: {reason}")),
                    Ok(pre) => match self
                        .resolve_permission(
                            name,
                            input,
                            token_tx,
                            force_prompt || pre.ask,
                            &session_id,
                        )
                        .await
                    {
                        Err(denial) => Err(denial),
                        Ok(()) => Ok(pre.context),
                    },
                };
                let gated = Gated {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    start,
                    verdict,
                };
                let mut sink = Sink {
                    written: &mut written_this_turn,
                    calls: &mut all_tool_calls,
                    results: &mut tool_results,
                    images: &mut tool_images,
                };
                self.settle(gated, &session_id, token_tx, &mut sink).await;
            }

            // Feed results back into the conversation
            tool_results.extend(tool_images);
            messages.push(Message {
                role: Role::User,
                content: tool_results,
            });
        }

        self.hit_iteration_limit.store(exhausted, Ordering::Relaxed);

        // A failure before anything was produced: fail the turn outright
        // rather than persisting an empty record.
        if all_text.is_empty() && all_tool_calls.is_empty() {
            if let Some(e) = turn_error {
                return Err(e);
            }
        }

        if let Some(tx) = token_tx {
            if tally.last_context_tokens > 0 || tally.output_tokens > 0 {
                let _ = tx
                    .send(AgentEvent::Usage {
                        input_tokens: tally.last_context_tokens,
                        output_tokens: tally.output_tokens,
                    })
                    .await;
            }
        }

        let usage = tally.any().then(|| {
            TurnUsage::new(tally.input_tokens, tally.output_tokens)
                .with_cache(tally.cache_read_tokens, tally.cache_write_tokens)
                .with_cost(
                    self.provider_id,
                    &self.model_id,
                    self.pricing_override.clone(),
                )
        });

        let turn = Turn {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user_message: if attached == 0 {
                user_input.to_string()
            } else {
                // Images aren't persisted; leave a note so later turns know.
                format!("{user_input}\n\n[{attached} image(s) were attached to this message]")
            },
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

        if self.auto_compact && tally.last_context_tokens > 0 {
            let threshold = (self.context_window() as f64 * 0.8) as u32;
            if tally.last_context_tokens >= threshold {
                match self.compact_for("auto").await {
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

        self.hooks.stop(&session_id, &all_text).await;

        Ok(all_text)
    }

    /// The system message for the next request: the agent's prompt, the
    /// compaction summary if any, and the plan-mode notice while plan mode is
    /// on.
    fn system_message(&self, summary: Option<&str>) -> Message {
        let mut text = match summary {
            Some(s) => format!(
                "{}\n\n## Prior Conversation Summary\n\n{}",
                self.system_prompt, s
            ),
            None => self.system_prompt.clone(),
        };
        if self.interaction_mode() == InteractionMode::Plan {
            text.push_str(PLAN_MODE_PROMPT);
        }
        Message {
            role: Role::System,
            content: vec![ContentBlock::Text { text }],
        }
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
        session_id: &str,
    ) -> Result<(), String> {
        let in_plan_mode = self.interaction_mode() == InteractionMode::Plan;
        if name == "exit_plan_mode" && !in_plan_mode {
            return Err(
                "exit_plan_mode only applies in plan mode, and plan mode is not active — there \
                 is nothing to exit. Carry on with the task."
                    .to_string(),
            );
        }
        let decision = self.permissions.decide_tool(name, input);
        let effective = if force_prompt && decision == PermissionLevel::Allow {
            PermissionLevel::Prompt
        } else {
            decision
        };
        match effective {
            PermissionLevel::Allow => Ok(()),
            PermissionLevel::Deny
                if in_plan_mode
                    && (is_file_write_tool(name)
                        || name == "bash"
                        || name.starts_with("mcp__")) =>
            {
                Err(format!(
                    "{name} refused: plan mode is active, so file changes and state-changing \
                     commands are not allowed. Research with read-only tools, then present your \
                     plan with exit_plan_mode."
                ))
            }
            PermissionLevel::Deny => Err(format!("tool {name} not permitted")),
            PermissionLevel::Prompt => {
                // Tell any notification hooks the user is needed (fire and
                // forget — it never delays the prompt).
                let detail = match name {
                    "bash" => input.get("command").and_then(|v| v.as_str()),
                    n if is_file_write_tool(n) => input.get("path").and_then(|v| v.as_str()),
                    _ => None,
                };
                self.hooks.notify(
                    session_id,
                    "permission",
                    &match detail {
                        Some(d) => format!("io needs your permission to use {name}: {d}"),
                        None => format!("io needs your permission to use {name}"),
                    },
                );
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
                if name == "exit_plan_mode" {
                    // Approving the plan ends plan mode; "always" (the second
                    // option in the plan dialog) also accepts edits from here.
                    return match reply {
                        PermissionReply::AllowOnce => {
                            self.set_interaction_mode(InteractionMode::Default);
                            Ok(())
                        }
                        PermissionReply::AllowSession => {
                            self.set_interaction_mode(InteractionMode::AcceptEdits);
                            Ok(())
                        }
                        PermissionReply::Deny => Err(
                            "The user did not approve the plan, so plan mode stays on. Ask what \
                             they want changed, or revise the plan and call exit_plan_mode again."
                                .to_string(),
                        ),
                    };
                }
                match reply {
                    PermissionReply::AllowOnce => Ok(()),
                    PermissionReply::AllowSession => {
                        let command = if name == "bash" {
                            input.get("command").and_then(|v| v.as_str())
                        } else if is_file_write_tool(name) {
                            // Per-path approval: "always" for file writes approves only this path
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

    /// Run one gated tool call, then report it: post-hooks, the `ToolDone`
    /// event, the saved record and the tool result for the model.
    async fn settle(
        &self,
        g: Gated,
        session_id: &str,
        token_tx: Option<&tokio::sync::mpsc::Sender<AgentEvent>>,
        sink: &mut Sink<'_>,
    ) {
        let (mut output, success, snapshot) = match g.verdict {
            Ok(pre_context) => {
                let run = self.execute_tool(&g.name, &g.input).await;
                sink.images
                    .extend(run.images.into_iter().map(|i| ContentBlock::Image {
                        media_type: i.media_type,
                        data: i.data,
                    }));
                let mut output = run.output;
                if run.success
                    && matches!(
                        g.name.as_str(),
                        "read" | "write" | "edit" | "multi_edit" | "notebook_edit"
                    )
                {
                    output.push_str(&self.nested_memory_for(&g.input));
                }
                // post_tool_use feedback (formatter/linter output, etc.) is
                // appended to what the model sees.
                let feedback = self
                    .hooks
                    .post_tool_use(session_id, &g.name, &g.input, &output, run.success)
                    .await;
                if let Some(context) = pre_context {
                    output.push_str(&format!("\n\n[pre_tool_use hook]\n{context}"));
                }
                if let Some(feedback) = feedback {
                    output.push_str(&format!("\n\n[post_tool_use hook]\n{feedback}"));
                }
                (output, run.success, run.snapshot)
            }
            // Blocked or denied calls never ran: nothing to react to.
            Err(denial) => (denial, false, None),
        };
        if success && is_file_write_tool(&g.name) {
            if let Some(path) = g.input.get("path").and_then(|v| v.as_str()) {
                sink.written.push(path.to_string());
            }
        }
        if let Some(tx) = token_tx {
            let _ = tx
                .send(AgentEvent::ToolDone {
                    name: g.name.clone(),
                    output: output.clone(),
                    success,
                })
                .await;
        }
        let record = ToolCallRecord {
            tool_name: g.name,
            input: g.input,
            output: output.clone(),
            success,
            duration_ms: g.start.elapsed().as_millis() as u64,
            pre_edit_snapshot: snapshot,
        };
        append_audit_log(&record);
        sink.calls.push(record);
        sink.results.push(ContentBlock::ToolResult {
            tool_use_id: g.id,
            content: std::mem::take(&mut output),
            is_error: if success { None } else { Some(true) },
        });
    }

    async fn execute_tool(&self, name: &str, input: &serde_json::Value) -> ToolRun {
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
                ToolRun {
                    output: out.data,
                    success: out.success,
                    snapshot: out.pre_edit_snapshot,
                    images: out.images,
                }
            }
            None => ToolRun {
                output: format!("unknown tool: {name}"),
                success: false,
                snapshot: None,
                images: Vec::new(),
            },
        }
    }

    pub async fn compact(&self) -> anyhow::Result<crate::compact::CompactResult> {
        self.compact_for("manual").await
    }

    /// Compact the session after the `pre_compact` hooks have had their say:
    /// a hook can cancel it, or add guidance for the summary. `trigger` is
    /// `manual` or `auto`.
    async fn compact_for(&self, trigger: &str) -> anyhow::Result<crate::compact::CompactResult> {
        let session_id = self.session.lock().await.id.to_string();
        let guidance = self
            .hooks
            .pre_compact(&session_id, trigger)
            .await
            .map_err(|reason| anyhow::anyhow!("compaction cancelled by a hook: {reason}"))?;
        crate::compact::run(
            &self.session,
            &self.provider,
            &self.memory,
            guidance.as_deref(),
        )
        .await
    }

    /// Tell the `session_end` hooks this session is over. `reason` is
    /// `exit`, `new` or `single_shot`.
    pub async fn end_session(&self, reason: &str) {
        let session_id = self.session.lock().await.id.to_string();
        self.hooks.session_end(&session_id, reason).await;
    }

    /// Turns in the current session, for a caller (e.g. the `/rewind` picker)
    /// to list as candidate rewind points.
    pub async fn session_turns(&self) -> Vec<Turn> {
        self.session.lock().await.turns.clone()
    }

    /// Restore files to their state right before `turn_index`, and drop that
    /// turn and every turn after it from the session.
    pub async fn rewind_to(
        &self,
        turn_index: usize,
    ) -> anyhow::Result<crate::rewind::RewindResult> {
        crate::rewind::run(&self.session, &self.memory, turn_index).await
    }
}

/// A tool call that has been through its start event, hooks and permission
/// check. `verdict` is `Ok(pre_tool_use context)` to run it, or `Err(why)`
/// with the message the model should see instead.
struct Gated {
    id: String,
    name: String,
    input: serde_json::Value,
    start: std::time::Instant,
    verdict: Result<Option<String>, String>,
}

/// Where `settle` records what the calls produced.
struct Sink<'a> {
    /// Paths written this turn (a later `bash` mentioning one gets a prompt).
    written: &'a mut Vec<String>,
    calls: &'a mut Vec<ToolCallRecord>,
    results: &'a mut Vec<ContentBlock>,
    images: &'a mut Vec<ContentBlock>,
}

/// What one tool call produced, as the agent loop needs it.
struct ToolRun {
    output: String,
    success: bool,
    snapshot: Option<crate::tools::FileSnapshot>,
    images: Vec<ToolImage>,
}

/// Token usage accumulated across the model calls of one turn.
#[derive(Default)]
struct UsageTally {
    /// Sums over every call (each call bills its own input). `input_tokens`
    /// is uncached input only; cache traffic is summed separately.
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    /// Full context size (uncached + cache reads + cache writes) reported by
    /// the most recent call — what the context bar and auto-compact use.
    last_context_tokens: u32,
}

impl UsageTally {
    fn add(&mut self, u: &Usage) {
        let context = u.context_tokens();
        if context > 0 {
            self.last_context_tokens = context;
        }
        self.input_tokens = self.input_tokens.saturating_add(u.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(u.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(u.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(u.cache_write_tokens);
    }

    fn any(&self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_tokens > 0
            || self.cache_write_tokens > 0
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
