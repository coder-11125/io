//! Integration tests for the agent loop, driven by a scripted mock provider.
//! These exercise the full turn lifecycle: message building, tool execution,
//! permission resolution, streaming events, usage tracking, and session
//! persistence — everything except a real LLM HTTP call.

use async_trait::async_trait;
use io_runtime::agent::{Agent, AgentEvent, PermissionReply, PromptBlocked};
use io_runtime::config::ThinkingConfig;
use io_runtime::hooks::{HookEntry, Hooks, HooksConfig};
use io_runtime::memory::SessionStore;
use io_runtime::provider::{
    CompletionModel, CompletionRequest, CompletionResponse, ContentBlock, StreamEvent, Usage,
};
use io_runtime::sandbox::{InteractionMode, PermissionChecker};
use io_runtime::tools::{default_registry, ToolImage};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Scripted provider: pops one canned response per model call and records
/// every request it receives so tests can assert on the message history.
#[derive(Debug, Default)]
struct MockProvider {
    responses: Mutex<VecDeque<CompletionResponse>>,
    requests: Mutex<Vec<CompletionRequest>>,
    /// 0 (the derived default) means "unset" — `context_window()` falls back
    /// to the trait's default of 128K, same as a real provider with no
    /// configured/guessed window.
    context_window: AtomicU64,
    /// Whether the mock claims to support extended thinking (default: yes).
    thinking_capable: std::sync::atomic::AtomicBool,
}

impl MockProvider {
    fn new(responses: Vec<CompletionResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
            context_window: AtomicU64::new(0),
            thinking_capable: std::sync::atomic::AtomicBool::new(true),
        })
    }

    fn recorded_requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn set_context_window(&self, tokens: u64) {
        self.context_window.store(tokens, Ordering::Relaxed);
    }

    fn next_response(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("mock provider ran out of scripted responses"))
    }
}

#[async_trait]
impl CompletionModel for MockProvider {
    fn provider_name(&self) -> &'static str {
        "mock"
    }

    fn context_window(&self) -> u64 {
        match self.context_window.load(Ordering::Relaxed) {
            0 => 128_000,
            tokens => tokens,
        }
    }

    fn supports_thinking(&self) -> bool {
        self.thinking_capable.load(Ordering::Relaxed)
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        self.next_response(request)
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        let response = self.next_response(request)?;
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            for block in response.content {
                let event = match block {
                    ContentBlock::Text { text } => StreamEvent {
                        delta: Some(text),
                        content_block: None,
                        stop_reason: None,
                        usage: None,
                    },
                    tool_use @ ContentBlock::ToolUse { .. } => StreamEvent {
                        delta: None,
                        content_block: Some(tool_use),
                        stop_reason: None,
                        usage: None,
                    },
                    // Reasoning blocks stream through unchanged: unsigned ones
                    // are display deltas, signed ones are finished blocks.
                    reasoning @ (ContentBlock::Thinking { .. }
                    | ContentBlock::RedactedThinking { .. }) => StreamEvent {
                        delta: None,
                        content_block: Some(reasoning),
                        stop_reason: None,
                        usage: None,
                    },
                    _ => continue,
                };
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
            let _ = tx
                .send(Ok(StreamEvent {
                    delta: None,
                    content_block: None,
                    stop_reason: Some("stop".to_string()),
                    usage: response.usage,
                }))
                .await;
        });
        Ok(rx)
    }
}

fn text_response(text: &str) -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: Some("end_turn".to_string()),
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 10,
            ..Usage::default()
        }),
    }
}

fn text_response_with_usage(
    text: &str,
    input_tokens: u32,
    output_tokens: u32,
) -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: Some("end_turn".to_string()),
        usage: Some(Usage {
            input_tokens,
            output_tokens,
            ..Usage::default()
        }),
    }
}

fn tool_response(name: &str, input: serde_json::Value) -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: name.to_string(),
            input,
        }],
        stop_reason: Some("tool_use".to_string()),
        usage: Some(Usage {
            input_tokens: 100,
            output_tokens: 10,
            ..Usage::default()
        }),
    }
}

/// Unique temp path per call so parallel tests don't share state.
fn temp_path(prefix: &str, suffix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{nanos}-{n}{suffix}"))
}

fn make_agent_at(
    provider: Arc<MockProvider>,
    permissions: PermissionChecker,
    db_path: PathBuf,
    session_id: Option<io_runtime::SessionId>,
) -> Agent {
    let memory = SessionStore::with_path(db_path).expect("session store");
    Agent::new(
        provider,
        default_registry(),
        memory,
        Arc::new(permissions),
        "You are a test agent.".to_string(),
        None,
        session_id,
        "mock-model".to_string(),
        1024,
        false,
        None,
    )
}

fn make_agent(provider: Arc<MockProvider>, permissions: PermissionChecker) -> Agent {
    make_agent_at(provider, permissions, temp_path("io-test", ".db"), None)
}

/// Like `make_agent_at` but with auto-compact turned on, for testing the
/// 80%-of-context-window auto-compact trigger in `run_turn_inner`.
fn make_auto_compact_agent(
    provider: Arc<MockProvider>,
    permissions: PermissionChecker,
    db_path: PathBuf,
) -> Agent {
    let memory = SessionStore::with_path(db_path).expect("session store");
    Agent::new(
        provider,
        default_registry(),
        memory,
        Arc::new(permissions),
        "You are a test agent.".to_string(),
        None,
        None,
        "mock-model".to_string(),
        1024,
        true,
        None,
    )
}

#[tokio::test]
async fn text_only_turn_returns_text_and_saves_session() {
    let provider = MockProvider::new(vec![text_response("hello from the mock")]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
        None,
    );

    let reply = agent.run_turn("hi").await.expect("turn should succeed");
    assert_eq!(reply, "hello from the mock");

    // The turn must be persisted with usage recorded.
    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert_eq!(session.turns.len(), 1);
    assert_eq!(session.turns[0].user_message, "hi");
    assert_eq!(
        session.turns[0].assistant_message.as_deref(),
        Some("hello from the mock")
    );
    let usage = session.turns[0].usage.as_ref().expect("usage recorded");
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 10);

    // Exactly one model call, carrying the system prompt and the user message.
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages.len(), 2);
}

