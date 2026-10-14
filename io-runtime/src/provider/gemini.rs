use super::{
    CompletionModel, CompletionRequest, CompletionResponse, ContentBlock, Role, StreamEvent, Usage,
};
use crate::config::GeminiConfig;

#[derive(Debug, Clone)]
pub struct GeminiProvider {
    config: GeminiConfig,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(config: GeminiConfig) -> Self {
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
            .unwrap_or("GEMINI_API_KEY");
        std::env::var(env_var)
            .map_err(|_| anyhow::anyhow!("missing {env_var} environment variable"))
    }

    fn url(&self, path: &str, api_key: &str) -> String {
        format!(
            "{}/{}?key={}",
            self.config.base_url.trim_end_matches('/'),
            path,
            api_key
        )
    }
}

#[async_trait::async_trait]
impl CompletionModel for GeminiProvider {
    fn provider_name(&self) -> &'static str {
        "gemini"
    }

    fn context_window(&self) -> u64 {
        self.config
            .context_window
            .unwrap_or_else(|| super::context_window_for_model(&self.config.model))
    }

    fn supports_thinking(&self) -> bool {
        supports_thinking(&self.config.model)
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let api_key = self.api_key()?;
        let body = build_request_body(&request, &self.config.model);
        let url = self.url(
            &format!("models/{}:generateContent", self.config.model),
            &api_key,
        );

        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Gemini", resp).await);
        }

        let data: GeminiResponse = resp.json().await?;
        Ok(convert_response(data))
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        let api_key = self.api_key()?;
        let body = build_request_body(&request, &self.config.model);
        let url = self.url(
            &format!("models/{}:streamGenerateContent", self.config.model),
            &api_key,
        ) + "&alt=sse";

        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Gemini", resp).await);
        }

        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
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
                            match serde_json::from_str::<GeminiResponse>(data) {
                                Ok(chunk) => {
                                    for event in convert_response_to_stream_events(chunk) {
                                        if tx.send(Ok(event)).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(anyhow::anyhow!("parse error: {e}"))).await;
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("stream error: {e}"))).await;
                        return;
                    }
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
        });

        Ok(rx)
    }
}

/// Gemini models with controllable thinking (2.5 and later).
fn supports_thinking(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("gemini-2.5") || m.contains("gemini-3")
}

fn build_request_body(request: &CompletionRequest, model: &str) -> serde_json::Value {
    let contents = build_contents(request);
    let mut body = serde_json::json!({ "contents": contents });

    if let Some(system) = request.system_text() {
        body["systemInstruction"] = serde_json::json!({ "parts": [{ "text": system }] });
    }

    if !request.tools.is_empty() {
        let declarations: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        body["tools"] = serde_json::json!([{ "functionDeclarations": declarations }]);
    }

    let mut gen_config = serde_json::Map::new();
    if let Some(max_tokens) = request.max_tokens {
        gen_config.insert("maxOutputTokens".to_string(), serde_json::json!(max_tokens));
    }
    if let Some(temp) = request.temperature {
        gen_config.insert("temperature".to_string(), serde_json::json!(temp));
    }
    if let (Some(thinking), true) = (request.thinking, supports_thinking(model)) {
        gen_config.insert(
            "thinkingConfig".to_string(),
            serde_json::json!({
                "thinkingBudget": thinking.budget_tokens.clamp(128, 24_576),
                "includeThoughts": true,
            }),
        );
    }
    if !gen_config.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(gen_config);
    }

    body
}

fn build_contents(request: &CompletionRequest) -> Vec<serde_json::Value> {
    let mut contents = Vec::new();

    for msg in &request.messages {
        if matches!(msg.role, Role::System) {
            continue;
        }

        let role = match msg.role {
            Role::Assistant => "model",
            _ => "user",
        };

        let parts: Vec<serde_json::Value> = msg
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(serde_json::json!({ "text": text })),
                ContentBlock::ToolUse { name, input, .. } => Some(serde_json::json!({
                    "functionCall": { "name": name, "args": input }
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let fn_name = tool_use_id.strip_prefix("tool_").unwrap_or(tool_use_id);
                    Some(serde_json::json!({
                        "functionResponse": {
                            "name": fn_name,
                            "response": { "output": content }
                        }
                    }))
                }
                ContentBlock::Image { media_type, data } => Some(serde_json::json!({
                    "inlineData": { "mimeType": media_type, "data": data }
                })),
                // Gemini thoughts are display-only and never sent back.
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => None,
            })
            .collect();

        // A message that was only thoughts has nothing left to send.
        if parts.is_empty() {
            continue;
        }
        contents.push(serde_json::json!({ "role": role, "parts": parts }));
    }

    contents
}

