//! End-to-end tests for the MCP client against a real (Python) server process
//! over every transport, and through the agent loop.

use async_trait::async_trait;
use io_runtime::agent::{Agent, PermissionReply};
use io_runtime::mcp::{McpManager, McpServerConfig, Scope, ServerEntry, ServerStatus, Settings};
use io_runtime::memory::SessionStore;
use io_runtime::provider::{
    CompletionModel, CompletionRequest, CompletionResponse, ContentBlock, StreamEvent, Usage,
};
use io_runtime::sandbox::PermissionChecker;
use io_runtime::tools::{default_registry, Tool, ToolInput};
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn script() -> String {
    format!(
        "{}/tests/fixtures/mock_mcp_server.py",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn settings() -> Settings {
    Settings {
        startup_timeout: Duration::from_secs(20),
        tool_timeout: Duration::from_secs(10),
    }
}

fn stdio_entry(name: &str, extra_args: &[&str], env: &[(&str, &str)]) -> ServerEntry {
    let mut args = vec![script(), "stdio".to_string()];
    args.extend(extra_args.iter().map(|a| a.to_string()));
    ServerEntry {
        name: name.to_string(),
        scope: Scope::User,
        config: McpServerConfig {
            command: Some("python3".to_string()),
            args,
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            ..Default::default()
        },
    }
}

/// A server process on an ephemeral port; killed when dropped.
struct Remote {
    child: Child,
    port: u16,
}

impl Remote {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new("python3")
            .arg(script())
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required for the MCP tests");
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line
            .trim()
            .strip_prefix("PORT ")
            .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
            .parse()
            .unwrap();
        Remote { child, port }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http_entry(name: &str, transport: &str, url: String, headers: &[(&str, &str)]) -> ServerEntry {
    ServerEntry {
        name: name.to_string(),
        scope: Scope::User,
        config: McpServerConfig {
            transport: Some(transport.to_string()),
            url: Some(url),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        },
    }
}

async fn start(entries: Vec<ServerEntry>) -> McpManager {
    McpManager::start(entries, vec![], settings()).await
}

fn tool(manager: &McpManager, name: &str) -> Box<dyn Tool> {
    manager
        .tools()
        .into_iter()
        .find(|t| t.name() == name)
        .unwrap_or_else(|| panic!("no tool {name}; have {:?}", names(manager)))
}

fn names(manager: &McpManager) -> Vec<String> {
    manager
        .tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect()
}

async fn run(tool: &dyn Tool, args: serde_json::Value) -> io_runtime::tools::ToolOutput {
    tool.execute(ToolInput {
        name: tool.name().to_string(),
        args: args.as_object().unwrap().clone().into_iter().collect(),
    })
    .await
}

/// The checks every transport must pass.
async fn exercise(manager: &McpManager, server: &str) {
    assert_eq!(
        manager.states()[0].status,
        ServerStatus::Connected { tools: 7 },
        "both pages of tools are listed: {:?}",
        manager.states()
    );
    let prefix = format!("mcp__{server}__");
    let echo = tool(manager, &format!("{prefix}echo"));
    assert!(echo.description().contains("Echo the text back"));
    assert_eq!(echo.input_schema()["required"][0], "text");

    let out = run(echo.as_ref(), serde_json::json!({"text": "hello"})).await;
    assert!(out.success, "{}", out.data);
    assert_eq!(out.data, "echo: hello");

    let add = tool(manager, &format!("{prefix}add"));
    assert_eq!(
        run(add.as_ref(), serde_json::json!({"a": 2, "b": 3}))
            .await
            .data,
        "5"
    );

    let fail = tool(manager, &format!("{prefix}fail"));
    let out = run(fail.as_ref(), serde_json::json!({})).await;
    assert!(!out.success);
    assert_eq!(out.data, "this tool always fails");

    let image = tool(manager, &format!("{prefix}image"));
    let out = run(image.as_ref(), serde_json::json!({})).await;
    assert!(out.success);
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].media_type, "image/png");

    assert_eq!(manager.read_only_tool_names(), [format!("{prefix}echo")]);
}

#[tokio::test]
async fn stdio_servers_work_end_to_end() {
    // The mock prints a non-JSON line on startup, which must be tolerated.
    let manager = start(vec![stdio_entry("mock", &[], &[])]).await;
    exercise(&manager, "mock").await;
}

#[tokio::test]
async fn stdio_servers_receive_their_configured_environment() {
    let manager = start(vec![stdio_entry(
        "mock",
        &[],
        &[("MOCK_SECRET", "hunter2")],
    )])
    .await;
    let env = tool(&manager, "mcp__mock__env");
    let out = run(env.as_ref(), serde_json::json!({"name": "MOCK_SECRET"})).await;
    assert_eq!(out.data, "hunter2");
}

#[tokio::test]
async fn streamable_http_with_json_replies_works() {
    let server = Remote::start(&["http"]);
    let manager = start(vec![http_entry("web", "http", server.url("/mcp"), &[])]).await;
    exercise(&manager, "web").await;
}

#[tokio::test]
async fn streamable_http_with_event_stream_replies_works() {
    let server = Remote::start(&["http", "--sse-replies"]);
    let manager = start(vec![http_entry("web", "http", server.url("/mcp"), &[])]).await;
    exercise(&manager, "web").await;
}

#[tokio::test]
async fn http_headers_are_sent_and_a_missing_one_is_reported() {
    let server = Remote::start(&["http", "--require-header"]);
    let denied = start(vec![http_entry("web", "http", server.url("/mcp"), &[])]).await;
    assert!(
        matches!(&denied.states()[0].status, ServerStatus::Failed(m) if m.contains("401")),
        "{:?}",
        denied.states()
    );
    let allowed = start(vec![http_entry(
        "web",
        "http",
        server.url("/mcp"),
        &[("X-Test", "yes")],
    )])
    .await;
    assert!(matches!(
        allowed.states()[0].status,
        ServerStatus::Connected { .. }
    ));
}

#[tokio::test]
async fn the_legacy_sse_transport_works() {
    let server = Remote::start(&["sse"]);
    let manager = start(vec![http_entry("legacy", "sse", server.url("/sse"), &[])]).await;
    exercise(&manager, "legacy").await;
}

#[tokio::test]
async fn several_servers_connect_together_with_their_own_prefixes() {
    let server = Remote::start(&["http"]);
    let manager = start(vec![
        stdio_entry("alpha", &[], &[]),
        http_entry("beta", "http", server.url("/mcp"), &[]),
    ])
    .await;
    let all = names(&manager);
    assert!(all.contains(&"mcp__alpha__echo".to_string()));
    assert!(all.contains(&"mcp__beta__echo".to_string()));
    assert_eq!(manager.tool_count(), 14);
}

#[tokio::test]
async fn a_slow_tool_times_out_without_hanging_the_agent() {
    let manager = McpManager::start(
        vec![stdio_entry("mock", &[], &[])],
        vec![],
        Settings {
            startup_timeout: Duration::from_secs(20),
            tool_timeout: Duration::from_millis(300),
        },
    )
    .await;
    let slow = tool(&manager, "mcp__mock__slow");
    let started = std::time::Instant::now();
    let out = run(slow.as_ref(), serde_json::json!({})).await;
    assert!(!out.success);
    assert!(
        out.data.contains("timed out") || out.data.contains("no response"),
        "{}",
        out.data
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    // The connection is still usable afterwards.
    let echo = tool(&manager, "mcp__mock__echo");
    assert!(
        run(echo.as_ref(), serde_json::json!({"text": "still here"}))
            .await
            .success
    );
}

#[tokio::test]
async fn a_server_that_dies_mid_call_is_an_error_not_a_hang() {
    let manager = start(vec![stdio_entry("mock", &[], &[])]).await;
    let crash = tool(&manager, "mcp__mock__crash");
    let out = run(crash.as_ref(), serde_json::json!({})).await;
    assert!(!out.success);
    assert!(out.data.contains("closed"), "{}", out.data);
    // Later calls fail fast too.
    let echo = tool(&manager, "mcp__mock__echo");
    let again = run(echo.as_ref(), serde_json::json!({"text": "x"})).await;
    assert!(!again.success);
}

#[tokio::test]
async fn startup_failures_explain_themselves() {
    let manager = start(vec![
        stdio_entry("early", &["exit-early"], &[]),
        ServerEntry {
            name: "missing".to_string(),
            scope: Scope::User,
            config: McpServerConfig {
                command: Some("/no/such/mcp-server".to_string()),
                ..Default::default()
            },
        },
        http_entry("nowhere", "http", "http://127.0.0.1:9/mcp".to_string(), &[]),
    ])
    .await;
    let status = |n: &str| {
        manager
            .states()
            .iter()
            .find(|s| s.name == n)
            .unwrap()
            .status
            .clone()
    };
    // `exit-early` is passed after "stdio", so the server runs in stdio mode
    // and the failure is on the other servers; check them individually.
    assert!(matches!(status("missing"), ServerStatus::Failed(m) if m.contains("failed to start")));
    assert!(matches!(status("nowhere"), ServerStatus::Failed(m) if m.contains("could not reach")));
    assert_eq!(
        manager.tool_count(),
        7,
        "the healthy server still contributes"
    );
}

#[tokio::test]
async fn a_server_that_exits_before_answering_reports_its_stderr() {
    let entry = ServerEntry {
        name: "early".to_string(),
        scope: Scope::User,
        config: McpServerConfig {
            command: Some("python3".to_string()),
            args: vec![script(), "exit-early".to_string()],
            ..Default::default()
        },
    };
    let manager = start(vec![entry]).await;
    match &manager.states()[0].status {
        ServerStatus::Failed(m) => assert!(m.contains("missing configuration"), "{m}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

// ── Through the agent ─────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Scripted {
    responses: Mutex<VecDeque<CompletionResponse>>,
    requests: Mutex<Vec<CompletionRequest>>,
}

#[async_trait]
impl CompletionModel for Scripted {
    fn provider_name(&self) -> &'static str {
        "scripted"
    }
    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("out of scripted responses"))
    }
    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        unimplemented!("these tests use blocking turns")
    }
}