#[tokio::test]
async fn tool_call_loop_executes_and_feeds_results_back() {
    let file = temp_path("io-test-read", ".txt");
    std::fs::write(&file, "file contents here").unwrap();

    let provider = MockProvider::new(vec![
        tool_response(
            "read",
            serde_json::json!({"file_path": file.to_str().unwrap()}),
        ),
        text_response("done reading"),
    ]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
        None,
    );

    let reply = agent.run_turn("read that file").await.unwrap();
    assert_eq!(reply, "done reading");

    // Two model calls: the second must contain the tool result.
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let last_message = requests[1].messages.last().unwrap();
    let has_tool_result = last_message.content.iter().any(|b| {
        matches!(b, ContentBlock::ToolResult { content, is_error, .. }
            if content.contains("file contents here") && is_error.is_none())
    });
    assert!(
        has_tool_result,
        "second request should carry the tool result"
    );

    // The turn record captures the successful tool call, and usage sums
    // across both model calls (each call bills its own input).
    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    let turn = &session.turns[0];
    assert_eq!(turn.tool_calls.len(), 1);
    assert!(turn.tool_calls[0].success);
    assert_eq!(turn.tool_calls[0].tool_name, "read");
    let usage = turn.usage.as_ref().unwrap();
    assert_eq!(usage.input_tokens, 200);
    assert_eq!(usage.output_tokens, 20);
}

#[tokio::test]
async fn prompt_mode_denies_mutating_tool_without_prompt() {
    let target = temp_path("io-test-write", ".txt");
    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "x"}),
        ),
        text_response("ok"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("prompt"));

    agent.run_turn("write the file").await.unwrap();

    // No prompt_fn is set: the call must be denied and the file not created.
    assert!(!target.exists(), "denied write must not create the file");
    let requests = provider.recorded_requests();
    let last_message = requests[1].messages.last().unwrap();
    let denied = last_message.content.iter().any(|b| {
        matches!(b, ContentBlock::ToolResult { content, is_error, .. }
            if content.contains("no prompt is available") && *is_error == Some(true))
    });
    assert!(
        denied,
        "model should see the denial as an error tool result"
    );
}

#[tokio::test]
async fn prompt_fn_allow_once_executes_tool() {
    let target = temp_path("io-test-write", ".txt");
    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "approved"}),
        ),
        text_response("written"),
    ]);
    let agent = make_agent(provider, PermissionChecker::new("prompt"));
    agent.set_prompt_fn(Arc::new(|_, _| PermissionReply::AllowOnce));

    let reply = agent.run_turn("write the file").await.unwrap();
    assert_eq!(reply, "written");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "approved");
    std::fs::remove_file(&target).ok();
}

#[tokio::test]
async fn allow_session_skips_subsequent_prompts() {
    let t1 = temp_path("io-test-write", ".txt");
    let t2 = temp_path("io-test-write", ".txt");
    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": t1.to_str().unwrap(), "content": "a"}),
        ),
        tool_response(
            "write",
            serde_json::json!({"path": t2.to_str().unwrap(), "content": "b"}),
        ),
        text_response("both written"),
    ]);
    let agent = make_agent(provider, PermissionChecker::new("prompt"));

    let prompt_count = Arc::new(AtomicU32::new(0));
    let counter = prompt_count.clone();
    agent.set_prompt_fn(Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        PermissionReply::AllowSession
    }));

    let reply = agent.run_turn("write both files").await.unwrap();
    assert_eq!(reply, "both written");
    assert!(t1.exists() && t2.exists());
    assert_eq!(
        prompt_count.load(Ordering::Relaxed),
        2,
        "AllowSession for write grants per-path approval: each distinct path prompts once"
    );
    std::fs::remove_file(&t1).ok();
    std::fs::remove_file(&t2).ok();
}

#[tokio::test]
async fn streaming_emits_events_and_permission_is_answerable() {
    let target = temp_path("io-test-write", ".txt");
    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "streamed"}),
        ),
        text_response("stream done"),
    ]);
    let agent = make_agent(provider, PermissionChecker::new("prompt"));

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    // UI stand-in: collect events, approve any permission request.
    let consumer = tokio::spawn(async move {
        let mut text = String::new();
        let mut tool_started = false;
        let mut tool_succeeded = false;
        let mut usage_seen = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Text(delta) => text.push_str(&delta),
                AgentEvent::ToolStart { .. } => tool_started = true,
                AgentEvent::ToolDone { success, .. } => tool_succeeded = success,
                AgentEvent::PermissionRequest { respond, .. } => {
                    let _ = respond.send(PermissionReply::AllowOnce);
                }
                AgentEvent::Usage { .. } => usage_seen = true,
                _ => {}
            }
        }
        (text, tool_started, tool_succeeded, usage_seen)
    });

    let reply = agent.run_turn_streaming("write it", tx).await.unwrap();
    assert_eq!(reply, "stream done");

    let (text, tool_started, tool_succeeded, usage_seen) = consumer.await.unwrap();
    assert_eq!(text, "stream done");
    assert!(tool_started, "ToolStart event expected");
    assert!(tool_succeeded, "tool should run after approval");
    assert!(usage_seen, "Usage event expected");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "streamed");
    std::fs::remove_file(&target).ok();
}

