use super::{CompletionModel, CompletionRequest, CompletionResponse, OpenAICompatProvider, StreamEvent, context_window_for_model};
use crate::config::OllamaConfig;

fn ollama_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    if m.contains("llama-3.1") || m.contains("llama-3.2") || m.contains("llama-3.3") || m.contains("llama3.1") || m.contains("llama3.2") || m.contains("llama3.3") { return 128_000; }
    if m.contains("deepseek") { return 128_000; }
    if m.contains("mistral") { return 32_768; }
    if m.contains("qwen2") || m.contains("qwen-2") { return 128_000; }
    if m.contains("gemma") { return 8_192; }
    context_window_for_model(model)
}

#[derive(Debug, Clone)]
pub struct OllamaProvider(OpenAICompatProvider);

impl OllamaProvider {
    pub fn new(config: OllamaConfig) -> Self {
        let endpoint = config.endpoint
            .or_else(|| std::env::var("OLLAMA_HOST").ok().map(|h| format!("{}/v1", h.trim_end_matches('/'))))
            .unwrap_or_else(|| "http://localhost:11434/v1".to_string());
        let ctx = ollama_context_window(&config.model);
        Self(OpenAICompatProvider::with_context_window("ollama", crate::config::OpenAIConfig {
            model: config.model,
            base_url: endpoint,
            api_key_env: None,
            api_key: Some("ollama".to_string()),
            context_window: None,
        }, ctx))
    }
}

#[async_trait::async_trait]
impl CompletionModel for OllamaProvider {
    fn provider_name(&self) -> &'static str { "ollama" }
    fn context_window(&self) -> u64 { self.0.context_window() }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResponse> { self.0.complete(req).await }
    async fn complete_stream(&self, req: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> { self.0.complete_stream(req).await }
}
