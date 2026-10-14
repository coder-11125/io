//! JSON-RPC transports for MCP: stdio, streamable HTTP, and the older SSE.
//!
//! Stdio and SSE are message-passing: requests go out on one channel and
//! responses come back, in any order, on another, matched by id. HTTP is
//! request/response: each POST's reply (JSON, or an SSE stream ending in the
//! JSON-RPC response) is the answer.

use super::config::Launch;
use super::sse::SseParser;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// The protocol revision we ask for. Servers answer with the one they speak.
pub const PROTOCOL_VERSION: &str = "2025-03-26";

/// A JSON-RPC error returned by the server.
#[derive(Debug, thiserror::Error)]
#[error("MCP error {code}: {message}")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a request and return its `result` (or the server's error).
    async fn request(&self, method: &str, params: Value) -> Result<Value>;
    /// Send a notification (no response expected).
    async fn notify(&self, method: &str, params: Value) -> Result<()>;
    /// Record the protocol version negotiated by `initialize` (HTTP sends it
    /// as a header on later requests).
    fn set_protocol_version(&self, _version: &str) {}
}

/// Turn a response message into its `result`, or the server's error.
fn into_result(msg: &Value) -> Result<Value> {
    if let Some(err) = msg.get("error") {
        return Err(RpcError {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(-1),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
        }
        .into());
    }
    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
}

fn request_message(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn notification_message(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

// ── Routing (stdio + SSE) ──────────────────────────────────────────────────────

/// Matches responses to waiting requests, and answers the few requests a
/// server may send us.
#[derive(Default)]
struct Router {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
}

impl Router {
    fn register(&self) -> (u64, oneshot::Receiver<Value>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        (id, rx)
    }

    fn forget(&self, id: u64) {
        self.pending.lock().unwrap().remove(&id);
    }

    /// Handle one incoming message. Anything that needs a reply (a request
    /// from the server) is answered through `reply`.
    fn handle(&self, msg: Value, reply: &mpsc::UnboundedSender<Value>) {
        let is_response = msg.get("result").is_some() || msg.get("error").is_some();
        if msg.get("method").is_none() && is_response {
            if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(msg);
                }
            }
            return;
        }
        // A request from the server: we advertise no client capabilities, so
        // only `ping` is meaningful. Notifications need no reply.
        if let (Some(method), Some(id)) = (msg.get("method").and_then(Value::as_str), msg.get("id"))
        {
            let response = if method == "ping" {
                json!({ "jsonrpc": "2.0", "id": id, "result": {} })
            } else {
                json!({ "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32601, "message": format!("method not supported: {method}") } })
            };
            let _ = reply.send(response);
        }
    }

    /// The connection died: wake every waiting request with an error.
    fn fail_all(&self, why: &str) {
        let waiting: Vec<_> = self.pending.lock().unwrap().drain().collect();
        for (_, tx) in waiting {
            let _ = tx.send(json!({ "error": { "code": -32000, "message": why } }));
        }
    }
}

/// A transport whose messages travel over a pair of channels, serviced by
/// background tasks (stdio and SSE).
struct ChannelTransport {
    router: Arc<Router>,
    out: mpsc::UnboundedSender<Value>,
    timeout: Duration,
    label: String,
    /// Background tasks, aborted on drop.
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Child process group to kill on drop (stdio).
    child_group: Option<u32>,
    /// Last stderr output of the child, for error messages.
    stderr_tail: Arc<Mutex<String>>,
}

impl Drop for ChannelTransport {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        if let Some(pid) = self.child_group {
            // SAFETY: a plain signal to the process group we created.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

#[async_trait]
impl Transport for ChannelTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let (id, rx) = self.router.register();
        if self.out.send(request_message(id, method, params)).is_err() {
            self.router.forget(id);
            bail!("{}: connection closed{}", self.label, self.stderr_hint());
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(msg)) => into_result(&msg).map_err(|e| {
                // A dead connection surfaces as a synthetic error; add why.
                if e.downcast_ref::<RpcError>()
                    .is_some_and(|r| r.code == -32000)
                {
                    anyhow!("{e}{}", self.stderr_hint())
                } else {
                    e
                }
            }),
            Ok(Err(_)) => bail!("{}: connection closed{}", self.label, self.stderr_hint()),
            Err(_) => {
                self.router.forget(id);
                let _ = self.out.send(notification_message(
                    "notifications/cancelled",
                    json!({ "requestId": id, "reason": "timed out" }),
                ));
                bail!(
                    "{}: no response to {method} after {}s",
                    self.label,
                    self.timeout.as_secs()
                )
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.out
            .send(notification_message(method, params))
            .map_err(|_| anyhow!("{}: connection closed", self.label))
    }
}

impl ChannelTransport {
    fn stderr_hint(&self) -> String {
        let tail = self.stderr_tail.lock().unwrap();
        let tail = tail.trim();
        if tail.is_empty() {
            String::new()
        } else {
            format!(" (server stderr: {tail})")
        }
    }
}

// ── stdio ──────────────────────────────────────────────────────────────────────

/// Keep only the last ~2 KB of a server's stderr.
const STDERR_TAIL_BYTES: usize = 2048;

pub async fn connect_stdio(
    label: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<Box<dyn Transport>> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start `{command}`"))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut stderr = child.stderr.take().context("no stderr")?;

    let router = Arc::new(Router::default());
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let stderr_tail = Arc::new(Mutex::new(String::new()));

    // Writer: one JSON message per line.
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let mut line = msg.to_string();
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });

    // Reader: route responses; anything unparseable (stray logging) is skipped.
    let reader = {
        let router = router.clone();
        let reply = out_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(msg) = serde_json::from_str::<Value>(line.trim()) {
                    router.handle(msg, &reply);
                }
            }
            router.fail_all("the server closed the connection");
        })
    };

    let tail = stderr_tail.clone();
    let stderr_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            let mut tail = tail.lock().unwrap();
            tail.push_str(&String::from_utf8_lossy(&buf[..n]));
            if tail.len() > STDERR_TAIL_BYTES {
                let mut cut = tail.len() - STDERR_TAIL_BYTES;
                while !tail.is_char_boundary(cut) {
                    cut += 1;
                }
                tail.drain(..cut);
            }
        }
    });

    // The child itself is kept alive (and reaped) by a task that owns it.
    let waiter = tokio::spawn(async move {
        let _ = child.wait().await;
    });

    Ok(Box::new(ChannelTransport {
        router,
        out: out_tx,
        timeout,
        label: label.to_string(),
        tasks: vec![writer, reader, stderr_task, waiter],
        child_group: pid,
        stderr_tail,
    }))
}

