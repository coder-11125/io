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

    fn supports_thinking(&self) -> bool {
        is_reasoning_model(&self.config.model)
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
                                                usage: Some(u.into()),
                                            }))
                                            .await
                                            .is_err()
                                        {
                                            break 'outer;
                                        }
                                    }
                                    for choice in chunk.choices {
                                        if let Some(r) = choice.delta.reasoning_text() {
                                            if tx
                                                .send(Ok(StreamEvent::thinking_delta(r)))
                                                .await
                                                .is_err()
                                            {
                                                break 'outer;
                                            }
                                        }
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
    let reasoning = is_reasoning_model(&config.model);
    if let Some(max_tokens) = request.max_tokens {
        // OpenAI's reasoning models reject `max_tokens`.
        let key = if reasoning {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body[key] = serde_json::json!(max_tokens);
    }
    if let Some(temp) = request.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let (true, Some(thinking)) = (reasoning, request.thinking) {
        body["reasoning_effort"] = serde_json::json!(reasoning_effort(thinking.budget_tokens));
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

/// OpenAI models that reason before answering (o-series, GPT-5) and so take
/// `reasoning_effort` / `max_completion_tokens`. Provider prefixes such as
/// OpenRouter's `openai/` are ignored.
pub(crate) fn is_reasoning_model(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model).to_lowercase();
    (m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.starts_with("gpt-5"))
        && !m.contains("chat")
}

/// Map a thinking-token budget onto OpenAI's coarse effort levels, which
/// line up with the `think` / `think hard` / `ultrathink` budget tiers.
fn reasoning_effort(budget_tokens: u32) -> &'static str {
    match budget_tokens {
        0..=4_000 => "low",
        4_001..=12_000 => "medium",
        _ => "high",
    }
}

/// An `image_url` content part carrying an inline base64 image.
fn image_part(media_type: &str, data: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{media_type};base64,{data}") },
    })
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
                    // `role: tool` messages are text-only, so images a tool
                    // returned (e.g. `read` on a screenshot) follow as a
                    // user message.
                    let images: Vec<serde_json::Value> = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Image { media_type, data } => {
                                Some(image_part(media_type, data))
                            }
                            _ => None,
                        })
                        .collect();
                    if !images.is_empty() {
                        let mut parts = vec![
                            serde_json::json!({"type": "text", "text": "Image(s) returned by the tool call above:"}),
                        ];
                        parts.extend(images);
                        out.push(serde_json::json!({ "role": "user", "content": parts }));
                    }
                } else {
                    let content: Vec<serde_json::Value> = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => {
                                Some(serde_json::json!({"type": "text", "text": text}))
                            }
                            ContentBlock::Image { media_type, data } => {
                                Some(image_part(media_type, data))
                            }
                            _ => None,
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
    /// Present on OpenAI and most compatible providers; absent otherwise.
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}
#[derive(serde::Deserialize)]
struct PromptTokensDetails {
    /// How much of `prompt_tokens` was served from the automatic prompt cache.
    #[serde(default)]
    cached_tokens: Option<u32>,
}

impl From<&ChatUsage> for Usage {
    fn from(u: &ChatUsage) -> Self {
        Usage::from_prompt_total(
            u.prompt_tokens,
            u.completion_tokens,
            u.prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
        )
    }
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
    let usage = data.usage.as_ref().map(Usage::from);
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
    /// Reasoning text streamed by DeepSeek-style providers…
    #[serde(default)]
    reasoning_content: Option<String>,
    /// …or under this name by OpenRouter and others.
    #[serde(default)]
    reasoning: Option<String>,
}

