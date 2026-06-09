use super::{CompletionModel, CompletionRequest, CompletionResponse, OpenAICompatProvider, StreamEvent, context_window_for_model};
use crate::config::DeepSeekConfig;

/// DeepSeek native models support 1M context for V4 series.
fn deepseek_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("v4") || m.contains("deepseek-v4") || m.contains("deepseek-v3") { return 1_000_000; }
    if m.contains("deepseek-r1") || m.contains("deepseek-reasoner") { return 128_000; }
    context_window_for_model(model)
}

#[derive(Debug, Clone)]
pub struct DeepSeekProvider(OpenAICompatProvider);

impl DeepSeekProvider {
    pub fn new(config: DeepSeekConfig) -> Self {
        let ctx = deepseek_context_window(&config.model);
        Self(OpenAICompatProvider::with_context_window("deepseek", crate::config::OpenAIConfig {
            model: config.model,
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key_env: config.api_key_env,
            api_key: config.api_key,
            context_window: None,
        }, ctx))
    }
}

#[async_trait::async_trait]
impl CompletionModel for DeepSeekProvider {
    fn provider_name(&self) -> &'static str { "deepseek" }
    fn context_window(&self) -> u64 { self.0.context_window() }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResponse> { self.0.complete(req).await }
    async fn complete_stream(&self, req: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> { self.0.complete_stream(req).await }
}
