use std::fmt::Debug;

pub mod openai;
pub mod anthropic;
pub mod gemini;
pub mod azure;
pub mod bedrock;
pub mod groq;
pub mod openrouter;
pub mod xai;
pub mod opencode_go;
pub mod opencode_zen;
pub mod ollama;
pub mod mistral;
pub mod deepseek;

pub use openai::OpenAIProvider;
pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub use azure::AzureProvider;
pub use bedrock::BedrockProvider;
pub use groq::GroqProvider;
pub use openrouter::OpenRouterProvider;
pub use xai::XAIProvider;
pub use opencode_go::OpenCodeGoProvider;
pub use opencode_zen::OpenCodeZenProvider;
pub use ollama::OllamaProvider;
pub use mistral::MistralProvider;
pub use deepseek::DeepSeekProvider;

use crate::tools::ToolSpec;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, #[serde(skip_serializing_if = "Option::is_none")] is_error: Option<bool> },
}

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub system_prompt: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stream: bool,
}

#[derive(Debug)]
pub struct CompletionResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug)]
pub struct StreamEvent {
    pub delta: Option<String>,
    pub content_block: Option<ContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: Option<Usage>,
}

/// Infer context window size (tokens) from a model ID string.
/// Matches on well-known model families; returns 128K for anything unknown.
/// Providers call this with their configured model ID so the display is always dynamic.
pub(crate) fn context_window_for_model(model: &str) -> u64 {
    let m = model.to_lowercase();
    // Anthropic — all Claude models are 200K
    if m.starts_with("claude") || m.contains("claude") { return 200_000; }
    // OpenAI reasoning models
    if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") { return 200_000; }
    // OpenAI chat
    if m.contains("gpt-4o") || m.contains("gpt-4-turbo") { return 128_000; }
    if m.starts_with("gpt-4") { return 8_192; }
    if m.contains("gpt-3.5") { return 16_385; }
    // Gemini
    if m.contains("gemini-1.5") || m.contains("gemini-2") { return 1_000_000; }
    if m.starts_with("gemini") || m.contains("gemini") { return 32_000; }
    // Mistral
    if m.contains("mistral-large") || m.contains("mistral-medium") || m.contains("mixtral") { return 32_768; }
    if m.contains("mistral") { return 32_768; }
    // Meta Llama
    if m.contains("llama-3.1") || m.contains("llama-3.2") || m.contains("llama-3.3") || m.contains("llama3.1") || m.contains("llama3.2") || m.contains("llama3.3") { return 128_000; }
    if m.contains("llama") { return 8_192; }
    // xAI Grok
    if m.contains("grok") { return 131_072; }
    // DeepSeek
    if m.contains("deepseek") { return 128_000; }
    // Amazon Bedrock Titan / Nova
    if m.contains("nova") { return 300_000; }
    if m.contains("titan") { return 32_000; }
    // Default
    128_000
}

#[async_trait::async_trait]
pub trait CompletionModel: Debug + Send + Sync {
    fn provider_name(&self) -> &'static str;
    /// Context window size in tokens for the configured model. Defaults to 128K.
    fn context_window(&self) -> u64 { 128_000 }
    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse>;
    async fn complete_stream(&self, request: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>>;
}

#[derive(Debug, Clone)]
pub(crate) struct OpenAICompatProvider {
    name: &'static str,
    inner: OpenAIProvider,
    context_window: u64,
}

impl OpenAICompatProvider {
    pub fn with_context_window(
        name: &'static str,
        config: crate::config::OpenAIConfig,
        provider_ctx: u64,
    ) -> Self {
        let ctx = config.context_window.unwrap_or(provider_ctx);
        Self { name, inner: OpenAIProvider::new(config), context_window: ctx }
    }
}

#[async_trait::async_trait]
impl CompletionModel for OpenAICompatProvider {
    fn provider_name(&self) -> &'static str { self.name }
    fn context_window(&self) -> u64 { self.context_window }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        self.inner.complete(req).await
    }

    async fn complete_stream(&self, req: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        self.inner.complete_stream(req).await
    }
}