#[tokio::test]
async fn provider_failure_mid_turn_saves_partial_progress() {
    let file = temp_path("io-test-read", ".txt");
    std::fs::write(&file, "partial progress contents").unwrap();

    // One successful tool iteration, then the provider dies (script empty).
    let provider = MockProvider::new(vec![tool_response(
        "read",
        serde_json::json!({"file_path": file.to_str().unwrap()}),
    )]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(provider, PermissionChecker::new("allow"), db.clone(), None);

    let err = agent.run_turn("read it").await.expect_err("turn must fail");
    assert!(err.to_string().contains("ran out of scripted responses"));

    // The executed tool call must be persisted despite the failure.
    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert_eq!(session.turns.len(), 1);
    assert_eq!(session.turns[0].tool_calls.len(), 1);
    assert!(session.turns[0].tool_calls[0].success);
}

#[tokio::test]
async fn sub_agent_inherits_permissions_and_fails_closed() {
    let target = temp_path("io-test-spawn-touch", ".txt");
    let touch_cmd = format!("touch {}", target.to_str().unwrap());

    // Parent asks to spawn the git sub-agent (has bash access); the sub-agent
    // tries to run a command that is neither allowlisted nor approvable
    // (sub-agents cannot prompt), so it must be denied.
    let provider = MockProvider::new(vec![
        tool_response(
            "spawn_agent",
            serde_json::json!({"agent_id": "git", "task": "touch a file"}),
        ),
        tool_response("bash", serde_json::json!({"command": touch_cmd})),
        text_response("sub-agent done"),
        text_response("parent done"),
    ]);

    let permissions = Arc::new(PermissionChecker::new("prompt"));
    let mut tools = default_registry();
    tools.register(Box::new(io_runtime::SpawnAgentTool::new(
        provider.clone(),
        "mock-model".to_string(),
        1024,
        permissions.clone(),
        None,
    )));
    let memory = SessionStore::with_path(temp_path("io-test", ".db")).expect("session store");
    let agent = Agent::new(
        provider.clone(),
        tools,
        memory,
        permissions,
        "You are a test agent.".to_string(),
        None,
        None,
        "mock-model".to_string(),
        1024,
        false,
        None,
    );
    // The user approves spawning the sub-agent — but nothing else.
    agent.set_prompt_fn(Arc::new(|name, _| {
        if name == "spawn_agent" {
            PermissionReply::AllowOnce
        } else {
            PermissionReply::Deny
        }
    }));

    let reply = agent.run_turn("delegate this").await.unwrap();
    assert_eq!(reply, "parent done");
    assert!(
        !target.exists(),
        "sub-agent bash must fail closed, not execute"
    );

    // The sub-agent's second request must carry the denial as an error result.
    let requests = provider.recorded_requests();
    let denied = requests.iter().flat_map(|r| r.messages.iter()).any(|m| {
        m.content.iter().any(|b| {
            matches!(b, ContentBlock::ToolResult { content, is_error, .. }
                if content.contains("no prompt is available") && *is_error == Some(true))
        })
    });
    assert!(denied, "sub-agent tool call should be denied, not allowed");
}

#[tokio::test]
async fn resumed_session_carries_prior_history() {
    let db = temp_path("io-test", ".db");

    // Turn 1 in a fresh session.
    let provider1 = MockProvider::new(vec![text_response("first answer")]);
    let agent1 = make_agent_at(provider1, PermissionChecker::new("allow"), db.clone(), None);
    agent1.run_turn("remember the number 42").await.unwrap();
    let sid = agent1.session_id().await;
    drop(agent1);

    // A new agent resuming the same session — what /model and /connect do.
    let provider2 = MockProvider::new(vec![text_response("second answer")]);
    let agent2 = make_agent_at(
        provider2.clone(),
        PermissionChecker::new("allow"),
        db,
        Some(sid),
    );
    assert_eq!(agent2.session_id().await, sid, "session id must survive");

    agent2.run_turn("what was the number?").await.unwrap();

    // The resumed agent's request must include turn-1 history.
    let requests = provider2.recorded_requests();
    let all_text: String = requests[0]
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        all_text.contains("remember the number 42"),
        "prior user message should be in context"
    );
    assert!(
        all_text.contains("first answer"),
        "prior assistant reply should be in context"
    );
}

#[tokio::test]
async fn auto_compact_triggers_after_turn_crosses_80_percent_threshold() {
    let provider = MockProvider::new(vec![
        // The task's own turn: 900/1000 = 90%, past the 80% threshold.
        text_response_with_usage("task done", 900, 10),
        // compact::run's own summarization call (non-streaming `complete`).
        text_response_with_usage("summary of the session", 50, 20),
    ]);
    provider.set_context_window(1000);
    let db = temp_path("io-test", ".db");
    let agent = make_auto_compact_agent(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    let consumer = tokio::spawn(async move {
        let mut turns_compacted = None;
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::AutoCompact { turns_compacted: n } = ev {
                turns_compacted = Some(n);
            }
        }
        turns_compacted
    });

    // The task itself must complete and return normally — compaction is a
    // side effect that happens after, not something that interrupts it.
    let reply = agent.run_turn_streaming("do the task", tx).await.unwrap();
    assert_eq!(reply, "task done");

    let turns_compacted = consumer.await.unwrap();
    assert_eq!(
        turns_compacted,
        Some(1),
        "AutoCompact event should fire once the turn's usage crosses 80% of the context window"
    );

    // The task's turn was compacted away into a summary, post-hoc.
    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert!(
        session.turns.is_empty(),
        "compacted turns should be cleared from the session"
    );
    assert_eq!(session.summary.as_deref(), Some("summary of the session"));
}

#[tokio::test]
async fn auto_compact_does_not_trigger_below_80_percent_threshold() {
    // 700/1000 = 70%, under the 80% threshold — no second (summarization)
    // response is scripted, so the test would fail loudly if compaction
    // fired anyway and tried to call the provider again.
    let provider = MockProvider::new(vec![text_response_with_usage("task done", 700, 10)]);
    provider.set_context_window(1000);
    let db = temp_path("io-test", ".db");
    let agent = make_auto_compact_agent(provider, PermissionChecker::new("allow"), db.clone());

    let reply = agent.run_turn("do the task").await.unwrap();
    assert_eq!(reply, "task done");

    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert_eq!(
        session.turns.len(),
        1,
        "turn should not be compacted below the 80% threshold"
    );
    assert!(session.summary.is_none());
}

#[tokio::test]
async fn auto_compact_second_pass_folds_in_prior_summary() {
    let provider = MockProvider::new(vec![
        // First turn crosses the threshold and triggers compaction.
        text_response_with_usage("first task done", 900, 10),
        text_response_with_usage("summary of round one", 50, 20),
        // Second turn also crosses the threshold, triggering a second compaction.
        text_response_with_usage("second task done", 900, 10),
        text_response_with_usage("summary of round two", 50, 20),
    ]);
    provider.set_context_window(1000);
    let db = temp_path("io-test", ".db");
    let agent = make_auto_compact_agent(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
    );

    agent.run_turn("do the first task").await.unwrap();
    agent.run_turn("do the second task").await.unwrap();

    // The prompt for the second compaction call must include the first
    // compaction's summary, so nothing from before it is lost.
    let requests = provider.recorded_requests();
    let second_compact_request = &requests[3];
    let prompt_text: String = second_compact_request
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        prompt_text.contains("summary of round one"),
        "second compaction's summarization prompt should carry forward the prior summary"
    );

    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert_eq!(session.summary.as_deref(), Some("summary of round two"));
}

