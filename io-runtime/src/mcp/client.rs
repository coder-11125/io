//! The MCP client: the initialize handshake, tool discovery and tool calls
//! over any [`Transport`].

use super::config::Launch;
use super::transport::{self, Transport, PROTOCOL_VERSION};
use crate::tools::ToolImage;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// Cap on the text one tool call may return to the model.
pub const MAX_RESULT_BYTES: usize = 100 * 1024;
/// Stop following `nextCursor` after this many pages of tools.
const MAX_TOOL_PAGES: usize = 20;

/// A tool a server offers.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// The server's `readOnlyHint`. Advisory only: a server can lie.
    pub read_only: bool,
}

/// What a `tools/call` came back with, in the shape our tools use.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallOutcome {
    pub text: String,
    pub images: Vec<ToolImage>,
    pub is_error: bool,
}

pub struct McpClient {
    transport: Box<dyn Transport>,
    /// The server's self-reported name and version, if it gave them.
    pub server_info: Option<String>,
}

impl McpClient {
    /// Connect, then run the `initialize` handshake. `startup` bounds the
    /// whole thing; `call` is the per-request timeout afterwards.
    pub async fn connect(
        label: &str,
        launch: &Launch,
        startup: Duration,
        call: Duration,
    ) -> Result<Self> {
        let handshake = async {
            let transport = transport::connect(label, launch, startup.max(call)).await?;
            Self::initialize(transport).await
        };
        tokio::time::timeout(startup, handshake)
            .await
            .map_err(|_| anyhow::anyhow!("did not finish starting within {}s", startup.as_secs()))?
    }

    /// Wrap an already-open transport and run the handshake. (Separate from
    /// `connect` so tests can supply a fake transport.)
    pub async fn initialize(transport: Box<dyn Transport>) -> Result<Self> {
        let result = transport
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "io", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await
            .context("initialize failed")?;
        if let Some(version) = result.get("protocolVersion").and_then(Value::as_str) {
            transport.set_protocol_version(version);
        }
        let server_info = result.get("serverInfo").and_then(|i| {
            let name = i.get("name")?.as_str()?;
            Some(match i.get("version").and_then(Value::as_str) {
                Some(v) => format!("{name} {v}"),
                None => name.to_string(),
            })
        });
        transport
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(Self {
            transport,
            server_info,
        })
    }

    /// Every tool the server offers, following pagination.
    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = self
                .transport
                .request("tools/list", params)
                .await
                .context("tools/list failed")?;
            for t in page
                .get("tools")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                if let Some(info) = parse_tool(t) {
                    tools.push(info);
                }
            }
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => return Ok(tools),
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallOutcome> {
        let result = self
            .transport
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        Ok(parse_call_result(&result))
    }
}

fn parse_tool(t: &Value) -> Option<ToolInfo> {
    let name = t.get("name")?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(ToolInfo {
        description: t
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        input_schema: t
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        read_only: t
            .pointer("/annotations/readOnlyHint")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        name,
    })
}

fn cap(mut text: String) -> String {
    if text.len() > MAX_RESULT_BYTES {
        let mut end = MAX_RESULT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let total = text.len();
        text.truncate(end);
        text.push_str(&format!("\n… (truncated, {total} bytes total)"));
    }
    text
}

