use super::{CompletionModel, CompletionRequest, CompletionResponse, OpenAICompatProvider, StreamEvent, context_window_for_model};
use crate::config::GroqConfig;

fn groq_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("llama-3.1") || m.contains("llama-3.2") || m.contains("llama-3.3") || m.contains("llama3.1") || m.contains("llama3.2") || m.contains("llama3.3") { return 128_000; }
    if m.contains("llama") { return 8_192; }
    if m.contains("mixtral") { return 32_768; }
    if m.contains("gemma") { return 8_192; }
    context_window_for_model(model)
}

#[derive(Debug, Clone)]
pub struct GroqProvider(OpenAICompatProvider);

impl GroqProvider {
    pub fn new(config: GroqConfig) -> Self {
        let ctx = groq_context_window(&config.model);
        Self(OpenAICompatProvider::with_context_window("groq", crate::config::OpenAIConfig {
            model: config.model,
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key_env: config.api_key_env,
            api_key: config.api_key,
            context_window: None,
        }, ctx))
    }
}

#[async_trait::async_trait]
impl CompletionModel for GroqProvider {
    fn provider_name(&self) -> &'static str { "groq" }
    fn context_window(&self) -> u64 { self.0.context_window() }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResponse> { self.0.complete(req).await }
    async fn complete_stream(&self, req: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> { self.0.complete_stream(req).await }
}