fn tool_call(name: &str, input: serde_json::Value) -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::ToolUse {
            id: "call_1".into(),
            name: name.into(),
            input,
        }],
        stop_reason: Some("tool_use".into()),
        usage: Some(Usage::default()),
    }
}

fn say(text: &str) -> CompletionResponse {
    CompletionResponse {
        content: vec![ContentBlock::Text { text: text.into() }],
        stop_reason: Some("end_turn".into()),
        usage: Some(Usage::default()),
    }
}

fn agent_with(
    manager: &McpManager,
    provider: Arc<Scripted>,
    permissions: PermissionChecker,
) -> Agent {
    let mut tools = default_registry();
    for t in manager.tools() {
        tools.register(t);
    }
    permissions.mark_read_only(&manager.read_only_tool_names());
    let db = std::env::temp_dir().join(format!("io-mcp-{}.db", uuid::Uuid::new_v4()));
    Agent::new(
        provider,
        tools,
        SessionStore::with_path(db).unwrap(),
        Arc::new(permissions),
        "test".to_string(),
        None,
        None,
        "m".to_string(),
        1024,
        false,
        None,
    )
}

fn tool_result(provider: &Scripted, request_index: usize) -> (String, Option<bool>) {
    let reqs = provider.requests.lock().unwrap();
    reqs[request_index]
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
        .unwrap()
}

