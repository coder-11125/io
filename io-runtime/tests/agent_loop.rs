//! Integration tests for the agent loop, driven by a scripted mock provider.
//! These exercise the full turn lifecycle: message building, tool execution,
//! permission resolution, streaming events, usage tracking, and session
//! persistence — everything except a real LLM HTTP call.

use async_trait::async_trait;
use io_runtime::agent::{Agent, AgentEvent, PermissionReply};
use io_runtime::memory::SessionStore;
use io_runtime::provider::{
    CompletionModel, CompletionRequest, CompletionResponse, ContentBlock, StreamEvent, Usage,
};
use io_runtime::sandbox::PermissionChecker;
use io_runtime::tools::default_registry;
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
}

impl MockProvider {
    fn new(responses: Vec<CompletionResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn recorded_requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
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
        session_id,
        "mock-model".to_string(),
        1024,
        false,
    )
}

fn make_agent(provider: Arc<MockProvider>, permissions: PermissionChecker) -> Agent {
    make_agent_at(provider, permissions, temp_path("io-test", ".db"), None)
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
        1,
        "AllowSession should suppress the second prompt"
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
    )));
    let memory = SessionStore::with_path(temp_path("io-test", ".db")).expect("session store");
    let agent = Agent::new(
        provider.clone(),
        tools,
        memory,
        permissions,
        "You are a test agent.".to_string(),
        None,
        "mock-model".to_string(),
        1024,
        false,
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