// ── SSE (legacy HTTP+SSE) ──────────────────────────────────────────────────────

fn header_map(headers: &BTreeMap<String, String>) -> Result<reqwest::header::HeaderMap> {
    let mut map = reqwest::header::HeaderMap::new();
    for (k, v) in headers {
        map.insert(
            reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .with_context(|| format!("bad header name '{k}'"))?,
            reqwest::header::HeaderValue::from_str(v)
                .with_context(|| format!("bad value for header '{k}'"))?,
        );
    }
    Ok(map)
}

/// Resolve the `endpoint` event's data against the SSE URL.
fn resolve_endpoint(base: &str, endpoint: &str) -> Result<String> {
    let base = reqwest::Url::parse(base).context("bad SSE url")?;
    Ok(base
        .join(endpoint.trim())
        .with_context(|| format!("bad endpoint '{endpoint}'"))?
        .to_string())
}

pub async fn connect_sse(
    label: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<Box<dyn Transport>> {
    let headers = header_map(headers)?;
    let client = reqwest::Client::builder()
        .connect_timeout(timeout)
        .build()?;
    let response = client
        .get(url)
        .headers(headers.clone())
        .header("Accept", "text/event-stream")
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;
    if !response.status().is_success() {
        bail!("{url} answered HTTP {}", response.status());
    }

    let router = Arc::new(Router::default());
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let (endpoint_tx, endpoint_rx) = oneshot::channel::<String>();

    let reader = {
        let router = router.clone();
        let reply = out_tx.clone();
        let base = url.to_string();
        tokio::spawn(async move {
            let mut endpoint_tx = Some(endpoint_tx);
            let mut parser = SseParser::default();
            let mut stream = response.bytes_stream();
            while let Some(Ok(chunk)) = stream.next().await {
                for event in parser.feed(&chunk) {
                    match event.event.as_str() {
                        "endpoint" => {
                            if let Some(tx) = endpoint_tx.take() {
                                let _ = tx.send(
                                    resolve_endpoint(&base, &event.data)
                                        .unwrap_or_else(|e| format!("error: {e}")),
                                );
                            }
                        }
                        _ => {
                            if let Ok(msg) = serde_json::from_str::<Value>(&event.data) {
                                router.handle(msg, &reply);
                            }
                        }
                    }
                }
            }
            router.fail_all("the SSE stream ended");
        })
    };

    let endpoint = match tokio::time::timeout(timeout, endpoint_rx).await {
        Ok(Ok(e)) if !e.starts_with("error: ") => e,
        Ok(Ok(e)) => {
            reader.abort();
            bail!("{e}")
        }
        _ => {
            reader.abort();
            bail!("the SSE server never announced a message endpoint");
        }
    };

    // Writer: POST each outgoing message to the announced endpoint.
    let writer = {
        let client = client.clone();
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                let sent = client
                    .post(&endpoint)
                    .headers(headers.clone())
                    .json(&msg)
                    .send()
                    .await;
                if let Err(e) = sent {
                    tracing::warn!("MCP SSE post failed: {e}");
                }
            }
        })
    };

    Ok(Box::new(ChannelTransport {
        router,
        out: out_tx,
        timeout,
        label: label.to_string(),
        tasks: vec![reader, writer],
        child_group: None,
        stderr_tail: Arc::default(),
    }))
}