#[derive(serde::Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(serde::Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(serde::Deserialize)]
struct GeminiPart {
    text: Option<String>,
    /// True when `text` is a thought summary rather than answer text.
    #[serde(default)]
    thought: Option<bool>,
    #[serde(rename = "functionCall")]
    function_call: Option<GeminiFunctionCall>,
}

#[derive(serde::Deserialize)]
struct GeminiFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct GeminiUsage {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u32>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u32>,
    /// How much of `promptTokenCount` came from Gemini's implicit cache.
    #[serde(rename = "cachedContentTokenCount", default)]
    cached_content_token_count: Option<u32>,
}

impl From<&GeminiUsage> for Usage {
    fn from(u: &GeminiUsage) -> Self {
        Usage::from_prompt_total(
            u.prompt_token_count.unwrap_or(0),
            u.candidates_token_count.unwrap_or(0),
            u.cached_content_token_count,
        )
    }
}

fn convert_response(data: GeminiResponse) -> CompletionResponse {
    let mut content = Vec::new();
    let mut stop_reason = None;

    for candidate in data.candidates.unwrap_or_default() {
        if let Some(reason) = candidate.finish_reason {
            stop_reason = Some(reason.to_lowercase());
        }
        if let Some(c) = candidate.content {
            for part in c.parts {
                if let Some(text) = part.text {
                    if !text.is_empty() {
                        content.push(if part.thought == Some(true) {
                            ContentBlock::Thinking {
                                thinking: text,
                                signature: None,
                            }
                        } else {
                            ContentBlock::Text { text }
                        });
                    }
                }
                if let Some(fc) = part.function_call {
                    content.push(ContentBlock::ToolUse {
                        id: format!("tool_{}", fc.name),
                        name: fc.name,
                        input: fc.args,
                    });
                }
            }
        }
    }

    let usage = data.usage_metadata.as_ref().map(Usage::from);

    CompletionResponse {
        content,
        stop_reason,
        usage,
    }
}

