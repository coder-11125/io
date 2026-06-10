use super::{
    context_window_for_model, CompletionModel, CompletionRequest, CompletionResponse,
    OpenAICompatProvider, StreamEvent,
};
use crate::config::XAIConfig;

fn xai_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("grok") {
        return 131_072;
    }
    context_window_for_model(model)
}

#[derive(Debug, Clone)]
pub struct XAIProvider(OpenAICompatProvider);

impl XAIProvider {
    pub fn new(config: XAIConfig) -> Self {
        let ctx = xai_context_window(&config.model);
        Self(OpenAICompatProvider::with_context_window(
            "xai",
            crate::config::OpenAIConfig {
                model: config.model,
                base_url: "https://api.x.ai/v1".to_string(),
                api_key_env: config.api_key_env,
                api_key: config.api_key,
                context_window: None,
            },
            ctx,
        ))
    }
}

#[async_trait::async_trait]
impl CompletionModel for XAIProvider {
    fn provider_name(&self) -> &'static str {
        "xai"
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