// ── Streamable HTTP ────────────────────────────────────────────────────────────

struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: reqwest::header::HeaderMap,
    next_id: AtomicU64,
    session_id: Mutex<Option<String>>,
    protocol: Mutex<Option<String>>,
    timeout: Duration,
    label: String,
}

pub fn connect_http(
    label: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<Box<dyn Transport>> {
    Ok(Box::new(HttpTransport {
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?,
        url: url.to_string(),
        headers: header_map(headers)?,
        next_id: AtomicU64::new(0),
        session_id: Mutex::new(None),
        protocol: Mutex::new(None),
        timeout,
        label: label.to_string(),
    }))
}

impl HttpTransport {
    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        let mut req = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .header("Accept", "application/json, text/event-stream")
            .json(body);
        if let Some(id) = self.session_id.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", id);
        }
        if let Some(v) = self.protocol.lock().unwrap().clone() {
            req = req.header("MCP-Protocol-Version", v);
        }
        let response = req
            .send()
            .await
            .with_context(|| format!("{}: could not reach {}", self.label, self.url))?;
        if let Some(sid) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().unwrap() = Some(sid.to_string());
        }
        Ok(response)
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let work = async {
            let response = self.post(&request_message(id, method, params)).await?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "{}: HTTP {status}: {}",
                    self.label,
                    body.chars().take(300).collect::<String>()
                );
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            if content_type.contains("text/event-stream") {
                let mut parser = SseParser::default();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    for event in parser.feed(&chunk?) {
                        if let Ok(msg) = serde_json::from_str::<Value>(&event.data) {
                            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                                return into_result(&msg);
                            }
                        }
                    }
                }
                bail!(
                    "{}: the response stream ended without an answer",
                    self.label
                )
            }
            let body: Value = response
                .json()
                .await
                .with_context(|| format!("{}: the response was not JSON", self.label))?;
            let answer = match &body {
                Value::Array(items) => items
                    .iter()
                    .find(|m| m.get("id").and_then(Value::as_u64) == Some(id)),
                single => Some(single),
            };
            match answer {
                Some(msg) => into_result(msg),
                None => bail!("{}: no response to {method} in the reply", self.label),
            }
        };
        tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| {
                anyhow!(
                    "{}: no response to {method} after {}s",
                    self.label,
                    self.timeout.as_secs()
                )
            })?
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let response = self.post(&notification_message(method, params)).await?;
        if !response.status().is_success() {
            bail!("{}: HTTP {} for {method}", self.label, response.status());
        }
        Ok(())
    }

    fn set_protocol_version(&self, version: &str) {
        *self.protocol.lock().unwrap() = Some(version.to_string());
    }
}

