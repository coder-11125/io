use super::{CompletionModel, CompletionRequest, CompletionResponse, ContentBlock, Role, StreamEvent, Usage};
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
        if let Some(ref key) = self.config.api_key { return Ok(key.clone()); }
        let env_var = self.config.api_key_env.as_deref().unwrap_or("GEMINI_API_KEY");
        std::env::var(env_var).map_err(|_| anyhow::anyhow!("missing {env_var} environment variable"))
    }

    fn url(&self, path: &str, api_key: &str) -> String {
        format!("{}/{}?key={}", self.config.base_url.trim_end_matches('/'), path, api_key)
    }
}

#[async_trait::async_trait]
impl CompletionModel for GeminiProvider {
    fn provider_name(&self) -> &'static str { "gemini" }

    fn context_window(&self) -> u64 { super::context_window_for_model(&self.config.model) }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let api_key = self.api_key()?;
        let body = build_request_body(&request);
        let url = self.url(&format!("models/{}:generateContent", self.config.model), &api_key);

        let resp = self.client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gemini API error ({status}): {text}");
        }

        let data: GeminiResponse = resp.json().await?;
        Ok(convert_response(data))
    }

    async fn complete_stream(&self, request: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        let api_key = self.api_key()?;
        let body = build_request_body(&request);
        let url = self.url(&format!("models/{}:streamGenerateContent", self.config.model), &api_key) + "&alt=sse";

        let resp = self.client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gemini API error ({status}): {text}");
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
                            if line.is_empty() { continue; }
                            if !line.starts_with("data: ") { continue; }
                            let data = &line[6..];
                            match serde_json::from_str::<GeminiResponse>(data) {
                                Ok(chunk) => {
                                    if tx.send(Ok(convert_response_to_stream_event(chunk))).await.is_err() { return; }
                                }
                                Err(e) => { let _ = tx.send(Err(anyhow::anyhow!("parse error: {e}"))).await; return; }
                            }
                        }
                    }
                    Err(e) => { let _ = tx.send(Err(anyhow::anyhow!("stream error: {e}"))).await; return; }
                }
            }
            let _ = tx.send(Ok(StreamEvent { delta: None, content_block: None, stop_reason: Some("stop".to_string()), usage: None })).await;
        });

        Ok(rx)
    }
}

fn build_request_body(request: &CompletionRequest) -> serde_json::Value {
    let contents = build_contents(request);
    let mut body = serde_json::json!({ "contents": contents });

    if let Some(ref system) = request.system_prompt {
        body["systemInstruction"] = serde_json::json!({ "parts": [{ "text": system }] });
    }

    if !request.tools.is_empty() {
        let declarations: Vec<serde_json::Value> = request.tools.iter().map(|t| serde_json::json!({
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        })).collect();
        body["tools"] = serde_json::json!([{ "functionDeclarations": declarations }]);
    }

    let mut gen_config = serde_json::Map::new();
    if let Some(max_tokens) = request.max_tokens {
        gen_config.insert("maxOutputTokens".to_string(), serde_json::json!(max_tokens));
    }
    if let Some(temp) = request.temperature {
        gen_config.insert("temperature".to_string(), serde_json::json!(temp));
    }
    if !gen_config.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(gen_config);
    }

    body
}

fn build_contents(request: &CompletionRequest) -> Vec<serde_json::Value> {
    let mut contents = Vec::new();

    for msg in &request.messages {
        if matches!(msg.role, Role::System) { continue; }

        let role = match msg.role {
            Role::Assistant => "model",
            _ => "user",
        };

        let parts: Vec<serde_json::Value> = msg.content.iter().map(|block| match block {
            ContentBlock::Text { text } => serde_json::json!({ "text": text }),
            ContentBlock::ToolUse { name, input, .. } => serde_json::json!({
                "functionCall": { "name": name, "args": input }
            }),
            ContentBlock::ToolResult { tool_use_id, content, .. } => {
                let fn_name = tool_use_id.strip_prefix("tool_").unwrap_or(tool_use_id);
                serde_json::json!({
                    "functionResponse": {
                        "name": fn_name,
                        "response": { "output": content }
                    }
                })
            }
        }).collect();

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
                    if !text.is_empty() { content.push(ContentBlock::Text { text }); }
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

    let usage = data.usage_metadata.map(|u| Usage {
        input_tokens: u.prompt_token_count.unwrap_or(0),
        output_tokens: u.candidates_token_count.unwrap_or(0),
    });

    CompletionResponse { content, stop_reason, usage }
}

fn convert_response_to_stream_event(data: GeminiResponse) -> StreamEvent {
    let mut delta = None;
    let mut content_block = None;
    let mut stop_reason = None;
    let mut usage = None;

    if let Some(u) = data.usage_metadata {
        usage = Some(Usage {
            input_tokens: u.prompt_token_count.unwrap_or(0),
            output_tokens: u.candidates_token_count.unwrap_or(0),
        });
    }

    for candidate in data.candidates.unwrap_or_default() {
        if let Some(reason) = candidate.finish_reason {
            if reason != "FINISH_REASON_UNSPECIFIED" {
                stop_reason = Some(reason.to_lowercase());
            }
        }
        if let Some(c) = candidate.content {
            for part in c.parts {
                if let Some(text) = part.text {
                    if !text.is_empty() { delta = Some(text); }
                }
                if let Some(fc) = part.function_call {
                    content_block = Some(ContentBlock::ToolUse {
                        id: format!("tool_{}", fc.name),
                        name: fc.name,
                        input: fc.args,
                    });
                }
            }
        }
    }

    StreamEvent { delta, content_block, stop_reason, usage }
}