#[derive(Debug)]
pub enum ProviderKind {
    OpenAI(OpenAIProvider),
    Anthropic(AnthropicProvider),
    Gemini(GeminiProvider),
    Azure(AzureProvider),
    Bedrock(BedrockProvider),
    Groq(GroqProvider),
    OpenRouter(OpenRouterProvider),
    XAI(XAIProvider),
    OpenCodeGo(OpenCodeGoProvider),
    OpenCodeZen(OpenCodeZenProvider),
    Ollama(OllamaProvider),
    Mistral(MistralProvider),
    DeepSeek(DeepSeekProvider),
}

impl ProviderKind {
    pub fn name(&self) -> &'static str {
        match self {
            ProviderKind::OpenAI(_) => "openai",
            ProviderKind::Anthropic(_) => "anthropic",
            ProviderKind::Gemini(_) => "gemini",
            ProviderKind::Azure(_) => "azure",
            ProviderKind::Bedrock(_) => "bedrock",
            ProviderKind::Groq(_) => "groq",
            ProviderKind::OpenRouter(_) => "openrouter",
            ProviderKind::XAI(_) => "xai",
            ProviderKind::OpenCodeGo(_) => "opencode_go",
            ProviderKind::OpenCodeZen(_) => "opencode_zen",
            ProviderKind::Ollama(_) => "ollama",
            ProviderKind::Mistral(_) => "mistral",
            ProviderKind::DeepSeek(_) => "deepseek",
        }
    }
}

#[async_trait::async_trait]
impl CompletionModel for ProviderKind {
    fn provider_name(&self) -> &'static str { self.name() }

    fn context_window(&self) -> u64 {
        match self {
            ProviderKind::OpenAI(p) => p.context_window(),
            ProviderKind::Anthropic(p) => p.context_window(),
            ProviderKind::Gemini(p) => p.context_window(),
            ProviderKind::Azure(p) => p.context_window(),
            ProviderKind::Bedrock(p) => p.context_window(),
            ProviderKind::Groq(p) => p.context_window(),
            ProviderKind::OpenRouter(p) => p.context_window(),
            ProviderKind::XAI(p) => p.context_window(),
            ProviderKind::OpenCodeGo(p) => p.context_window(),
            ProviderKind::OpenCodeZen(p) => p.context_window(),
            ProviderKind::Ollama(p) => p.context_window(),
            ProviderKind::Mistral(p) => p.context_window(),
            ProviderKind::DeepSeek(p) => p.context_window(),
        }
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        match self {
            ProviderKind::OpenAI(p) => p.complete(request).await,
            ProviderKind::Anthropic(p) => p.complete(request).await,
            ProviderKind::Gemini(p) => p.complete(request).await,
            ProviderKind::Azure(p) => p.complete(request).await,
            ProviderKind::Bedrock(p) => p.complete(request).await,
            ProviderKind::Groq(p) => p.complete(request).await,
            ProviderKind::OpenRouter(p) => p.complete(request).await,
            ProviderKind::XAI(p) => p.complete(request).await,
            ProviderKind::OpenCodeGo(p) => p.complete(request).await,
            ProviderKind::OpenCodeZen(p) => p.complete(request).await,
            ProviderKind::Ollama(p) => p.complete(request).await,
            ProviderKind::Mistral(p) => p.complete(request).await,
            ProviderKind::DeepSeek(p) => p.complete(request).await,
        }
    }

    async fn complete_stream(&self, request: CompletionRequest) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        match self {
            ProviderKind::OpenAI(p) => p.complete_stream(request).await,
            ProviderKind::Anthropic(p) => p.complete_stream(request).await,
            ProviderKind::Gemini(p) => p.complete_stream(request).await,
            ProviderKind::Azure(p) => p.complete_stream(request).await,
            ProviderKind::Bedrock(p) => p.complete_stream(request).await,
            ProviderKind::Groq(p) => p.complete_stream(request).await,
            ProviderKind::OpenRouter(p) => p.complete_stream(request).await,
            ProviderKind::XAI(p) => p.complete_stream(request).await,
            ProviderKind::OpenCodeGo(p) => p.complete_stream(request).await,
            ProviderKind::OpenCodeZen(p) => p.complete_stream(request).await,
            ProviderKind::Ollama(p) => p.complete_stream(request).await,
            ProviderKind::Mistral(p) => p.complete_stream(request).await,
            ProviderKind::DeepSeek(p) => p.complete_stream(request).await,
        }
    }
}

