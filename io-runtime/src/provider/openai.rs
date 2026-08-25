use super::{
    CompletionModel, CompletionRequest, CompletionResponse, ContentBlock, Message, Role,
    StreamEvent, Usage,
};
use crate::config::OpenAIConfig;

#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    config: OpenAIConfig,
    client: reqwest::Client,
}

impl OpenAIProvider {
    pub fn new(config: OpenAIConfig) -> Self {
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
            .unwrap_or("OPENAI_API_KEY");
        std::env::var(env_var)
            .map_err(|_| anyhow::anyhow!("missing {env_var} environment variable"))
    }

    /// The bearer credential: the OAuth access token when the provider is
    /// configured for OAuth login, otherwise the API key. OAuth tokens are
    /// refreshed (and persisted) automatically when expired.
    async fn bearer_token(&self) -> anyhow::Result<String> {
        if self.config.auth == crate::config::AuthMethod::OAuth {
            crate::oauth::oauth_access_token("openai").await
        } else {
            self.api_key()
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.config.base_url.trim_end_matches('/'), path)
    }
}

#[async_trait::async_trait]
impl CompletionModel for OpenAIProvider {
    fn provider_name(&self) -> &'static str {
        "openai"
    }

    fn context_window(&self) -> u64 {
        self.config
            .context_window
            .unwrap_or_else(|| super::context_window_for_model(&self.config.model))
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let token = self.bearer_token().await?;
        let body = build_chat_body(&self.config, &request, false);

        let resp = self
            .client
            .post(self.url("chat/completions"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("OpenAI", resp).await);
        }

        let data: ChatResponse = resp.json().await?;
        Ok(convert_chat_response(data))
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        let token = self.bearer_token().await?;
        let body = build_chat_body(&self.config, &request, true);

        let resp = self
            .client
            .post(self.url("chat/completions"))
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("OpenAI", resp).await);
        }

        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();
            // Accumulate tool call arguments by index: index -> (id, name, args_buf)
            let mut pending: std::collections::HashMap<u32, (String, String, String)> =
                Default::default();

            'outer: while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(line_end) = buffer.find('\n') {
                            let line = buffer[..line_end].trim().to_string();
                            buffer = buffer[line_end + 1..].to_string();
                            if line.is_empty() {
                                continue;
                            }
                            if !line.starts_with("data: ") {
                                continue;
                            }
                            let data = &line[6..];
                            if data == "[DONE]" {
                                // Emit accumulated tool calls in order
                                let mut calls: Vec<_> = pending.into_iter().collect();
                                calls.sort_by_key(|(i, _)| *i);
                                for (_, (id, name, args)) in calls {
                                    let input = serde_json::from_str::<serde_json::Value>(&args)
                                        .unwrap_or(serde_json::json!({}));
                                    if tx
                                        .send(Ok(StreamEvent {
                                            delta: None,
                                            content_block: Some(ContentBlock::ToolUse {
                                                id,
                                                name,
                                                input,
                                            }),
                                            stop_reason: None,
                                            usage: None,
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        break 'outer;
                                    }
                                }
                                let _ = tx
                                    .send(Ok(StreamEvent {
                                        delta: None,
                                        content_block: None,
                                        stop_reason: Some("stop".to_string()),
                                        usage: None,
                                    }))
                                    .await;
                                break 'outer;
                            }
                            match serde_json::from_str::<ChatChunk>(data) {
                                Ok(chunk) => {
                                    // Emit usage when present (final chunk before [DONE])
                                    if let Some(ref u) = chunk.usage {
                                        if tx
                                            .send(Ok(StreamEvent {
                                                delta: None,
                                                content_block: None,
                                                stop_reason: None,
                                                usage: Some(Usage {
                                                    input_tokens: u.prompt_tokens,
                                                    output_tokens: u.completion_tokens,
                                                }),
                                            }))
                                            .await
                                            .is_err()
                                        {
                                            break 'outer;
                                        }
                                    }
                                    for choice in chunk.choices {
                                        if let Some(ref c) = choice.delta.content {
                                            if !c.is_empty()
                                                && tx
                                                    .send(Ok(StreamEvent {
                                                        delta: Some(c.clone()),
                                                        content_block: None,
                                                        stop_reason: None,
                                                        usage: None,
                                                    }))
                                                    .await
                                                    .is_err()
                                            {
                                                break 'outer;
                                            }
                                        }
                                        if let Some(ref calls) = choice.delta.tool_calls {
                                            for tc in calls {
                                                let idx = tc.index.unwrap_or(0);
                                                let entry =
                                                    pending.entry(idx).or_insert_with(|| {
                                                        (
                                                            String::new(),
                                                            String::new(),
                                                            String::new(),
                                                        )
                                                    });
                                                if let Some(ref id) = tc.id {
                                                    entry.0 = id.clone();
                                                }
                                                if let Some(ref f) = tc.function {
                                                    if let Some(ref name) = f.name {
                                                        entry.1 = name.clone();
                                                    }
                                                    if let Some(ref args) = f.arguments {
                                                        entry.2.push_str(args);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
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

pub(crate) fn build_chat_body_with_model(
    model: &str,
    request: &CompletionRequest,
    stream: bool,
) -> serde_json::Value {
    let cfg = OpenAIConfig {
        model: model.to_string(),
        base_url: String::new(),
        api_key_env: None,
        api_key: None,
        context_window: None,
        cost_input_per_1k: None,
        cost_output_per_1k: None,
        auth: crate::config::AuthMethod::ApiKey,
    };
    build_chat_body(&cfg, request, stream)
}

pub(crate) fn parse_and_convert_chat_response(text: &str) -> anyhow::Result<CompletionResponse> {
    let data: ChatResponse = serde_json::from_str(text)?;
    Ok(convert_chat_response(data))
}

pub(crate) fn parse_and_convert_chunk(data: &str) -> anyhow::Result<StreamEvent> {
    let chunk: ChatChunk = serde_json::from_str(data)?;
    Ok(convert_chunk(chunk))
}

fn build_chat_body(
    config: &OpenAIConfig,
    request: &CompletionRequest,
    stream: bool,
) -> serde_json::Value {
    let messages = convert_messages(&request.messages);
    let mut body =
        serde_json::json!({ "model": config.model, "messages": messages, "stream": stream });

    if stream {
        body["stream_options"] = serde_json::json!({ "include_usage": true });
    }
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }
    if let Some(temp) = request.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request.tools.iter().map(|t| serde_json::json!({
            "type": "function",
            "function": { "name": t.name, "description": t.description, "parameters": t.input_schema }
        })).collect();
        body["tools"] = serde_json::json!(tools);
    }
    if let Some(ref system) = request.system_prompt {
        if let Some(arr) = body["messages"].as_array_mut() {
            arr.insert(
                0,
                serde_json::json!({ "role": "system", "content": system }),
            );
        }
    }
    body
}

/// Convert our internal message list to OpenAI wire format.
/// Assistant tool-use becomes `tool_calls`; tool results become separate `role:tool` messages.
fn convert_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for msg in messages {
        match msg.role {
            Role::System => {
                let text = msg
                    .content
                    .iter()
                    .find_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .unwrap_or("");
                out.push(serde_json::json!({ "role": "system", "content": text }));
            }
            Role::User => {
                let has_results = msg
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
                if has_results {
                    for block in &msg.content {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } = block
                        {
                            out.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content,
                            }));
                        }
                    }
                } else {
                    let content: Vec<serde_json::Value> = msg
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(serde_json::json!({"type": "text", "text": text}))
                            } else {
                                None
                            }
                        })
                        .collect();
                    out.push(serde_json::json!({ "role": "user", "content": content }));
                }
            }
            Role::Assistant => {
                let mut text_parts: Vec<serde_json::Value> = Vec::new();
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            text_parts.push(serde_json::json!({"type": "text", "text": text}));
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(serde_json::json!({
                                "id": id,
                                "type": "function",
                                "function": { "name": name, "arguments": input.to_string() }
                            }));
                        }
                        _ => {}
                    }
                }
                let mut m = serde_json::json!({ "role": "assistant" });
                m["content"] = if text_parts.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::json!(text_parts)
                };
                if !tool_calls.is_empty() {
                    m["tool_calls"] = serde_json::json!(tool_calls);
                }
                out.push(m);
            }
            Role::Tool => {
                for block in &msg.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = block
                    {
                        out.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content,
                        }));
                    }
                }
            }
        }
    }
    out
}

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}
#[derive(serde::Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}
#[derive(serde::Deserialize)]
struct ChatMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ChatToolCall>>,
}
#[derive(serde::Deserialize)]
struct ChatToolCall {
    id: String,
    function: ChatFunction,
}
#[derive(serde::Deserialize)]
struct ChatFunction {
    name: String,
    arguments: String,
}
#[derive(serde::Deserialize)]
struct ChatUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

