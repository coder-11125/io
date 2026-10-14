use super::{
    CompletionModel, CompletionRequest, CompletionResponse, ContentBlock, Role, StreamEvent, Usage,
};
use crate::config::AnthropicConfig;

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    config: AnthropicConfig,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(config: AnthropicConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build reqwest client");
        Self { config, client }
    }

    fn api_key(&self) -> anyhow::Result<String> {
        if let Some(ref key) = self.config.api_key {
            return Ok(key.clone());
        }
        let env_var = self
            .config
            .api_key_env
            .as_deref()
            .unwrap_or("ANTHROPIC_API_KEY");
        std::env::var(env_var)
            .map_err(|_| anyhow::anyhow!("missing {env_var} environment variable"))
    }

    /// The authentication header for this request: `Authorization: Bearer` for
    /// OAuth (Claude subscription) logins, `x-api-key` for API keys. OAuth
    /// tokens are refreshed (and persisted) automatically when expired.
    async fn auth_header(&self) -> anyhow::Result<(reqwest::header::HeaderName, String)> {
        if self.config.auth == crate::config::AuthMethod::OAuth {
            let token = crate::oauth::oauth_access_token("anthropic").await?;
            Ok((reqwest::header::AUTHORIZATION, format!("Bearer {token}")))
        } else {
            Ok(("x-api-key".parse()?, self.api_key()?))
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.config.base_url.trim_end_matches('/'), path)
    }

    /// The `/messages` request body, shared by the blocking and streaming
    /// paths so both always send identical content and cache markers.
    fn build_body(&self, request: &CompletionRequest, stream: bool) -> serde_json::Value {
        let cache = self.config.prompt_cache;
        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": request.max_tokens.unwrap_or(16384),
            "messages": build_messages(request, cache),
        });
        if stream {
            body["stream"] = serde_json::json!(true);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        let system = build_system(request, cache);
        // Request order is tools → system → messages, so a breakpoint on the
        // system block caches the tool definitions too. Without a system
        // prompt, mark the last tool instead.
        if !request.tools.is_empty() {
            body["tools"] = build_tools(request, cache && system.is_none());
        }
        if let Some(system) = system {
            body["system"] = system;
        }
        body
    }
}

/// Anthropic's prompt-cache breakpoint marker (5-minute TTL).
fn cache_marker() -> serde_json::Value {
    serde_json::json!({ "type": "ephemeral" })
}

/// The `system` field. With caching on it is a single text block carrying a
/// cache breakpoint; otherwise a plain string.
fn build_system(request: &CompletionRequest, cache: bool) -> Option<serde_json::Value> {
    let text = request.system_text()?;
    Some(if cache {
        serde_json::json!([{ "type": "text", "text": text, "cache_control": cache_marker() }])
    } else {
        serde_json::json!(text)
    })
}

fn build_tools(request: &CompletionRequest, cache_last: bool) -> serde_json::Value {
    let mut tools: Vec<serde_json::Value> = request
        .tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.input_schema,
            })
        })
        .collect();
    if cache_last {
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = cache_marker();
        }
    }
    serde_json::json!(tools)
}

/// Stateful parser for Anthropic's server-sent event stream.
/// Shared by `AnthropicProvider` and `BedrockProvider` (which wraps the same
/// Anthropic event JSON inside AWS event-stream binary frames).
pub(crate) struct AnthropicStreamState {
    input_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    // index → (id, name, accumulated_json_args)
    tool_blocks: std::collections::HashMap<u32, (String, String, String)>,
}

impl AnthropicStreamState {
    pub fn new() -> Self {
        Self {
            input_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            tool_blocks: Default::default(),
        }
    }