fn resolve_api_key(
    keys: &crate::config::KeyStore,
    provider_id: &str,
    api_key_env: Option<&str>,
    api_key: Option<&str>,
) -> Option<String> {
    if let Some(k) = keys.get(provider_id) { return Some(k.to_string()); }
    if let Some(env) = api_key_env {
        if let Ok(v) = std::env::var(env) { return Some(v); }
    }
    api_key.map(str::to_string)
}

pub fn create_provider(
    config: &crate::config::Config,
    keys: &crate::config::KeyStore,
) -> anyhow::Result<ProviderKind> {
    match config.provider.default.as_str() {
        "openai" => {
            let cfg = config.provider.openai.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "openai", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::OpenAI(OpenAIProvider::new(crate::config::OpenAIConfig {
                model: cfg.model, base_url: cfg.base_url, api_key_env: None, api_key,
                context_window: None,
            })))
        }
        "anthropic" => {
            let cfg = config.provider.anthropic.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "anthropic", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::Anthropic(AnthropicProvider::new(crate::config::AnthropicConfig {
                model: cfg.model, base_url: cfg.base_url, api_key_env: None, api_key,
            })))
        }
        "gemini" => {
            let cfg = config.provider.gemini.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "gemini", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::Gemini(GeminiProvider::new(crate::config::GeminiConfig {
                model: cfg.model, base_url: cfg.base_url, api_key_env: None, api_key,
            })))
        }
        "groq" => {
            let cfg = config.provider.groq.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "groq", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::Groq(GroqProvider::new(crate::config::GroqConfig {
                model: cfg.model, api_key_env: None, api_key,
            })))
        }
        "ollama" => {
            let cfg = config.provider.ollama.clone().unwrap_or_default();
            Ok(ProviderKind::Ollama(OllamaProvider::new(cfg)))
        }
        "azure" => {
            let cfg = config.provider.azure.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "azure", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::Azure(AzureProvider::new(crate::config::AzureConfig {
                deployment: cfg.deployment, api_version: cfg.api_version, endpoint: cfg.endpoint,
                api_key_env: None, api_key,
            })))
        }
        "bedrock" => {
            let cfg = config.provider.bedrock.clone().unwrap_or_default();
            Ok(ProviderKind::Bedrock(BedrockProvider::new(cfg)))
        }
        "mistral" => {
            let cfg = config.provider.mistral.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "mistral", cfg.api_key_env.as_deref().or(Some("MISTRAL_API_KEY")), cfg.api_key.as_deref());
            Ok(ProviderKind::Mistral(MistralProvider::new(crate::config::MistralConfig {
                model: cfg.model, api_key_env: None, api_key,
            })))
        }
        "deepseek" => {
            let cfg = config.provider.deepseek.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "deepseek", cfg.api_key_env.as_deref().or(Some("DEEPSEEK_API_KEY")), cfg.api_key.as_deref());
            Ok(ProviderKind::DeepSeek(DeepSeekProvider::new(crate::config::DeepSeekConfig {
                model: cfg.model, api_key_env: None, api_key,
            })))
        }
        "openrouter" => {
            let cfg = config.provider.openrouter.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "openrouter", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::OpenRouter(OpenRouterProvider::new(crate::config::OpenRouterConfig {
                model: cfg.model, api_key_env: None, api_key,
            })))
        }
        "xai" => {
            let cfg = config.provider.xai.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "xai", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::XAI(XAIProvider::new(crate::config::XAIConfig {
                model: cfg.model, api_key_env: None, api_key,
            })))
        }
        "opencode_go" => {
            let cfg = config.provider.opencode_go.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "opencode_go", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::OpenCodeGo(OpenCodeGoProvider::new(crate::config::OpenCodeGoConfig {
                model: cfg.model, api_key_env: None, api_key,
            })))
        }
        "opencode_zen" => {
            let cfg = config.provider.opencode_zen.clone().unwrap_or_default();
            let api_key = resolve_api_key(keys, "opencode_zen", cfg.api_key_env.as_deref(), cfg.api_key.as_deref());
            Ok(ProviderKind::OpenCodeZen(OpenCodeZenProvider::new(crate::config::OpenCodeZenConfig {
                model: cfg.model, api_key_env: None, api_key,
            })))
        }
        other => anyhow::bail!(
            "unsupported provider: {other}. Valid: anthropic, openai, gemini, groq, ollama, azure, bedrock, mistral, deepseek, openrouter, xai, opencode_go, opencode_zen"
        ),
    }
}
