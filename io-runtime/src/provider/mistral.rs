use super::{CompletionModel, CompletionRequest, CompletionResponse, OpenAICompatProvider, StreamEvent, context_window_for_model};
use crate::config::MistralConfig;

fn mistral_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("mistral-large") { return 128_000; }
    if m.contains("codestral") { return 256_000; }
    if m.contains("ministral") { return 32_768; }
    if m.contains("pixtral") { return 128_000; }
    if m.contains("mistral") { return 32_768; }
    context_window_for_model(model)
}

#[derive(Debug, Clone)]
pub struct MistralProvider(OpenAICompatProvider);

impl MistralProvider {
    pub fn new(config: MistralConfig) -> Self {
        let ctx = mistral_context_window(&config.model);
        Self(OpenAICompatProvider::with_context_window("mistral", crate::config::OpenAIConfig {
            model: config.model,
            base_url: "https://api.mistral.ai/v1".to_string(),
            api_key_env: config.api_key_env,
            api_key: config.api_key,
            context_window: None,
        }, ctx))
    }
}

#[async_trait::async_trait]
impl CompletionModel for MistralProvider {
    fn provider_name(&self) -> &'static str { "mistral" }
    fn context_window(&self) -> u64 { self.0.context_window() }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResponse> { self.0.complete(req).await }
    async fn complete_stream(&self, req: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> { self.0.complete_stream(req).await }
}