/// Open a transport for a resolved launch description.
pub async fn connect(
    label: &str,
    launch: &Launch,
    timeout: Duration,
) -> Result<Box<dyn Transport>> {
    match launch {
        Launch::Stdio { command, args, env } => {
            connect_stdio(label, command, args, env, timeout).await
        }
        Launch::Http { url, headers } => connect_http(label, url, headers, timeout),
        Launch::Sse { url, headers } => connect_sse(label, url, headers, timeout).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_become_results_or_typed_errors() {
        assert_eq!(
            into_result(&json!({"id": 1, "result": {"ok": true}})).unwrap(),
            json!({"ok": true})
        );
        assert_eq!(into_result(&json!({"id": 1})).unwrap(), Value::Null);
        let err =
            into_result(&json!({"id": 1, "error": {"code": -32602, "message": "bad params"}}))
                .unwrap_err();
        let rpc = err.downcast_ref::<RpcError>().expect("typed error");
        assert_eq!((rpc.code, rpc.message.as_str()), (-32602, "bad params"));
    }

    #[test]
    fn the_router_matches_responses_by_id_in_any_order() {
        let router = Router::default();
        let (reply, _rx) = mpsc::unbounded_channel();
        let (id1, mut r1) = router.register();
        let (id2, mut r2) = router.register();
        assert_ne!(id1, id2);
        router.handle(
            json!({"jsonrpc": "2.0", "id": id2, "result": "two"}),
            &reply,
        );
        router.handle(
            json!({"jsonrpc": "2.0", "id": id1, "result": "one"}),
            &reply,
        );
        assert_eq!(r2.try_recv().unwrap()["result"], "two");
        assert_eq!(r1.try_recv().unwrap()["result"], "one");
        // A response nobody waits for is dropped quietly.
        router.handle(json!({"id": 99, "result": 1}), &reply);
    }

    #[test]
    fn the_router_answers_pings_and_refuses_other_server_requests() {
        let router = Router::default();
        let (reply, mut rx) = mpsc::unbounded_channel();
        router.handle(json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}), &reply);
        let pong = rx.try_recv().unwrap();
        assert_eq!(pong["id"], 7);
        assert_eq!(pong["result"], json!({}));

        router.handle(
            json!({"jsonrpc": "2.0", "id": "a", "method": "sampling/createMessage"}),
            &reply,
        );
        let refusal = rx.try_recv().unwrap();
        assert_eq!(refusal["error"]["code"], -32601);

        // Notifications get no reply.
        router.handle(
            json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
            &reply,
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_dead_connection_wakes_every_waiting_request() {
        let router = Router::default();
        let (_, mut r) = router.register();
        router.fail_all("gone");
        let msg = r.try_recv().unwrap();
        assert_eq!(
            into_result(&msg).unwrap_err().to_string(),
            "MCP error -32000: gone"
        );
    }

    #[test]
    fn sse_endpoints_resolve_against_the_stream_url() {
        assert_eq!(
            resolve_endpoint("http://h:8/sse", "/messages?sessionId=1").unwrap(),
            "http://h:8/messages?sessionId=1"
        );
        assert_eq!(
            resolve_endpoint("http://h:8/a/sse", "https://other/m").unwrap(),
            "https://other/m"
        );
    }

    #[test]
    fn header_maps_reject_garbage() {
        let good: BTreeMap<_, _> = [("Authorization".to_string(), "Bearer x".to_string())].into();
        assert!(header_map(&good).is_ok());
        let bad: BTreeMap<_, _> = [("bad name".to_string(), "v".to_string())].into();
        assert!(header_map(&bad).is_err());
    }
}