#[tokio::test]
async fn rewind_restores_file_and_truncates_session() {
    let file = temp_path("io-test-rewind", ".txt");
    let path_str = file.to_str().unwrap().to_string();

    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": path_str, "content": "version one"}),
        ),
        text_response("wrote it"),
        tool_response(
            "edit",
            serde_json::json!({"path": path_str, "old_string": "one", "new_string": "two"}),
        ),
        text_response("edited it"),
    ]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
        None,
    );

    agent.run_turn("write the file").await.unwrap();
    agent.run_turn("edit the file").await.unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "version two");

    let result = agent.rewind_to(1).await.unwrap();

    assert_eq!(result.turns_dropped, 1);
    assert_eq!(result.files_restored, vec![path_str]);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "version one",
        "file should be restored to its state before the reverted edit"
    );
    std::fs::remove_file(&file).ok();

    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert_eq!(
        session.turns.len(),
        1,
        "rewound turn should be dropped from the session"
    );
}

// ── Prompt-cache accounting ───────────────────────────────────────────────────

fn text_response_with_cache(
    text: &str,
    input_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
) -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: Some("end_turn".to_string()),
        usage: Some(Usage {
            input_tokens,
            output_tokens: 10,
            cache_read_tokens,
            cache_write_tokens,
        }),
    }
}

#[tokio::test]
async fn cache_tokens_count_toward_context_size_and_auto_compact() {
    // Only 100 tokens are uncached, but 850 more were served from the cache:
    // the context is 950/1000 = 95%, past the 80% threshold. Measuring
    // `input_tokens` alone (10%) would silently never compact.
    let provider = MockProvider::new(vec![
        text_response_with_cache("task done", 100, 850, 0),
        text_response_with_usage("summary of the session", 50, 20),
    ]);
    provider.set_context_window(1000);
    let db = temp_path("io-test", ".db");
    let agent = make_auto_compact_agent(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    let consumer = tokio::spawn(async move {
        let mut context_tokens = None;
        let mut compacted = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Usage { input_tokens, .. } => context_tokens = Some(input_tokens),
                AgentEvent::AutoCompact { .. } => compacted = true,
                _ => {}
            }
        }
        (context_tokens, compacted)
    });

    agent.run_turn_streaming("do the task", tx).await.unwrap();
    let (context_tokens, compacted) = consumer.await.unwrap();
    assert_eq!(
        context_tokens,
        Some(950),
        "the Usage event reports the full context, cache reads included"
    );
    assert!(
        compacted,
        "auto-compact must see the cache-inclusive context"
    );
}

#[tokio::test]
async fn turn_usage_records_cache_traffic_separately_from_input() {
    let provider = MockProvider::new(vec![text_response_with_cache("ok", 40, 900, 60)]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(provider, PermissionChecker::new("allow"), db.clone(), None);

    agent.run_turn("hi").await.unwrap();

    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    let usage = session.turns[0].usage.as_ref().unwrap();
    assert_eq!(usage.input_tokens, 40, "input_tokens stays uncached-only");
    assert_eq!(usage.cache_read_tokens, 900);
    assert_eq!(usage.cache_write_tokens, 60);
}

// ── Hooks ─────────────────────────────────────────────────────────────────────

fn hook(matcher: Option<&str>, command: &str) -> HookEntry {
    HookEntry {
        matcher: matcher.map(str::to_string),
        command: command.to_string(),
        timeout_ms: None,
    }
}

fn hooks(config: HooksConfig) -> Hooks {
    Hooks::new(config, std::env::temp_dir())
}

/// The first tool result the model was shown in `request`, if any.
fn tool_result_text(request: &CompletionRequest) -> Option<(String, Option<bool>)> {
    request.messages.iter().rev().find_map(|m| {
        m.content.iter().find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
    })
}

#[tokio::test]
async fn pre_tool_use_hook_blocks_the_tool_even_when_permissions_allow_it() {
    let target = temp_path("io-test-hook-write", ".txt");
    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "x"}),
        ),
        text_response("ok"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow")).with_hooks(hooks(
        HooksConfig {
            pre_tool_use: vec![hook(Some("write"), "echo 'writes are frozen' >&2; exit 2")],
            ..Default::default()
        },
    ));

    agent.run_turn("write the file").await.unwrap();

    assert!(!target.exists(), "a vetoed write must not run");
    let (content, is_error) = tool_result_text(&provider.recorded_requests()[1]).unwrap();
    assert_eq!(
        content, "blocked by pre_tool_use hook: writes are frozen",
        "the model sees the hook's reason"
    );
    assert_eq!(is_error, Some(true));
}

#[tokio::test]
async fn pre_tool_use_hook_cannot_grant_permission() {
    // Prompt mode with no way to ask: the write is denied. A hook that exits
    // 0 ("fine by me") must not turn that into an allow.
    let target = temp_path("io-test-hook-write", ".txt");
    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "x"}),
        ),
        text_response("ok"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("prompt")).with_hooks(hooks(
        HooksConfig {
            pre_tool_use: vec![hook(None, "exit 0")],
            ..Default::default()
        },
    ));

    agent.run_turn("write the file").await.unwrap();

    assert!(!target.exists());
    let (content, _) = tool_result_text(&provider.recorded_requests()[1]).unwrap();
    assert!(content.contains("no prompt is available"), "got: {content}");
}

#[tokio::test]
async fn pre_tool_use_hook_that_fails_open_lets_the_tool_run() {
    // Only exit 2 blocks; a hook that crashes (exit 1) is logged and ignored.
    let file = temp_path("io-test-hook-read", ".txt");
    std::fs::write(&file, "readable").unwrap();
    let provider = MockProvider::new(vec![
        tool_response(
            "read",
            serde_json::json!({"file_path": file.to_str().unwrap()}),
        ),
        text_response("done"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow")).with_hooks(hooks(
        HooksConfig {
            pre_tool_use: vec![hook(None, "exit 1")],
            ..Default::default()
        },
    ));

    agent.run_turn("read it").await.unwrap();

    let (content, is_error) = tool_result_text(&provider.recorded_requests()[1]).unwrap();
    assert!(content.contains("readable"));
    assert_eq!(is_error, None);
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn post_tool_use_hook_feedback_is_shown_to_the_model() {
    let file = temp_path("io-test-hook-read", ".txt");
    std::fs::write(&file, "file body").unwrap();
    let provider = MockProvider::new(vec![
        tool_response(
            "read",
            serde_json::json!({"file_path": file.to_str().unwrap()}),
        ),
        text_response("done"),
    ]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
        None,
    )
    .with_hooks(hooks(HooksConfig {
        post_tool_use: vec![
            hook(Some("read"), "echo 'lint: all good'"),
            // Wrong tool — must not run.
            hook(Some("write"), "echo 'wrong tool'"),
        ],
        ..Default::default()
    }));

    agent.run_turn("read it").await.unwrap();

    let (content, _) = tool_result_text(&provider.recorded_requests()[1]).unwrap();
    assert!(content.contains("file body"), "original output is kept");
    assert!(
        content.ends_with("[post_tool_use hook]\nlint: all good"),
        "feedback is appended, got: {content}"
    );
    assert!(!content.contains("wrong tool"));

    // The saved record matches what the model saw.
    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert!(session.turns[0].tool_calls[0]
        .output
        .contains("lint: all good"));
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn post_tool_use_hook_does_not_run_for_blocked_or_denied_calls() {
    let marker = temp_path("io-test-hook-post-marker", ".txt");
    let target = temp_path("io-test-hook-write", ".txt");
    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "x"}),
        ),
        text_response("ok"),
    ]);
    let agent =
        make_agent(provider, PermissionChecker::new("allow")).with_hooks(hooks(HooksConfig {
            pre_tool_use: vec![hook(None, "exit 2")],
            post_tool_use: vec![hook(None, &format!("touch {}", marker.display()))],
            ..Default::default()
        }));

    agent.run_turn("write the file").await.unwrap();

    assert!(
        !marker.exists(),
        "post hook must not fire for a call that never ran"
    );
}