impl ChatChunkDelta {
    /// The reasoning text carried by this delta, if any.
    fn reasoning_text(&self) -> Option<String> {
        self.reasoning_content
            .as_deref()
            .or(self.reasoning.as_deref())
            .filter(|r| !r.is_empty())
            .map(str::to_string)
    }
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
        if delta.is_none() && content_block.is_none() {
            if let Some(r) = choice.delta.reasoning_text() {
                content_block = Some(ContentBlock::Thinking {
                    thinking: r,
                    signature: None,
                });
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

    let usage = chunk.usage.as_ref().map(Usage::from);
    StreamEvent {
        delta,
        content_block,
        stop_reason,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_response_splits_cached_tokens_from_the_prompt() {
        let resp = parse_and_convert_chat_response(
            r#"{"choices":[{"message":{"content":"hi"}}],
                "usage":{"prompt_tokens":2006,"completion_tokens":300,
                         "prompt_tokens_details":{"cached_tokens":1920}}}"#,
        )
        .unwrap();
        let u = resp.usage.unwrap();
        assert_eq!(u.input_tokens, 86);
        assert_eq!(u.cache_read_tokens, 1920);
        assert_eq!(u.output_tokens, 300);
        assert_eq!(u.context_tokens(), 2006, "context is the whole prompt");
    }

    #[test]
    fn streaming_chunk_splits_cached_tokens_too() {
        let ev = parse_and_convert_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":500,"completion_tokens":20,
                "prompt_tokens_details":{"cached_tokens":400}}}"#,
        )
        .unwrap();
        let u = ev.usage.unwrap();
        assert_eq!((u.input_tokens, u.cache_read_tokens), (100, 400));
    }

    #[test]
    fn usage_without_details_is_all_uncached() {
        // Providers that don't report cache info (or send null/empty details).
        for usage_json in [
            r#"{"prompt_tokens":300,"completion_tokens":10}"#,
            r#"{"prompt_tokens":300,"completion_tokens":10,"prompt_tokens_details":null}"#,
            r#"{"prompt_tokens":300,"completion_tokens":10,"prompt_tokens_details":{}}"#,
            r#"{"prompt_tokens":300,"completion_tokens":10,
                "prompt_tokens_details":{"cached_tokens":null}}"#,
        ] {
            let body = format!(r#"{{"choices":[],"usage":{usage_json}}}"#);
            let u = parse_and_convert_chat_response(&body)
                .unwrap()
                .usage
                .unwrap();
            assert_eq!(
                (u.input_tokens, u.cache_read_tokens),
                (300, 0),
                "{usage_json}"
            );
        }
    }

    fn request_with(model_thinking: Option<u32>, max_tokens: u32) -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            }],
            max_tokens: Some(max_tokens),
            thinking: model_thinking.map(|b| crate::provider::ThinkingRequest { budget_tokens: b }),
            ..Default::default()
        }
    }

    #[test]
    fn reasoning_models_get_effort_and_completion_token_limit() {
        let body = build_chat_body_with_model("o3-mini", &request_with(Some(10_000), 2048), false);
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["max_completion_tokens"], 2048);
        assert!(body.get("max_tokens").is_none());

        for (budget, effort) in [
            (1_000, "low"),
            (4_000, "low"),
            (8_000, "medium"),
            (31_999, "high"),
        ] {
            let body = build_chat_body_with_model("gpt-5", &request_with(Some(budget), 100), false);
            assert_eq!(body["reasoning_effort"], effort, "budget {budget}");
        }
    }

    #[test]
    fn non_reasoning_models_ignore_thinking_and_keep_max_tokens() {
        let body = build_chat_body_with_model("gpt-4o", &request_with(Some(31_999), 2048), false);
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["max_tokens"], 2048);

        // A reasoning model with thinking off leaves the provider default.
        let body = build_chat_body_with_model("o3", &request_with(None, 2048), false);
        assert!(body.get("reasoning_effort").is_none());
        // Provider prefixes (OpenRouter) are looked through.
        assert!(is_reasoning_model("openai/o4-mini"));
        assert!(!is_reasoning_model("gpt-5-chat-latest"));
        assert!(!is_reasoning_model("deepseek-chat"));
    }

    #[test]
    fn user_images_become_image_url_parts() {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "look".into(),
                },
                ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "QUJD".into(),
                },
            ],
        }];
        let out = convert_messages(&msgs);
        assert_eq!(out[0]["content"][1]["type"], "image_url");
        assert_eq!(
            out[0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,QUJD"
        );
    }

    #[test]
    fn tool_result_images_follow_the_tool_messages_as_a_user_message() {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "Image file".into(),
                    is_error: None,
                },
                ContentBlock::Image {
                    media_type: "image/jpeg".into(),
                    data: "QUJD".into(),
                },
            ],
        }];
        let out = convert_messages(&msgs);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[1]["content"][1]["type"], "image_url");
    }

    #[test]
    fn streaming_chunk_surfaces_reasoning_text_as_a_thinking_delta() {
        for field in ["reasoning_content", "reasoning"] {
            let ev = parse_and_convert_chunk(&format!(
                r#"{{"choices":[{{"delta":{{"{field}":"pondering"}}}}]}}"#
            ))
            .unwrap();
            assert!(ev.delta.is_none());
            assert!(matches!(
                ev.content_block,
                Some(ContentBlock::Thinking { ref thinking, signature: None }) if thinking == "pondering"
            ));
        }
    }
}
