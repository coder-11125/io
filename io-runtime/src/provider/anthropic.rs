use super::{CompletionModel, CompletionRequest, CompletionResponse, ContentBlock, Role, StreamEvent, Usage};
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
        if let Some(ref key) = self.config.api_key { return Ok(key.clone()); }
        let env_var = self.config.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY");
        std::env::var(env_var).map_err(|_| anyhow::anyhow!("missing {env_var} environment variable"))
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.config.base_url.trim_end_matches('/'), path)
    }
}

#[async_trait::async_trait]
impl CompletionModel for AnthropicProvider {
    fn provider_name(&self) -> &'static str { "anthropic" }

    fn context_window(&self) -> u64 { super::context_window_for_model(&self.config.model) }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let api_key = self.api_key()?;

        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": request.max_tokens.unwrap_or(8192),
            "messages": build_messages(&request),
        });

        if let Some(temp) = request.temperature { body["temperature"] = serde_json::json!(temp); }
        if !request.tools.is_empty() {
            body["tools"] = serde_json::json!(request.tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.input_schema,
            })).collect::<Vec<_>>());
        }
        if let Some(ref system) = request.system_prompt { body["system"] = serde_json::json!(system); }

        let resp = self.client
            .post(self.url("messages"))
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API error ({status}): {text}");
        }

        let data: AnthropicResponse = resp.json().await?;
        Ok(convert_response(data))
    }

    async fn complete_stream(&self, _request: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        anyhow::bail!("Anthropic streaming not yet implemented")
    }
}

pub(crate) fn build_messages(request: &CompletionRequest) -> Vec<serde_json::Value> {
    request.messages.iter().filter_map(|msg| {
        if matches!(msg.role, Role::System) { return None; }
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "user",
            _ => "user",
        };

        let content: Vec<serde_json::Value> = msg.content.iter().map(|block| match block {
            ContentBlock::Text { text } => serde_json::json!({ "type": "text", "text": text }),
            ContentBlock::ToolUse { id, name, input } => serde_json::json!({
                "type": "tool_use", "id": id, "name": name, "input": input,
            }),
            ContentBlock::ToolResult { tool_use_id, content, is_error } => serde_json::json!({
                "type": "tool_result", "tool_use_id": tool_use_id, "content": content,
                "is_error": is_error.unwrap_or(false),
            }),
        }).collect();

        Some(serde_json::json!({ "role": role, "content": content }))
    }).collect()
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
    #[serde(rename = "text")] Text { text: String },
    #[serde(rename = "tool_use")] ToolUse { id: String, name: String, input: serde_json::Value },
}

#[derive(serde::Deserialize)]
struct AnthropicUsage { input_tokens: u32, output_tokens: u32 }

pub(crate) fn parse_and_convert_response(text: &str) -> anyhow::Result<CompletionResponse> {
    let data: AnthropicResponse = serde_json::from_str(text)?;
    Ok(convert_response(data))
}

fn convert_response(data: AnthropicResponse) -> CompletionResponse {
    let content = data.content.into_iter().map(|block| match block {
        AnthropicContentBlock::Text { text } => ContentBlock::Text { text },
        AnthropicContentBlock::ToolUse { id, name, input } => ContentBlock::ToolUse { id, name, input },
    }).collect();

    let usage = data.usage.map(|u| Usage { input_tokens: u.input_tokens, output_tokens: u.output_tokens });
    CompletionResponse { content, stop_reason: data.stop_reason, usage }
}
