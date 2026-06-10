use super::{
    context_window_for_model, CompletionModel, CompletionRequest, CompletionResponse,
    OpenAICompatProvider, StreamEvent,
};
use crate::config::OpenCodeGoConfig;

fn opencode_go_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("claude") {
        return 200_000;
    }
    if m.contains("deepseek") {
        return 200_000;
    }
    if m.contains("gpt-5") {
        return 200_000;
    }
    if m.contains("gpt-4o") {
        return 128_000;
    }
    if m.contains("gemini") {
        return 1_000_000;
    }
    if m.contains("qwen") {
        return 1_000_000;
    }
    context_window_for_model(model)
}

#[derive(Debug, Clone)]
pub struct OpenCodeGoProvider(OpenAICompatProvider);

impl OpenCodeGoProvider {
    pub fn new(config: OpenCodeGoConfig) -> Self {
        let ctx = opencode_go_context_window(&config.model);
        Self(OpenAICompatProvider::with_context_window(
            "opencode_go",
            crate::config::OpenAIConfig {
                model: config.model,
                base_url: "https://opencode.ai/api/v1".to_string(),
                api_key_env: config.api_key_env,
                api_key: config.api_key,
                context_window: None,
            },
            ctx,
        ))
    }
}

#[async_trait::async_trait]
impl CompletionModel for OpenCodeGoProvider {
    fn provider_name(&self) -> &'static str {
        "opencode_go"
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
