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

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.config.base_url.trim_end_matches('/'), path)
    }
}

/// Stateful parser for Anthropic's server-sent event stream.
/// Shared by `AnthropicProvider` and `BedrockProvider` (which wraps the same
/// Anthropic event JSON inside AWS event-stream binary frames).
pub(crate) struct AnthropicStreamState {
    input_tokens: u32,
    // index → (id, name, accumulated_json_args)
    tool_blocks: std::collections::HashMap<u32, (String, String, String)>,
}

impl AnthropicStreamState {
    pub fn new() -> Self {
        Self {
            input_tokens: 0,
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
                if let Some(t) = event["message"]["usage"]["input_tokens"].as_u64() {
                    self.input_tokens = t as u32;
                }
            }
            "content_block_start" => {
                let idx = event["index"].as_u64().unwrap_or(0) as u32;
                if event["content_block"]["type"].as_str() == Some("tool_use") {
                    let id = event["content_block"]["id"].as_str().unwrap_or("").to_string();
                    let name =
                        event["content_block"]["name"].as_str().unwrap_or("").to_string();
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
                let output_tokens =
                    event["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
                events.push(StreamEvent {
                    delta: None,
                    content_block: None,
                    stop_reason,
                    usage: Some(Usage {
                        input_tokens: self.input_tokens,
                        output_tokens,
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
        let api_key = self.api_key()?;

        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": request.max_tokens.unwrap_or(16384),
            "messages": build_messages(&request),
        });

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !request.tools.is_empty() {
            body["tools"] =
                serde_json::json!(request.tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.input_schema,
            })).collect::<Vec<_>>());
        }
        if let Some(ref system) = request.system_prompt {
            body["system"] = serde_json::json!(system);
        }

        let resp = self
            .client
            .post(self.url("messages"))
            .header("x-api-key", &api_key)
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
        let api_key = self.api_key()?;

        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": request.max_tokens.unwrap_or(16384),
            "messages": build_messages(&request),
            "stream": true,
        });
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !request.tools.is_empty() {
            body["tools"] =
                serde_json::json!(request.tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.input_schema,
            })).collect::<Vec<_>>());
        }
        if let Some(ref system) = request.system_prompt {
            body["system"] = serde_json::json!(system);
        }

        let resp = self
            .client
            .post(self.url("messages"))
            .header("x-api-key", &api_key)
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
                        loop {
                            let Some(line_end) = buffer.find('\n') else { break };
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
                                    let _ = tx
                                        .send(Err(anyhow::anyhow!("parse error: {e}")))
                                        .await;
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

pub(crate) fn build_messages(request: &CompletionRequest) -> Vec<serde_json::Value> {
    request
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
        .collect()
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
    });
    CompletionResponse {
        content,
        stop_reason: data.stop_reason,
        usage,
    }
}