fn convert_chat_response(data: ChatResponse) -> CompletionResponse {
    let mut content = Vec::new();
    for choice in data.choices {
        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                content.push(ContentBlock::Text { text });
            }
        }
        if let Some(calls) = choice.message.tool_calls {
            for tc in calls {
                if let Ok(input) = serde_json::from_str(&tc.function.arguments) {
                    content.push(ContentBlock::ToolUse {
                        id: tc.id,
                        name: tc.function.name,
                        input,
                    });
                }
            }
        }
    }
    let usage = data.usage.map(|u| Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
    });
    CompletionResponse {
        content,
        stop_reason: None,
        usage,
    }
}

#[derive(serde::Deserialize)]
struct ChatChunk {
    choices: Vec<ChatChunkChoice>,
    usage: Option<ChatUsage>,
}
#[derive(serde::Deserialize)]
struct ChatChunkChoice {
    delta: ChatChunkDelta,
    finish_reason: Option<String>,
}
#[derive(serde::Deserialize)]
struct ChatChunkDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ChatChunkToolCall>>,
}
#[derive(serde::Deserialize)]
struct ChatChunkToolCall {
    index: Option<u32>,
    id: Option<String>,
    function: Option<ChatChunkFunction>,
}
#[derive(serde::Deserialize)]
struct ChatChunkFunction {
    name: Option<String>,
    arguments: Option<String>,
}

fn convert_chunk(chunk: ChatChunk) -> StreamEvent {
    let mut delta = None;
    let mut content_block = None;
    let mut stop_reason = None;

    for choice in chunk.choices {
        if let Some(ref c) = choice.delta.content {
            if !c.is_empty() {
                delta = choice.delta.content.clone();
            }
        }
        if let Some(ref calls) = choice.delta.tool_calls {
            for tc in calls {
                let args = tc
                    .function
                    .as_ref()
                    .and_then(|f| f.arguments.clone())
                    .unwrap_or_default();
                let args_json = serde_json::from_str::<serde_json::Value>(&args)
                    .unwrap_or(serde_json::json!({}));
                content_block = Some(ContentBlock::ToolUse {
                    id: tc.id.clone().unwrap_or_default(),
                    name: tc
                        .function
                        .as_ref()
                        .and_then(|f| f.name.clone())
                        .unwrap_or_default(),
                    input: args_json,
                });
            }
        }
        if let Some(ref reason) = choice.finish_reason {
            stop_reason = Some(reason.clone());
        }
    }

    let usage = chunk.usage.map(|u| Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
    });
    StreamEvent {
        delta,
        content_block,
        stop_reason,
        usage,
    }
}