    /// Process one Anthropic SSE event JSON object.
    /// Returns `(events_to_emit, stream_is_done)`.
    pub fn process_event(
        &mut self,
        event: &serde_json::Value,
    ) -> anyhow::Result<(Vec<StreamEvent>, bool)> {
        let mut events = Vec::new();
        let mut done = false;

        match event["type"].as_str().unwrap_or("") {
            "message_start" => {
                let usage = &event["message"]["usage"];
                if let Some(t) = usage["input_tokens"].as_u64() {
                    self.input_tokens = t as u32;
                }
                self.record_cache_usage(usage);
            }
            "content_block_start" => {
                let idx = event["index"].as_u64().unwrap_or(0) as u32;
                if event["content_block"]["type"].as_str() == Some("tool_use") {
                    let id = event["content_block"]["id"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let name = event["content_block"]["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    self.tool_blocks.insert(idx, (id, name, String::new()));
                }
            }
            "content_block_delta" => {
                let idx = event["index"].as_u64().unwrap_or(0) as u32;
                match event["delta"]["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        if let Some(text) = event["delta"]["text"].as_str() {
                            if !text.is_empty() {
                                events.push(StreamEvent {
                                    delta: Some(text.to_string()),
                                    content_block: None,
                                    stop_reason: None,
                                    usage: None,
                                });
                            }
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) = event["delta"]["partial_json"].as_str() {
                            if let Some(entry) = self.tool_blocks.get_mut(&idx) {
                                entry.2.push_str(partial);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = event["index"].as_u64().unwrap_or(0) as u32;
                if let Some((id, name, args)) = self.tool_blocks.remove(&idx) {
                    let input = serde_json::from_str::<serde_json::Value>(&args)
                        .unwrap_or(serde_json::json!({}));
                    events.push(StreamEvent {
                        delta: None,
                        content_block: Some(ContentBlock::ToolUse { id, name, input }),
                        stop_reason: None,
                        usage: None,
                    });
                }
            }
            "message_delta" => {
                let stop_reason = event["delta"]["stop_reason"].as_str().map(str::to_string);
                let output_tokens = event["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
                // Newer API versions repeat the (final) cache counters here.
                self.record_cache_usage(&event["usage"]);
                events.push(StreamEvent {
                    delta: None,
                    content_block: None,
                    stop_reason,
                    usage: Some(Usage {
                        input_tokens: self.input_tokens,
                        output_tokens,
                        cache_read_tokens: self.cache_read_tokens,
                        cache_write_tokens: self.cache_write_tokens,
                    }),
                });
            }
            "message_stop" => {
                done = true;
            }
            _ => {}
        }

        Ok((events, done))
    }

    /// Take the cache counters from a `usage` object, when it carries them.
    fn record_cache_usage(&mut self, usage: &serde_json::Value) {
        if let Some(n) = usage["cache_read_input_tokens"].as_u64() {
            self.cache_read_tokens = n as u32;
        }
        if let Some(n) = usage["cache_creation_input_tokens"].as_u64() {
            self.cache_write_tokens = n as u32;
        }
    }
}

#[async_trait::async_trait]
impl CompletionModel for AnthropicProvider {
    fn provider_name(&self) -> &'static str {
        "anthropic"
    }

    fn context_window(&self) -> u64 {
        self.config
            .context_window
            .unwrap_or_else(|| super::context_window_for_model(&self.config.model))
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let (auth_header, auth_value) = self.auth_header().await?;
        let body = self.build_body(&request, false);

        let resp = self
            .client
            .post(self.url("messages"))
            .header(auth_header, auth_value)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Anthropic", resp).await);
        }

        let data: AnthropicResponse = resp.json().await?;
        Ok(convert_response(data))
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        let (auth_header, auth_value) = self.auth_header().await?;
        let body = self.build_body(&request, true);

        let resp = self
            .client
            .post(self.url("messages"))
            .header(auth_header, auth_value)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Anthropic", resp).await);
        }

        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();
            let mut state = AnthropicStreamState::new();

            'outer: while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(line_end) = buffer.find('\n') {
                            let line = buffer[..line_end].trim().to_string();
                            buffer = buffer[line_end + 1..].to_string();
                            if line.is_empty() || line.starts_with("event:") {
                                continue;
                            }
                            let Some(data) = line.strip_prefix("data: ") else {
                                continue;
                            };
                            match serde_json::from_str::<serde_json::Value>(data) {
                                Ok(event) => match state.process_event(&event) {
                                    Ok((events, done)) => {
                                        for ev in events {
                                            if tx.send(Ok(ev)).await.is_err() {
                                                break 'outer;
                                            }
                                        }
                                        if done {
                                            break 'outer;
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Err(e)).await;
                                        break 'outer;
                                    }
                                },
                                Err(e) => {
                                    let _ = tx.send(Err(anyhow::anyhow!("parse error: {e}"))).await;
                                    break 'outer;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("stream error: {e}"))).await;
                        break 'outer;
                    }
                }
            }
        });

        Ok(rx)
    }
}