/// One streamed chunk as agent events: thought summaries first (display
/// only), then the answer text, then each tool call as its own event, and
/// finally a bare event carrying the stop reason and usage. A chunk can hold
/// several text parts or parallel function calls, so none may overwrite
/// another.
fn convert_response_to_stream_events(data: GeminiResponse) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut stop_reason = None;
    let usage = data.usage_metadata.as_ref().map(Usage::from);

    for candidate in data.candidates.unwrap_or_default() {
        if let Some(reason) = candidate.finish_reason {
            if reason != "FINISH_REASON_UNSPECIFIED" {
                stop_reason = Some(reason.to_lowercase());
            }
        }
        if let Some(c) = candidate.content {
            for part in c.parts {
                if let Some(t) = part.text {
                    if t.is_empty() {
                        // nothing to show
                    } else if part.thought == Some(true) {
                        events.push(StreamEvent::thinking_delta(t));
                    } else {
                        text.push_str(&t);
                    }
                }
                if let Some(fc) = part.function_call {
                    calls.push(ContentBlock::ToolUse {
                        id: format!("tool_{}", fc.name),
                        name: fc.name,
                        input: fc.args,
                    });
                }
            }
        }
    }

    if !text.is_empty() {
        events.push(StreamEvent {
            delta: Some(text),
            content_block: None,
            stop_reason: None,
            usage: None,
        });
    }
    for call in calls {
        events.push(StreamEvent {
            delta: None,
            content_block: Some(call),
            stop_reason: None,
            usage: None,
        });
    }
    events.push(StreamEvent {
        delta: None,
        content_block: None,
        stop_reason,
        usage,
    });
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Message;

    fn usage_of(json: &str) -> Usage {
        convert_response(serde_json::from_str(json).unwrap())
            .usage
            .unwrap()
    }

    #[test]
    fn cached_content_tokens_are_split_out_of_the_prompt_count() {
        let u = usage_of(
            r#"{"candidates":[],"usageMetadata":{"promptTokenCount":1000,
                "candidatesTokenCount":40,"cachedContentTokenCount":750}}"#,
        );
        assert_eq!(u.input_tokens, 250);
        assert_eq!(u.cache_read_tokens, 750);
        assert_eq!(u.output_tokens, 40);
        assert_eq!(u.context_tokens(), 1000, "context is the whole prompt");
    }

    #[test]
    fn usage_without_cached_content_is_all_uncached() {
        let u = usage_of(
            r#"{"candidates":[],"usageMetadata":{"promptTokenCount":1000,
                "candidatesTokenCount":40}}"#,
        );
        assert_eq!((u.input_tokens, u.cache_read_tokens), (1000, 0));
    }

    #[test]
    fn streaming_chunk_reports_cached_tokens() {
        let chunk: GeminiResponse = serde_json::from_str(
            r#"{"usageMetadata":{"promptTokenCount":600,"candidatesTokenCount":5,
                "cachedContentTokenCount":500}}"#,
        )
        .unwrap();
        let u = convert_response_to_stream_events(chunk)
            .pop()
            .unwrap()
            .usage
            .unwrap();
        assert_eq!((u.input_tokens, u.cache_read_tokens), (100, 500));
    }

    #[test]
    fn system_message_is_sent_as_system_instruction() {
        // Regression: the agent supplies its prompt as a Role::System
        // message, which used to be dropped on the way to Gemini.
        let request = CompletionRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: vec![ContentBlock::Text {
                        text: "You are a test agent.".to_string(),
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "hello".to_string(),
                    }],
                },
            ],
            ..Default::default()
        };
        let body = build_request_body(&request, "gemini-2.5-pro");
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are a test agent."
        );
        // The system text is not duplicated into the conversation contents.
        assert_eq!(body["contents"].as_array().unwrap().len(), 1);
    }

    fn chunk(json: &str) -> GeminiResponse {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn thinking_config_is_sent_only_for_models_that_support_it() {
        let request = CompletionRequest {
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            }],
            thinking: Some(crate::provider::ThinkingRequest {
                budget_tokens: 10_000,
            }),
            ..Default::default()
        };
        let body = build_request_body(&request, "gemini-2.5-flash");
        let cfg = &body["generationConfig"]["thinkingConfig"];
        assert_eq!(cfg["thinkingBudget"], 10_000);
        assert_eq!(cfg["includeThoughts"], true);

        let body = build_request_body(&request, "gemini-1.5-pro");
        assert!(body.get("generationConfig").is_none());

        let off = CompletionRequest {
            thinking: None,
            ..request
        };
        assert!(build_request_body(&off, "gemini-2.5-pro")
            .get("generationConfig")
            .is_none());
    }

    #[test]
    fn thought_parts_stream_as_thinking_and_text_parts_concatenate() {
        let events = convert_response_to_stream_events(chunk(
            r#"{"candidates":[{"content":{"parts":[
                {"text":"weighing options","thought":true},
                {"text":"Hello "},{"text":"world"},
                {"functionCall":{"name":"read","args":{"a":1}}},
                {"functionCall":{"name":"glob","args":{"b":2}}}
            ]},"finishReason":"STOP"}]}"#,
        ));
        assert!(matches!(
            &events[0].content_block,
            Some(ContentBlock::Thinking { thinking, signature: None }) if thinking == "weighing options"
        ));
        assert_eq!(events[1].delta.as_deref(), Some("Hello world"));
        // Parallel calls each survive.
        assert!(
            matches!(&events[2].content_block, Some(ContentBlock::ToolUse { name, .. }) if name == "read")
        );
        assert!(
            matches!(&events[3].content_block, Some(ContentBlock::ToolUse { name, .. }) if name == "glob")
        );
        assert_eq!(events.last().unwrap().stop_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn blocking_response_separates_thoughts_from_answer_text() {
        let resp = convert_response(chunk(
            r#"{"candidates":[{"content":{"parts":[
                {"text":"hmm","thought":true},{"text":"answer"}]}}]}"#,
        ));
        assert!(matches!(&resp.content[0], ContentBlock::Thinking { .. }));
        assert!(matches!(&resp.content[1], ContentBlock::Text { text } if text == "answer"));
    }

    #[test]
    fn images_become_inline_data_and_thinking_is_never_replayed() {
        let request = CompletionRequest {
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Thinking {
                        thinking: "x".into(),
                        signature: Some("s".into()),
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::Image {
                        media_type: "image/png".into(),
                        data: "QUJD".into(),
                    }],
                },
            ],
            ..Default::default()
        };
        let contents = build_contents(&request);
        // The thinking-only assistant message is dropped entirely.
        assert_eq!(contents.len(), 1);
        assert_eq!(
            contents[0]["parts"][0]["inlineData"]["mimeType"],
            "image/png"
        );
        assert_eq!(contents[0]["parts"][0]["inlineData"]["data"], "QUJD");
    }
}
