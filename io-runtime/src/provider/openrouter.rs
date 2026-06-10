use super::{
    context_window_for_model, CompletionModel, CompletionRequest, CompletionResponse,
    OpenAICompatProvider, StreamEvent,
};
use crate::config::OpenRouterConfig;

fn openrouter_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("claude") {
        return 200_000;
    }
    if m.contains("gpt-4o") || m.contains("gpt-4-turbo") {
        return 128_000;
    }
    if m.contains("gpt-5") {
        return 200_000;
    }
    if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        return 200_000;
    }
    if m.contains("gemini") {
        return 1_000_000;
    }
    if m.contains("deepseek") && m.contains("v4") {
        return 1_000_000;
    }
    if m.contains("deepseek") {
        return 128_000;
    }
    context_window_for_model(model)
}

#[derive(Debug, Clone)]
pub struct OpenRouterProvider(OpenAICompatProvider);

impl OpenRouterProvider {
    pub fn new(config: OpenRouterConfig) -> Self {
        let ctx = openrouter_context_window(&config.model);
        Self(OpenAICompatProvider::with_context_window(
            "openrouter",
            crate::config::OpenAIConfig {
                model: config.model,
                base_url: "https://openrouter.ai/api/v1".to_string(),
                api_key_env: config.api_key_env,
                api_key: config.api_key,
                context_window: None,
            },
            ctx,
        ))
    }
}

#[async_trait::async_trait]
impl CompletionModel for OpenRouterProvider {
    fn provider_name(&self) -> &'static str {
        "openrouter"
    }
    fn context_window(&self) -> u64 {
        self.0.context_window()
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        self.0.complete(req).await
    }
    async fn complete_stream(
        &self,
        req: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        self.0.complete_stream(req).await
    }
}