#[tokio::test]
async fn user_prompt_submit_hook_can_reject_the_prompt() {
    let provider = MockProvider::new(vec![text_response("should never be asked")]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
        None,
    )
    .with_hooks(hooks(HooksConfig {
        user_prompt_submit: vec![hook(None, "echo 'no secrets please' >&2; exit 2")],
        ..Default::default()
    }));

    let err = agent.run_turn("my password is hunter2").await.unwrap_err();
    let blocked = err.downcast_ref::<PromptBlocked>().expect("typed error");
    assert_eq!(blocked.0, "no secrets please");

    assert!(
        provider.recorded_requests().is_empty(),
        "a rejected prompt never reaches the model"
    );
    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await);
    assert!(
        session.map(|s| s.turns.is_empty()).unwrap_or(true),
        "a rejected prompt is not saved"
    );
}

#[tokio::test]
async fn user_prompt_submit_hook_context_reaches_the_model_but_not_the_saved_turn() {
    let provider = MockProvider::new(vec![text_response("ok")]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
        None,
    )
    .with_hooks(hooks(HooksConfig {
        user_prompt_submit: vec![hook(None, "echo 'current branch: main'")],
        ..Default::default()
    }));

    agent.run_turn("what branch am I on?").await.unwrap();

    let requests = provider.recorded_requests();
    let sent = requests[0].messages.last().unwrap();
    let sent_text = match &sent.content[0] {
        ContentBlock::Text { text } => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };
    assert!(sent_text.starts_with("what branch am I on?"));
    assert!(sent_text.contains("<hook-context>\ncurrent branch: main\n</hook-context>"));

    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert_eq!(
        session.turns[0].user_message, "what branch am I on?",
        "the saved turn keeps the prompt as typed"
    );
}

#[tokio::test]
async fn stop_hook_runs_after_a_completed_turn_only() {
    let marker = temp_path("io-test-hook-stop", ".txt");
    let stop_config = HooksConfig {
        stop: vec![hook(None, &format!("cat > {}", marker.display()))],
        ..Default::default()
    };

    // A failing turn (provider has no scripted response) must not fire it.
    let failing = make_agent(MockProvider::new(vec![]), PermissionChecker::new("allow"))
        .with_hooks(hooks(stop_config.clone()));
    assert!(failing.run_turn("hi").await.is_err());
    assert!(
        !marker.exists(),
        "stop hook must not fire for a failed turn"
    );

    let provider = MockProvider::new(vec![text_response("all done")]);
    let agent =
        make_agent(provider, PermissionChecker::new("allow")).with_hooks(hooks(stop_config));
    agent.run_turn("hi").await.unwrap();

    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
    assert_eq!(payload["hook_event_name"], "Stop");
    assert_eq!(payload["response"], "all done");
    std::fs::remove_file(&marker).ok();
}

#[tokio::test]
async fn sub_agents_inherit_pre_tool_use_hooks() {
    // The guard must hold when the model delegates the work: a sub-agent's
    // `read` is vetoed by the parent's hook.
    let file = temp_path("io-test-hook-sub-read", ".txt");
    std::fs::write(&file, "secret contents").unwrap();
    let hook_config = HooksConfig {
        pre_tool_use: vec![hook(Some("read"), "echo 'reads are frozen' >&2; exit 2")],
        // Prompt/stop hooks are for the user's turn — not inherited, so this
        // one must not fire for the sub-agent's task prompt.
        user_prompt_submit: vec![hook(None, "echo 'prompt hook ran' >&2; exit 2")],
        ..Default::default()
    };
    let hooks = hooks(hook_config);

    let provider = MockProvider::new(vec![
        tool_response(
            "spawn_agent",
            serde_json::json!({"agent_id": "explore", "task": "read the file"}),
        ),
        tool_response(
            "read",
            serde_json::json!({"file_path": file.to_str().unwrap()}),
        ),
        text_response("sub-agent done"),
        text_response("parent done"),
    ]);
    let permissions = Arc::new(PermissionChecker::new("allow"));
    let mut tools = default_registry();
    tools.register(Box::new(
        io_runtime::SpawnAgentTool::new(
            provider.clone(),
            "mock-model".to_string(),
            1024,
            permissions.clone(),
            None,
        )
        .with_hooks(&hooks),
    ));
    let memory = SessionStore::with_path(temp_path("io-test", ".db")).expect("session store");
    // The parent itself has no user_prompt_submit hook (only the sub-agent
    // config carries one, and it is stripped on inheritance).
    let parent_hooks = Hooks::new(
        HooksConfig {
            user_prompt_submit: vec![],
            ..HooksConfig::default()
        },
        std::env::temp_dir(),
    );
    let agent = Agent::new(
        provider.clone(),
        tools,
        memory,
        permissions,
        "You are a test agent.".to_string(),
        None,
        None,
        "mock-model".to_string(),
        1024,
        false,
        None,
    )
    .with_hooks(parent_hooks);

    let reply = agent.run_turn("delegate this").await.unwrap();
    assert_eq!(reply, "parent done");

    let requests = provider.recorded_requests();
    let sub_agent_saw_veto = requests.iter().any(
        |r| matches!(tool_result_text(r), Some((c, Some(true))) if c.contains("reads are frozen")),
    );
    assert!(
        sub_agent_saw_veto,
        "sub-agent's read must be vetoed by the inherited hook"
    );
    let leaked = requests
        .iter()
        .flat_map(|r| r.messages.iter())
        .flat_map(|m| m.content.iter())
        .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("secret contents")));
    assert!(!leaked, "file contents must never reach the model");
    std::fs::remove_file(&file).ok();
}

