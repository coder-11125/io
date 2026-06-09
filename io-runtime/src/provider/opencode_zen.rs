use super::{CompletionModel, CompletionRequest, CompletionResponse, OpenAICompatProvider, StreamEvent, context_window_for_model};
use crate::config::OpenCodeZenConfig;

/// Provider-specific context window for known OpenCode Zen models.
/// These are the actual limits OpenCode Zen exposes per model.
fn opencode_zen_context_window(model: &str) -> u64 {
    let m = model.to_lowercase();
    // Claude models
    if m.contains("claude") { return 200_000; }
    // OpenAI reasoning
    if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") { return 200_000; }
    // GPT-5 family
    if m.contains("gpt-5") || m.contains("gpt-5") { return 200_000; }
    // GPT-4 family
    if m.contains("gpt-4o") || m.contains("gpt-4-turbo") { return 128_000; }
    // Gemini
    if m.contains("gemini") { return 1_000_000; }
    // DeepSeek — 200K on OpenCode Zen (vs 1M on native)
    if m.contains("deepseek") { return 200_000; }
    // Qwen
    if m.contains("qwen") { return 1_000_000; }
    // Kimi
    if m.contains("kimi") || m.starts_with("k") { return 128_000; }
    // MiniMax
    if m.contains("minimax") || m.contains("abab") { return 245_760; }
    // GLM
    if m.contains("glm") { return 128_000; }
    // Mimo
    if m.contains("mimo") { return 128_000; }
    // Grok
    if m.contains("grok") { return 131_072; }
    // Nemotron
    if m.contains("nemotron") || m.contains("llama") { return 128_000; }
    // Big Pickle
    if m.contains("big-pickle") || m.contains("big_pickle") { return 128_000; }
    // Fallback to generic model-name guess
    context_window_for_model(model)
}

#[derive(Debug, Clone)]
pub struct OpenCodeZenProvider(OpenAICompatProvider);

impl OpenCodeZenProvider {
    pub fn new(config: OpenCodeZenConfig) -> Self {
        let ctx = opencode_zen_context_window(&config.model);
        Self(OpenAICompatProvider::with_context_window("opencode_zen", crate::config::OpenAIConfig {
            model: config.model,
            base_url: "https://opencode.ai/zen/v1".to_string(),
            api_key_env: config.api_key_env,
            api_key: config.api_key,
            context_window: None,
        }, ctx))
    }
}

#[async_trait::async_trait]
impl CompletionModel for OpenCodeZenProvider {
    fn provider_name(&self) -> &'static str { "opencode_zen" }
    fn context_window(&self) -> u64 { self.0.context_window() }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResponse> { self.0.complete(req).await }
    async fn complete_stream(&self, req: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> { self.0.complete_stream(req).await }
}