/// Convert the conversation to Anthropic's `messages` array. System messages
/// are skipped here — they travel in the separate `system` field. With
/// `cache` on, the last content block of the last message gets a cache
/// breakpoint, so each request in the agent's tool loop (and the next turn)
/// can reuse everything before it.
pub(crate) fn build_messages(request: &CompletionRequest, cache: bool) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .filter_map(|msg| {
            if matches!(msg.role, Role::System) {
                return None;
            }
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "user",
                _ => "user",
            };

            let content: Vec<serde_json::Value> = msg
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => {
                        serde_json::json!({ "type": "text", "text": text })
                    }
                    ContentBlock::ToolUse { id, name, input } => serde_json::json!({
                        "type": "tool_use", "id": id, "name": name, "input": input,
                    }),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => serde_json::json!({
                        "type": "tool_result", "tool_use_id": tool_use_id, "content": content,
                        "is_error": is_error.unwrap_or(false),
                    }),
                })
                .collect();

            Some(serde_json::json!({ "role": role, "content": content }))
        })
        .collect();

    if cache {
        let last_block = messages
            .last_mut()
            .and_then(|m| m["content"].as_array_mut())
            .and_then(|blocks| blocks.last_mut());
        if let Some(block) = last_block {
            // The API rejects a cache breakpoint on an empty text block.
            let empty_text = block["type"] == "text" && block["text"] == "";
            if !empty_text {
                block["cache_control"] = cache_marker();
            }
        }
    }
    messages
}

#[derive(serde::Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(serde::Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    // `null` or absent when the request used no caching.
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
}

pub(crate) fn parse_and_convert_response(text: &str) -> anyhow::Result<CompletionResponse> {
    let data: AnthropicResponse = serde_json::from_str(text)?;
    Ok(convert_response(data))
}