// ── Edit safety ───────────────────────────────────────────────────────────────

/// The result text and error flag of the tool result at the end of `request`.
fn last_tool_result(request: &CompletionRequest) -> (String, Option<bool>) {
    request
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .expect("last message carries a tool result")
}

#[tokio::test]
async fn editing_an_unread_file_is_refused_until_it_has_been_read() {
    let file = temp_path("io-test-unread", ".txt");
    std::fs::write(&file, "alpha").unwrap();
    let p = file.to_str().unwrap();
    let edit = serde_json::json!({"path": p, "old_string": "alpha", "new_string": "beta"});

    let provider = MockProvider::new(vec![
        tool_response("edit", edit.clone()),
        tool_response("read", serde_json::json!({"file_path": p})),
        tool_response("edit", edit),
        text_response("done"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent.run_turn("change alpha to beta").await.unwrap();

    let requests = provider.recorded_requests();
    let (refusal, is_error) = last_tool_result(&requests[1]);
    assert_eq!(is_error, Some(true));
    assert!(refusal.contains("has not been read yet"), "{refusal}");
    let (_, is_error) = last_tool_result(&requests[3]);
    assert_eq!(is_error, None, "the edit after the read succeeds");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "beta");
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn a_file_changed_behind_the_agents_back_must_be_read_again() {
    let file = temp_path("io-test-stale", ".txt");
    std::fs::write(&file, "v1").unwrap();
    let p = file.to_str().unwrap();
    let edit = serde_json::json!({"path": p, "old_string": "v1", "new_string": "mine"});

    let provider = MockProvider::new(vec![
        tool_response("read", serde_json::json!({"file_path": p})),
        text_response("read it"),
        tool_response("edit", edit),
        text_response("tried"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent.run_turn("look at it").await.unwrap();
    // The user (or a formatter) edits the file between turns.
    std::fs::write(&file, "v1 plus the user's change").unwrap();
    agent.run_turn("now edit it").await.unwrap();

    let requests = provider.recorded_requests();
    let (refusal, is_error) = last_tool_result(&requests[3]);
    assert_eq!(is_error, Some(true));
    assert!(refusal.contains("modified since"), "{refusal}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "v1 plus the user's change",
        "the user's change must not be clobbered"
    );
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn multi_edit_changes_are_rewound_like_edits() {
    let file = temp_path("io-test-multi-rewind", ".txt");
    std::fs::write(&file, "one two").unwrap();
    let p = file.to_str().unwrap();
    let provider = MockProvider::new(vec![
        tool_response("read", serde_json::json!({"file_path": p})),
        tool_response(
            "multi_edit",
            serde_json::json!({"path": p, "edits": [
                {"old_string": "one", "new_string": "1"},
                {"old_string": "two", "new_string": "2"}
            ]}),
        ),
        text_response("edited"),
    ]);
    let agent = make_agent(provider, PermissionChecker::new("allow"));
    agent.run_turn("renumber").await.unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "1 2");

    let result = agent.rewind_to(0).await.unwrap();
    assert_eq!(result.files_restored.len(), 1);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one two");
    std::fs::remove_file(&file).ok();
}

// ── Todo list ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn todo_write_runs_without_prompting_even_in_strict_mode() {
    let provider = MockProvider::new(vec![
        tool_response(
            "todo_write",
            serde_json::json!({"todos": [
                {"content": "Write code", "status": "in_progress", "activeForm": "Writing code"},
                {"content": "Test it", "status": "pending"}
            ]}),
        ),
        text_response("planned"),
    ]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(
        provider.clone(),
        PermissionChecker::new("prompt"),
        db.clone(),
        None,
    );
    agent.run_turn("do the thing").await.unwrap();

    let (result, is_error) = last_tool_result(&provider.recorded_requests()[1]);
    assert_eq!(is_error, None);
    assert!(
        result.contains("1 pending, 1 in progress, 0 completed"),
        "{result}"
    );

    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert_eq!(session.turns[0].tool_calls[0].tool_name, "todo_write");
}

// ── Plan mode ─────────────────────────────────────────────────────────────────

fn system_text_of(request: &CompletionRequest) -> String {
    request.system_text().unwrap_or_default()
}

#[tokio::test]
async fn plan_mode_blocks_edits_until_the_user_approves_the_plan() {
    let target = temp_path("io-test-plan", ".txt");
    let write = serde_json::json!({"path": target.to_str().unwrap(), "content": "built"});
    let provider = MockProvider::new(vec![
        tool_response("write", write.clone()),
        tool_response(
            "exit_plan_mode",
            serde_json::json!({"plan": "1. write the file"}),
        ),
        tool_response("write", write),
        text_response("implemented"),
    ]);
    // Even with every permission granted, plan mode is read-only.
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent.set_interaction_mode(InteractionMode::Plan);

    let asked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    agent.set_prompt_fn(Arc::new(move |name, input| {
        log.lock().unwrap().push(format!("{name}: {input}"));
        // The second option in the plan dialog: approve and accept edits.
        PermissionReply::AllowSession
    }));

    let reply = agent.run_turn("add the file").await.unwrap();
    assert_eq!(reply, "implemented");

    let requests = provider.recorded_requests();
    // The first write was refused with a pointer to exit_plan_mode.
    let (refusal, is_error) = last_tool_result(&requests[1]);
    assert_eq!(is_error, Some(true));
    assert!(refusal.contains("plan mode is active"), "{refusal}");
    assert!(refusal.contains("exit_plan_mode"));
    // Only the plan itself was put to the user — never the blocked write.
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].starts_with("exit_plan_mode") && asked[0].contains("1. write the file"));
    // Approval ended plan mode and accepted edits, so the second write ran.
    assert_eq!(agent.interaction_mode(), InteractionMode::AcceptEdits);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "built");
    // The model was told it is in plan mode while it was, and not after.
    assert!(system_text_of(&requests[0]).contains("Plan mode is active"));
    assert!(system_text_of(&requests[1]).contains("Plan mode is active"));
    assert!(!system_text_of(&requests[2]).contains("Plan mode is active"));
    let (approved, _) = last_tool_result(&requests[2]);
    assert!(approved.contains("approved your plan"), "{approved}");
    std::fs::remove_file(&target).ok();
}

#[tokio::test]
async fn approving_the_plan_once_returns_to_the_default_mode() {
    let provider = MockProvider::new(vec![
        tool_response("exit_plan_mode", serde_json::json!({"plan": "do it"})),
        text_response("ok"),
    ]);
    let agent = make_agent(provider, PermissionChecker::new("prompt"));
    agent.set_interaction_mode(InteractionMode::Plan);
    agent.set_prompt_fn(Arc::new(|_, _| PermissionReply::AllowOnce));
    agent.run_turn("plan it").await.unwrap();
    assert_eq!(agent.interaction_mode(), InteractionMode::Default);
}

#[tokio::test]
async fn a_rejected_plan_keeps_plan_mode_on_and_tells_the_model() {
    let target = temp_path("io-test-plan-rejected", ".txt");
    let provider = MockProvider::new(vec![
        tool_response("exit_plan_mode", serde_json::json!({"plan": "risky plan"})),
        tool_response(
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "x"}),
        ),
        text_response("revising"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent.set_interaction_mode(InteractionMode::Plan);
    agent.set_prompt_fn(Arc::new(|_, _| PermissionReply::Deny));
    agent.run_turn("plan it").await.unwrap();

    let requests = provider.recorded_requests();
    let (rejection, is_error) = last_tool_result(&requests[1]);
    assert_eq!(is_error, Some(true));
    assert!(
        rejection.contains("did not approve the plan"),
        "{rejection}"
    );
    assert_eq!(agent.interaction_mode(), InteractionMode::Plan);
    assert!(!target.exists(), "no edits while the plan is unapproved");
}

#[tokio::test]
async fn exit_plan_mode_outside_plan_mode_is_a_harmless_error() {
    let provider = MockProvider::new(vec![
        tool_response("exit_plan_mode", serde_json::json!({"plan": "unneeded"})),
        text_response("carrying on"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("prompt"));
    // No prompt_fn: if the user were asked, the call would fail differently.
    agent.run_turn("go").await.unwrap();
    let (result, is_error) = last_tool_result(&provider.recorded_requests()[1]);
    assert_eq!(is_error, Some(true));
    assert!(result.contains("plan mode is not active"), "{result}");
    assert_eq!(agent.interaction_mode(), InteractionMode::Default);
}

#[tokio::test]
async fn plan_mode_still_allows_reading_and_read_only_commands() {
    let file = temp_path("io-test-plan-read", ".txt");
    std::fs::write(&file, "research material").unwrap();
    let provider = MockProvider::new(vec![
        tool_response(
            "read",
            serde_json::json!({"file_path": file.to_str().unwrap()}),
        ),
        tool_response("bash", serde_json::json!({"command": "ls"})),
        tool_response(
            "bash",
            serde_json::json!({"command": "rm -rf /tmp/io-never"}),
        ),
        text_response("researched"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent.set_interaction_mode(InteractionMode::Plan);
    agent.run_turn("investigate").await.unwrap();

    let requests = provider.recorded_requests();
    let (read, e1) = last_tool_result(&requests[1]);
    assert!(read.contains("research material") && e1.is_none());
    let (_, e2) = last_tool_result(&requests[2]);
    assert_eq!(e2, None, "ls is read-only");
    let (blocked, e3) = last_tool_result(&requests[3]);
    assert_eq!(e3, Some(true));
    assert!(blocked.contains("plan mode is active"), "{blocked}");
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn accept_edits_mode_writes_without_asking() {
    let target = temp_path("io-test-accept", ".txt");
    let provider = MockProvider::new(vec![
        tool_response(
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "accepted"}),
        ),
        text_response("written"),
    ]);
    // Strict prompt mode with no way to ask: only accept-edits lets this run.
    let agent = make_agent(provider, PermissionChecker::new("prompt"));
    agent.set_interaction_mode(InteractionMode::AcceptEdits);
    agent.run_turn("write it").await.unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "accepted");
    std::fs::remove_file(&target).ok();
}

#[tokio::test]
async fn the_mode_cycles_and_is_shared_with_the_permission_checker() {
    let permissions = Arc::new(PermissionChecker::new("prompt"));
    let memory = SessionStore::with_path(temp_path("io-test", ".db")).unwrap();
    let agent = Agent::new(
        MockProvider::new(vec![]),
        default_registry(),
        memory,
        permissions.clone(),
        "x".to_string(),
        None,
        None,
        "mock-model".to_string(),
        1024,
        false,
        None,
    );
    assert_eq!(agent.cycle_interaction_mode(), InteractionMode::AcceptEdits);
    assert_eq!(permissions.interaction_mode(), InteractionMode::AcceptEdits);
    assert_eq!(agent.cycle_interaction_mode(), InteractionMode::Plan);
    assert_eq!(agent.cycle_interaction_mode(), InteractionMode::Default);
}

// ── Images ────────────────────────────────────────────────────────────────────

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";

#[tokio::test]
async fn images_a_tool_returns_follow_the_tool_results_to_the_model() {
    let file = temp_path("io-test-shot", ".png");
    std::fs::write(&file, PNG_BYTES).unwrap();
    let provider = MockProvider::new(vec![
        tool_response(
            "read",
            serde_json::json!({"file_path": file.to_str().unwrap()}),
        ),
        text_response("a 1x1 pixel"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent.run_turn("what is in the screenshot?").await.unwrap();

    let requests = provider.recorded_requests();
    let last = requests[1].messages.last().unwrap();
    // Tool results first (providers require it), then the image.
    assert!(matches!(last.content[0], ContentBlock::ToolResult { .. }));
    match &last.content[1] {
        ContentBlock::Image { media_type, data } => {
            assert_eq!(media_type, "image/png");
            assert!(!data.is_empty());
        }
        other => panic!("expected an image block, got {other:?}"),
    }
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn attached_images_lead_the_user_message_and_are_noted_in_history() {
    let provider = MockProvider::new(vec![text_response("I see it")]);
    let db = temp_path("io-test", ".db");
    let agent = make_agent_at(
        provider.clone(),
        PermissionChecker::new("allow"),
        db.clone(),
        None,
    );
    let image = ToolImage {
        media_type: "image/png".to_string(),
        data: "QUJD".to_string(),
    };
    agent
        .run_turn_with_images("describe this", vec![image])
        .await
        .unwrap();

    let requests = provider.recorded_requests();
    let user = requests[0].messages.last().unwrap();
    assert!(matches!(&user.content[0], ContentBlock::Image { data, .. } if data == "QUJD"));
    assert!(
        matches!(&user.content[1], ContentBlock::Text { text } if text.contains("describe this"))
    );

    // Images aren't persisted, but the saved turn says they existed.
    let store = SessionStore::with_path(db).unwrap();
    let session = store.load_session(agent.session_id().await).unwrap();
    assert!(session.turns[0]
        .user_message
        .contains("1 image(s) were attached"));
}

// ── Extended thinking ─────────────────────────────────────────────────────────

fn thinking(enabled: bool, budget_tokens: u32) -> ThinkingConfig {
    ThinkingConfig {
        enabled,
        budget_tokens,
        keywords: true,
    }
}

#[tokio::test]
async fn thinking_is_off_by_default_and_requests_carry_no_budget() {
    let provider = MockProvider::new(vec![text_response("hi")]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent.run_turn("hello there").await.unwrap();
    let req = &provider.recorded_requests()[0];
    assert_eq!(req.thinking, None);
    assert_eq!(req.max_tokens, Some(1024));
}

#[tokio::test]
async fn enabled_thinking_sends_the_budget_and_leaves_room_for_the_answer() {
    let provider = MockProvider::new(vec![text_response("hi"), text_response("hi again")]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"))
        .with_thinking(thinking(true, 8_000));
    agent.run_turn("hello").await.unwrap();
    let req = &provider.recorded_requests()[0];
    assert_eq!(req.thinking.map(|t| t.budget_tokens), Some(8_000));
    assert_eq!(req.max_tokens, Some(8_000 + 4_096));

    // Switching it off mid-session takes effect on the next turn.
    agent.set_thinking(thinking(false, 8_000));
    agent.run_turn("hello again").await.unwrap();
    let req = &provider.recorded_requests()[1];
    assert_eq!(req.thinking, None);
    assert_eq!(req.max_tokens, Some(1024));
}

#[tokio::test]
async fn keywords_turn_thinking_on_for_one_turn_only() {
    let provider = MockProvider::new(vec![
        text_response("a"),
        text_response("b"),
        text_response("c"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent
        .run_turn("ultrathink about the cache design")
        .await
        .unwrap();
    agent.run_turn("I think that is fine").await.unwrap();
    agent.run_turn("think hard about naming").await.unwrap();

    let reqs = provider.recorded_requests();
    assert_eq!(reqs[0].thinking.map(|t| t.budget_tokens), Some(31_999));
    assert_eq!(reqs[0].max_tokens, Some(31_999 + 4_096));
    assert_eq!(
        reqs[1].thinking, None,
        "a casual 'I think' is not a keyword"
    );
    assert_eq!(reqs[2].thinking.map(|t| t.budget_tokens), Some(10_000));
}

#[tokio::test]
async fn keywords_can_be_disabled() {
    let provider = MockProvider::new(vec![text_response("a")]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow")).with_thinking(
        ThinkingConfig {
            keywords: false,
            ..thinking(false, 8_000)
        },
    );
    agent.run_turn("ultrathink").await.unwrap();
    assert_eq!(provider.recorded_requests()[0].thinking, None);
}

#[tokio::test]
async fn signed_reasoning_is_replayed_during_the_tool_loop_and_shown_live() {
    let file = temp_path("io-test-thinking", ".txt");
    std::fs::write(&file, "data").unwrap();
    let provider = MockProvider::new(vec![
        CompletionResponse {
            content: vec![
                // A display delta, then the finished signed block, then the call.
                ContentBlock::Thinking {
                    thinking: "weighing it".to_string(),
                    signature: None,
                },
                ContentBlock::Thinking {
                    thinking: "weighing it up".to_string(),
                    signature: Some("sig-1".to_string()),
                },
                ContentBlock::RedactedThinking {
                    data: "opaque".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": file.to_str().unwrap()}),
                },
            ],
            stop_reason: Some("tool_use".to_string()),
            usage: None,
        },
        text_response("done"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"))
        .with_thinking(thinking(true, 8_000));

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(32);
    let consumer = tokio::spawn(async move {
        let mut shown = String::new();
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::Thinking(delta) = ev {
                shown.push_str(&delta);
            }
        }
        shown
    });
    agent.run_turn_streaming("read it", tx).await.unwrap();
    assert_eq!(
        consumer.await.unwrap(),
        "weighing it",
        "only deltas are shown"
    );

    // The follow-up request carries the signed + redacted blocks first, in
    // the assistant message, ahead of the tool call.
    let requests = provider.recorded_requests();
    let assistant = requests[1]
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, io_runtime::provider::Role::Assistant))
        .unwrap();
    assert!(matches!(
        &assistant.content[0],
        ContentBlock::Thinking { thinking, signature: Some(sig) }
            if thinking == "weighing it up" && sig == "sig-1"
    ));
    assert!(matches!(
        &assistant.content[1],
        ContentBlock::RedactedThinking { .. }
    ));
    assert!(matches!(
        &assistant.content[2],
        ContentBlock::ToolUse { .. }
    ));
    assert_eq!(
        assistant.content.len(),
        3,
        "the unsigned delta is not replayed"
    );
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn blocking_turns_drop_unsigned_reasoning_from_history() {
    let file = temp_path("io-test-thinking-blocking", ".txt");
    std::fs::write(&file, "data").unwrap();
    let provider = MockProvider::new(vec![
        CompletionResponse {
            content: vec![
                ContentBlock::Thinking {
                    thinking: "unsigned musings".to_string(),
                    signature: None,
                },
                ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": file.to_str().unwrap()}),
                },
            ],
            stop_reason: Some("tool_use".to_string()),
            usage: None,
        },
        text_response("done"),
    ]);
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"));
    agent.run_turn("read it").await.unwrap();
    let requests = provider.recorded_requests();
    let replayed = requests[1]
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .any(|b| matches!(b, ContentBlock::Thinking { .. }));
    assert!(!replayed);
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn a_model_without_thinking_support_gets_no_budget_and_no_extra_tokens() {
    let provider = MockProvider::new(vec![text_response("fine")]);
    provider
        .thinking_capable
        .store(false, std::sync::atomic::Ordering::Relaxed);
    // Both the setting and a keyword are ignored: raising max_tokens for a
    // model that can't think could make its API reject the request.
    let agent = make_agent(provider.clone(), PermissionChecker::new("allow"))
        .with_thinking(thinking(true, 8_000));
    assert!(!agent.supports_thinking());
    agent.run_turn("ultrathink about this").await.unwrap();
    let req = &provider.recorded_requests()[0];
    assert_eq!(req.thinking, None);
    assert_eq!(req.max_tokens, Some(1024));
}