#[tokio::test]
async fn the_model_sees_mcp_tools_and_their_results() {
    let manager = start(vec![stdio_entry("mock", &[], &[])]).await;
    let provider = Arc::new(Scripted {
        responses: Mutex::new(
            vec![
                tool_call("mcp__mock__echo", serde_json::json!({"text": "via agent"})),
                say("done"),
            ]
            .into(),
        ),
        ..Default::default()
    });
    let agent = agent_with(&manager, provider.clone(), PermissionChecker::new("allow"));
    assert_eq!(agent.run_turn("echo something").await.unwrap(), "done");

    // The tool was offered with the server's schema.
    let reqs = provider.requests.lock().unwrap();
    let offered = reqs[0]
        .tools
        .iter()
        .find(|t| t.name == "mcp__mock__echo")
        .expect("mcp tool is offered to the model");
    assert_eq!(offered.input_schema["required"][0], "text");
    drop(reqs);
    assert_eq!(
        tool_result(&provider, 1),
        ("echo: via agent".to_string(), None)
    );
}

#[tokio::test]
async fn mcp_tools_prompt_by_default_and_honor_server_and_tool_rules() {
    let manager = start(vec![stdio_entry("mock", &[], &[])]).await;
    let call = || tool_call("mcp__mock__add", serde_json::json!({"a": 1, "b": 2}));

    // Default: strict and agent mode both ask.
    let provider = Arc::new(Scripted {
        responses: Mutex::new(vec![call(), say("ok")].into()),
        ..Default::default()
    });
    let agent = agent_with(&manager, provider.clone(), PermissionChecker::new("agent"));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    agent.set_prompt_fn(Arc::new(move |name, _| {
        log.lock().unwrap().push(name.to_string());
        PermissionReply::AllowOnce
    }));
    agent.run_turn("add").await.unwrap();
    assert_eq!(*asked.lock().unwrap(), ["mcp__mock__add"]);

    // A server-wide allow rule runs without asking; a tool-level deny wins.
    for (allow, deny, expect) in [
        ("mcp__mock", None, "3"),
        ("mcp__mock__*", None, "3"),
        ("mcp__mock__add", None, "3"),
        (
            "mcp__mock",
            Some("mcp__mock__add"),
            "tool mcp__mock__add not permitted",
        ),
    ] {
        let provider = Arc::new(Scripted {
            responses: Mutex::new(vec![call(), say("ok")].into()),
            ..Default::default()
        });
        let mut checker = PermissionChecker::new("agent");
        checker.add_allow(allow.to_string());
        if let Some(d) = deny {
            checker.add_deny(d.to_string());
        }
        let agent = agent_with(&manager, provider.clone(), checker);
        agent.run_turn("add").await.unwrap(); // no prompt_fn: asking would deny
        assert_eq!(
            tool_result(&provider, 1).0,
            expect,
            "allow={allow} deny={deny:?}"
        );
    }
}

#[tokio::test]
async fn plan_mode_allows_only_tools_the_server_marked_read_only() {
    let manager = start(vec![stdio_entry("mock", &[], &[])]).await;
    let provider = Arc::new(Scripted {
        responses: Mutex::new(
            vec![
                tool_call("mcp__mock__echo", serde_json::json!({"text": "research"})),
                tool_call("mcp__mock__add", serde_json::json!({"a": 1, "b": 1})),
                say("planned"),
            ]
            .into(),
        ),
        ..Default::default()
    });
    let agent = agent_with(&manager, provider.clone(), PermissionChecker::new("allow"));
    agent.set_interaction_mode(io_runtime::InteractionMode::Plan);
    agent.run_turn("investigate").await.unwrap();
    assert_eq!(
        tool_result(&provider, 1),
        ("echo: research".to_string(), None)
    );
    let (refused, err) = tool_result(&provider, 2);
    assert_eq!(err, Some(true));
    assert!(refused.contains("plan mode"), "{refused}");
}