fn convert_response(data: AnthropicResponse) -> CompletionResponse {
    let content = data
        .content
        .into_iter()
        .map(|block| match block {
            AnthropicContentBlock::Text { text } => ContentBlock::Text { text },
            AnthropicContentBlock::ToolUse { id, name, input } => {
                ContentBlock::ToolUse { id, name, input }
            }
        })
        .collect();

    let usage = data.usage.map(|u| Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cache_read_input_tokens.unwrap_or(0),
        cache_write_tokens: u.cache_creation_input_tokens.unwrap_or(0),
    });
    CompletionResponse {
        content,
        stop_reason: data.stop_reason,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Message;
    use crate::tools::ToolSpec;

    fn msg(role: Role, content: Vec<ContentBlock>) -> Message {
        Message { role, content }
    }

    fn text(t: &str) -> ContentBlock {
        ContentBlock::Text {
            text: t.to_string(),
        }
    }

    fn sample_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![
                msg(Role::System, vec![text("You are a test agent.")]),
                msg(Role::User, vec![text("first question")]),
                msg(Role::Assistant, vec![text("first answer")]),
                msg(
                    Role::User,
                    vec![
                        text("second question"),
                        ContentBlock::ToolResult {
                            tool_use_id: "call_1".to_string(),
                            content: "result".to_string(),
                            is_error: None,
                        },
                    ],
                ),
            ],
            tools: vec![
                ToolSpec {
                    name: "read".to_string(),
                    description: "read a file".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                ToolSpec {
                    name: "bash".to_string(),
                    description: "run a command".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
            ],
            system_prompt: None,
            max_tokens: Some(1024),
            temperature: None,
            stream: false,
        }
    }

    fn provider(prompt_cache: bool) -> AnthropicProvider {
        AnthropicProvider::new(AnthropicConfig {
            prompt_cache,
            ..AnthropicConfig::default()
        })
    }

    #[test]
    fn system_prompt_from_system_message_is_sent() {
        // Regression: the agent supplies its prompt as a Role::System
        // message, which used to be dropped on the way to Anthropic.
        let body = provider(false).build_body(&sample_request(), false);
        assert_eq!(body["system"], "You are a test agent.");
        // …and is not duplicated into the messages array.
        let roles: Vec<&str> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
    }

    #[test]
    fn cache_on_marks_system_and_last_message_block_only() {
        let body = provider(true).build_body(&sample_request(), false);

        // System becomes a single cached text block.
        let system = body["system"].as_array().expect("system is a block array");
        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["text"], "You are a test agent.");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");

        // The breakpoint sits on the last block of the last message…
        let messages = body["messages"].as_array().unwrap();
        let last_blocks = messages.last().unwrap()["content"].as_array().unwrap();
        assert_eq!(
            last_blocks.last().unwrap()["cache_control"]["type"],
            "ephemeral"
        );
        // …and nowhere else in the conversation.
        let marked = messages
            .iter()
            .flat_map(|m| m["content"].as_array().unwrap())
            .filter(|b| b.get("cache_control").is_some())
            .count();
        assert_eq!(marked, 1);

        // Tools are covered by the system breakpoint, not marked themselves.
        assert!(body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t.get("cache_control").is_none()));
    }

    #[test]
    fn cache_off_sends_no_cache_markers() {
        let body = provider(false).build_body(&sample_request(), true);
        assert!(!body.to_string().contains("cache_control"));
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn streaming_and_blocking_bodies_match_apart_from_the_stream_flag() {
        let p = provider(true);
        let mut streaming = p.build_body(&sample_request(), true);
        let blocking = p.build_body(&sample_request(), false);
        assert!(blocking.get("stream").is_none());
        streaming.as_object_mut().unwrap().remove("stream");
        assert_eq!(streaming, blocking);
    }

    #[test]
    fn without_a_system_prompt_the_last_tool_carries_the_breakpoint() {
        let mut req = sample_request();
        req.messages.remove(0);
        let body = provider(true).build_body(&req, false);
        assert!(body.get("system").is_none());
        let tools = body["tools"].as_array().unwrap();
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn empty_trailing_text_block_is_not_marked() {
        let req = CompletionRequest {
            messages: vec![msg(Role::User, vec![text("")])],
            tools: vec![],
            system_prompt: None,
            max_tokens: None,
            temperature: None,
            stream: false,
        };
        let messages = build_messages(&req, true);
        assert!(messages[0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn stream_state_reports_cache_usage_from_message_start() {
        let mut state = AnthropicStreamState::new();
        state
            .process_event(&serde_json::json!({
                "type": "message_start",
                "message": { "usage": {
                    "input_tokens": 12,
                    "cache_read_input_tokens": 800,
                    "cache_creation_input_tokens": 30,
                }}
            }))
            .unwrap();
        let (events, _) = state
            .process_event(&serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 55 }
            }))
            .unwrap();
        let usage = events[0].usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 55);
        assert_eq!(usage.cache_read_tokens, 800);
        assert_eq!(usage.cache_write_tokens, 30);
        assert_eq!(usage.context_tokens(), 842);
    }

    #[test]
    fn stream_state_without_cache_fields_reports_zero_cache_usage() {
        let mut state = AnthropicStreamState::new();
        state
            .process_event(&serde_json::json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": 100 } }
            }))
            .unwrap();
        let (events, _) = state
            .process_event(&serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 5 }
            }))
            .unwrap();
        let usage = events[0].usage.as_ref().unwrap();
        assert_eq!((usage.cache_read_tokens, usage.cache_write_tokens), (0, 0));
        assert_eq!(usage.context_tokens(), 100);
    }

    #[test]
    fn message_delta_cache_counters_override_message_start() {
        let mut state = AnthropicStreamState::new();
        state
            .process_event(&serde_json::json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": 5, "cache_read_input_tokens": 1 } }
            }))
            .unwrap();
        let (events, _) = state
            .process_event(&serde_json::json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 9, "cache_read_input_tokens": 700 }
            }))
            .unwrap();
        assert_eq!(events[0].usage.as_ref().unwrap().cache_read_tokens, 700);
    }

    #[test]
    fn blocking_response_parses_cache_usage_including_nulls() {
        let with_cache = parse_and_convert_response(
            r#"{"content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn",
                "usage":{"input_tokens":4,"output_tokens":2,
                         "cache_read_input_tokens":500,"cache_creation_input_tokens":20}}"#,
        )
        .unwrap();
        let u = with_cache.usage.unwrap();
        assert_eq!((u.cache_read_tokens, u.cache_write_tokens), (500, 20));

        let nulls = parse_and_convert_response(
            r#"{"content":[],"stop_reason":"end_turn",
                "usage":{"input_tokens":4,"output_tokens":2,
                         "cache_read_input_tokens":null,"cache_creation_input_tokens":null}}"#,
        )
        .unwrap();
        let u = nulls.usage.unwrap();
        assert_eq!((u.cache_read_tokens, u.cache_write_tokens), (0, 0));

        let absent = parse_and_convert_response(
            r#"{"content":[],"usage":{"input_tokens":4,"output_tokens":2}}"#,
        )
        .unwrap();
        assert_eq!(absent.usage.unwrap().context_tokens(), 4);
    }
}