/// Flatten a `tools/call` result: text blocks joined, images attached,
/// everything else described briefly.
pub fn parse_call_result(result: &Value) -> CallOutcome {
    let mut parts: Vec<String> = Vec::new();
    let mut images = Vec::new();
    for item in result
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    parts.push(t.to_string());
                }
            }
            Some("image") => {
                let data = item.get("data").and_then(Value::as_str);
                let mime = item.get("mimeType").and_then(Value::as_str).unwrap_or("");
                match (data, mime) {
                    (Some(data), "image/png" | "image/jpeg" | "image/gif" | "image/webp") => images
                        .push(ToolImage {
                            media_type: mime.to_string(),
                            data: data.to_string(),
                        }),
                    _ => parts.push(format!("[image of type '{mime}' omitted]")),
                }
            }
            Some("audio") => parts.push("[audio content omitted]".to_string()),
            Some("resource") => {
                let r = item.get("resource");
                let uri = r
                    .and_then(|r| r.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match r.and_then(|r| r.get("text")).and_then(Value::as_str) {
                    Some(text) => parts.push(text.to_string()),
                    None => parts.push(format!("[binary resource {uri}]")),
                }
            }
            Some("resource_link") => {
                let uri = item.get("uri").and_then(Value::as_str).unwrap_or("");
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                parts.push(format!("[resource link: {name} {uri}]"));
            }
            _ => {}
        }
    }
    // A server that only returns structured content still says something.
    if parts.is_empty() && images.is_empty() {
        if let Some(structured) = result.get("structuredContent") {
            parts.push(serde_json::to_string_pretty(structured).unwrap_or_default());
        }
    }
    CallOutcome {
        text: cap(parts.join("\n")),
        images,
        is_error: result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A scripted in-memory server.
    struct Fake {
        calls: Mutex<Vec<(String, Value)>>,
        pages: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl Transport for Fake {
        async fn request(&self, method: &str, params: Value) -> Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            match method {
                "initialize" => Ok(json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fake", "version": "1.2" },
                })),
                "tools/list" => Ok(self.pages.lock().unwrap().remove(0)),
                "tools/call" => Ok(json!({ "content": [{ "type": "text", "text": "called" }] })),
                other => anyhow::bail!("unexpected {other}"),
            }
        }
        async fn notify(&self, method: &str, params: Value) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            Ok(())
        }
    }

    fn fake(pages: Vec<Value>) -> (Box<dyn Transport>, std::sync::Arc<Fake>) {
        let f = std::sync::Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            pages: Mutex::new(pages),
        });
        struct Shared(std::sync::Arc<Fake>);
        #[async_trait]
        impl Transport for Shared {
            async fn request(&self, m: &str, p: Value) -> Result<Value> {
                self.0.request(m, p).await
            }
            async fn notify(&self, m: &str, p: Value) -> Result<()> {
                self.0.notify(m, p).await
            }
        }
        (Box::new(Shared(f.clone())), f)
    }

    #[tokio::test]
    async fn the_handshake_sends_initialize_then_the_initialized_notification() {
        let (transport, probe) = fake(vec![]);
        let client = McpClient::initialize(transport).await.unwrap();
        assert_eq!(client.server_info.as_deref(), Some("fake 1.2"));
        let calls = probe.calls.lock().unwrap();
        assert_eq!(calls[0].0, "initialize");
        assert_eq!(calls[0].1["clientInfo"]["name"], "io");
        assert_eq!(calls[0].1["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(calls[1].0, "notifications/initialized");
    }

    #[tokio::test]
    async fn tool_listing_follows_cursors_and_reads_annotations() {
        let (transport, probe) = fake(vec![
            json!({ "tools": [{
                "name": "search", "description": "find things",
                "inputSchema": { "type": "object", "properties": { "q": { "type": "string" } } },
                "annotations": { "readOnlyHint": true }
            }], "nextCursor": "p2" }),
            json!({ "tools": [{ "name": "delete" }, { "description": "nameless" }] }),
        ]);
        let client = McpClient::initialize(transport).await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2, "the nameless entry is skipped");
        assert_eq!(tools[0].name, "search");
        assert!(tools[0].read_only);
        assert_eq!(tools[0].input_schema["properties"]["q"]["type"], "string");
        assert!(!tools[1].read_only, "no hint means not read-only");
        assert_eq!(
            tools[1].input_schema["type"], "object",
            "a schema is supplied"
        );
        let calls = probe.calls.lock().unwrap();
        let lists: Vec<_> = calls.iter().filter(|c| c.0 == "tools/list").collect();
        assert_eq!(lists[1].1["cursor"], "p2");
    }

    #[tokio::test]
    async fn calls_send_the_name_and_arguments() {
        let (transport, probe) = fake(vec![]);
        let client = McpClient::initialize(transport).await.unwrap();
        let out = client
            .call_tool("echo", json!({ "text": "hi" }))
            .await
            .unwrap();
        assert_eq!(out.text, "called");
        let calls = probe.calls.lock().unwrap();
        let call = calls.iter().find(|c| c.0 == "tools/call").unwrap();
        assert_eq!(call.1["name"], "echo");
        assert_eq!(call.1["arguments"]["text"], "hi");
    }

    #[test]
    fn results_flatten_text_images_and_resources() {
        let out = parse_call_result(&json!({
            "content": [
                { "type": "text", "text": "line one" },
                { "type": "image", "data": "QUJD", "mimeType": "image/png" },
                { "type": "image", "data": "QUJD", "mimeType": "image/tiff" },
                { "type": "audio", "data": "x", "mimeType": "audio/wav" },
                { "type": "resource", "resource": { "uri": "file:///a", "text": "from resource" } },
                { "type": "resource", "resource": { "uri": "file:///b", "blob": "AAAA" } },
                { "type": "resource_link", "uri": "https://x", "name": "doc" },
            ],
        }));
        assert!(!out.is_error);
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].media_type, "image/png");
        for expected in [
            "line one",
            "image of type 'image/tiff' omitted",
            "audio content omitted",
            "from resource",
            "binary resource file:///b",
            "resource link: doc https://x",
        ] {
            assert!(
                out.text.contains(expected),
                "missing {expected:?} in {:?}",
                out.text
            );
        }
    }

    #[test]
    fn errors_structured_content_and_oversized_results_are_handled() {
        let err = parse_call_result(&json!({
            "isError": true,
            "content": [{ "type": "text", "text": "boom" }],
        }));
        assert!(err.is_error);
        assert_eq!(err.text, "boom");

        let structured = parse_call_result(&json!({ "structuredContent": { "n": 3 } }));
        assert!(structured.text.contains("\"n\": 3"));

        let big = parse_call_result(&json!({
            "content": [{ "type": "text", "text": "x".repeat(MAX_RESULT_BYTES + 500) }],
        }));
        assert!(big.text.len() < MAX_RESULT_BYTES + 100);
        assert!(big.text.contains("truncated"));

        let empty = parse_call_result(&json!({}));
        assert_eq!(empty, CallOutcome::default());
    }
}
