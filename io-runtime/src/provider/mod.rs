use std::fmt::Debug;
use std::sync::Arc;

pub mod anthropic;
pub mod azure;
pub mod bedrock;
pub mod gemini;
pub mod openai;

pub use anthropic::AnthropicProvider;
pub use azure::AzureProvider;
pub use bedrock::BedrockProvider;
pub use gemini::GeminiProvider;
pub use openai::OpenAIProvider;

use crate::tools::ToolSpec;

/// A non-2xx response from a provider API. Carrying the HTTP status makes
/// retry classification structural — no string matching on error messages.
#[derive(Debug, thiserror::Error)]
#[error("{provider} API error ({status}): {message}")]
pub struct ApiError {
    pub provider: &'static str,
    pub status: u16,
    pub message: String,
}

/// Convert a failed HTTP response into an [`ApiError`], consuming the body
/// as the message. Shared by all provider implementations.
pub(crate) async fn api_error(provider: &'static str, resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status().as_u16();
    let message = resp.text().await.unwrap_or_default();
    ApiError {
        provider,
        status,
        message,
    }
    .into()
}

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
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
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
/// A `context_window` entry in the provider's config overrides this guess.
pub(crate) fn context_window_for_model(model: &str) -> u64 {
    let m = model.to_lowercase();
    // Anthropic — all Claude models are 200K
    if m.starts_with("claude") || m.contains("claude") {
        return 200_000;
    }
    // OpenAI reasoning models
    if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        return 200_000;
    }
    // OpenAI chat
    if m.contains("gpt-4o") || m.contains("gpt-4-turbo") {
        return 128_000;
    }
    if m.starts_with("gpt-4") {
        return 8_192;
    }
    if m.contains("gpt-3.5") {
        return 16_385;
    }
    // Gemini
    if m.contains("gemini-1.5") || m.contains("gemini-2") {
        return 1_000_000;
    }
    if m.starts_with("gemini") || m.contains("gemini") {
        return 32_000;
    }
    // Mistral
    if m.contains("mistral-large") || m.contains("mistral-medium") || m.contains("mixtral") {
        return 32_768;
    }
    if m.contains("mistral") {
        return 32_768;
    }
    // Meta Llama
    if m.contains("llama-3.1")
        || m.contains("llama-3.2")
        || m.contains("llama-3.3")
        || m.contains("llama3.1")
        || m.contains("llama3.2")
        || m.contains("llama3.3")
    {
        return 128_000;
    }
    if m.contains("llama") {
        return 8_192;
    }
    // xAI Grok
    if m.contains("grok") {
        return 131_072;
    }
    // DeepSeek
    if m.contains("deepseek") {
        return 128_000;
    }
    // Amazon Bedrock Titan / Nova
    if m.contains("nova") {
        return 300_000;
    }
    if m.contains("titan") {
        return 32_000;
    }
    // Default
    128_000
}

/// Providers that speak the OpenAI chat-completions protocol and differ only
/// by name and base URL. Adding one of these is a single line here (plus its
/// config struct).
fn compat_provider(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "groq" => ("groq", "https://api.groq.com/openai/v1"),
        "mistral" => ("mistral", "https://api.mistral.ai/v1"),
        "deepseek" => ("deepseek", "https://api.deepseek.com/v1"),
        "openrouter" => ("openrouter", "https://openrouter.ai/api/v1"),
        "xai" => ("xai", "https://api.x.ai/v1"),
        "opencode_go" => ("opencode_go", "https://opencode.ai/api/v1"),
        "opencode_zen" => ("opencode_zen", "https://opencode.ai/zen/v1"),
        _ => return None,
    })
}

/// Provider-tuned context-window heuristics for OpenAI-compatible providers.
/// Some providers expose different limits than the model's native ones (e.g.
/// DeepSeek is 200K on OpenCode Zen vs 1M native). Falls back to the generic
/// model-name guess.
fn compat_context_window(provider: &str, model: &str) -> u64 {
    let m = model.to_lowercase();
    let llama_3x = [
        "llama-3.1",
        "llama-3.2",
        "llama-3.3",
        "llama3.1",
        "llama3.2",
        "llama3.3",
    ]
    .iter()
    .any(|p| m.contains(p));
    let tuned = match provider {
        "groq" => {
            if llama_3x {
                Some(128_000)
            } else if m.contains("mixtral") {
                Some(32_768)
            } else if m.contains("llama") || m.contains("gemma") {
                Some(8_192)
            } else {
                None
            }
        }
        "ollama" => {
            if llama_3x || m.contains("deepseek") || m.contains("qwen2") || m.contains("qwen-2") {
                Some(128_000)
            } else if m.contains("mistral") {
                Some(32_768)
            } else if m.contains("gemma") {
                Some(8_192)
            } else {
                None
            }
        }
        "mistral" => {
            if m.contains("codestral") {
                Some(256_000)
            } else if m.contains("mistral-large") || m.contains("pixtral") {
                Some(128_000)
            } else if m.contains("ministral") || m.contains("mistral") {
                Some(32_768)
            } else {
                None
            }
        }
        "deepseek" => {
            if m.contains("v4") || m.contains("deepseek-v3") {
                Some(1_000_000)
            } else if m.contains("deepseek-r1") || m.contains("deepseek-reasoner") {
                Some(128_000)
            } else {
                None
            }
        }
        "openrouter" => {
            if m.contains("claude")
                || m.contains("gpt-5")
                || m.starts_with("o1")
                || m.starts_with("o3")
                || m.starts_with("o4")
            {
                Some(200_000)
            } else if m.contains("gpt-4o") || m.contains("gpt-4-turbo") {
                Some(128_000)
            } else if m.contains("gemini") || (m.contains("deepseek") && m.contains("v4")) {
                Some(1_000_000)
            } else if m.contains("deepseek") {
                Some(128_000)
            } else {
                None
            }
        }
        "xai" => m.contains("grok").then_some(131_072),
        "opencode_go" => {
            if m.contains("claude") || m.contains("deepseek") || m.contains("gpt-5") {
                Some(200_000)
            } else if m.contains("gpt-4o") {
                Some(128_000)
            } else if m.contains("gemini") || m.contains("qwen") {
                Some(1_000_000)
            } else {
                None
            }
        }
        "opencode_zen" => {
            if m.contains("claude")
                || m.starts_with("o1")
                || m.starts_with("o3")
                || m.starts_with("o4")
                || m.contains("gpt-5")
                || m.contains("deepseek")
            {
                Some(200_000)
            } else if m.contains("gpt-4o") || m.contains("gpt-4-turbo") {
                Some(128_000)
            } else if m.contains("gemini") || m.contains("qwen") {
                Some(1_000_000)
            } else if m.contains("minimax") || m.contains("abab") {
                Some(245_760)
            } else if m.contains("grok") {
                Some(131_072)
            } else if m.contains("glm")
                || m.contains("mimo")
                || m.contains("nemotron")
                || m.contains("llama")
                || m.contains("big-pickle")
                || m.contains("big_pickle")
                || m.contains("kimi")
                || m.starts_with("k")
            {
                Some(128_000)
            } else {
                None
            }
        }
        _ => None,
    };
    tuned.unwrap_or_else(|| context_window_for_model(model))
}

#[async_trait::async_trait]
pub trait CompletionModel: Debug + Send + Sync {
    fn provider_name(&self) -> &'static str;
    /// Context window size in tokens for the configured model. Defaults to 128K.
    fn context_window(&self) -> u64 {
        128_000
    }
    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse>;
    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>>;
}

/// An OpenAI-protocol provider under a different name and base URL.
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
        Self {
            name,
            inner: OpenAIProvider::new(config),
            context_window: ctx,
        }
    }
}

#[async_trait::async_trait]
impl CompletionModel for OpenAICompatProvider {
    fn provider_name(&self) -> &'static str {
        self.name
    }
    fn context_window(&self) -> u64 {
        self.context_window
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        self.inner.complete(req).await
    }

    async fn complete_stream(
        &self,
        req: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        self.inner.complete_stream(req).await
    }
}

/// Maximum number of retries after a failed provider request.
const MAX_RETRIES: u32 = 3;
/// Delay before the first retry; doubles each attempt (1s, 2s, 4s).
const INITIAL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether a provider error is transient and worth retrying: rate limits,
/// server errors, overload, or network-level timeouts and connection failures.
/// Auth errors, bad requests, and response-parsing failures are not retried.
fn is_retryable(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
            if re.is_timeout() || re.is_connect() {
                return true;
            }
        }
        if let Some(api) = cause.downcast_ref::<ApiError>() {
            // 408 timeout, 429 rate limit, 5xx server errors, 529 overloaded.
            return matches!(api.status, 408 | 429 | 500 | 502 | 503 | 504 | 529);
        }
    }
    false
}

/// Decorator adding bounded exponential-backoff retries for transient
/// failures (see [`is_retryable`]) around any provider. `create_provider`
/// wraps every provider in this, so the agent never deals with retries.
#[derive(Debug)]
struct Retrying(Arc<dyn CompletionModel>);

#[async_trait::async_trait]
impl CompletionModel for Retrying {
    fn provider_name(&self) -> &'static str {
        self.0.provider_name()
    }

    fn context_window(&self) -> u64 {
        self.0.context_window()
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let mut delay = INITIAL_RETRY_DELAY;
        let mut attempt = 0u32;
        loop {
            match self.0.complete(request.clone()).await {
                Err(e) if attempt < MAX_RETRIES && is_retryable(&e) => {
                    attempt += 1;
                    tracing::warn!(
                        "provider request failed (attempt {attempt}/{MAX_RETRIES}), retrying in {delay:?}: {e}"
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                result => return result,
            }
        }
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        // Only establishing the stream is retried; errors mid-stream are not.
        let mut delay = INITIAL_RETRY_DELAY;
        let mut attempt = 0u32;
        loop {
            match self.0.complete_stream(request.clone()).await {
                Err(e) if attempt < MAX_RETRIES && is_retryable(&e) => {
                    attempt += 1;
                    tracing::warn!(
                        "provider stream failed (attempt {attempt}/{MAX_RETRIES}), retrying in {delay:?}: {e}"
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                result => return result,
            }
        }
    }
}

fn resolve_api_key(
    keys: &crate::config::KeyStore,
    provider_id: &str,
    api_key_env: Option<&str>,
    api_key: Option<&str>,
) -> Option<String> {
    if let Some(k) = keys.get(provider_id) {
        return Some(k.to_string());
    }
    if let Some(env) = api_key_env {
        if let Ok(v) = std::env::var(env) {
            return Some(v);
        }
    }
    api_key.map(str::to_string)
}

/// Default environment variable holding the API key for a provider.
/// `None` means the provider needs no API key (local/ambient credentials).
pub fn default_key_env(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "groq" => Some("GROQ_API_KEY"),
        "azure" => Some("AZURE_OPENAI_API_KEY"),
        "mistral" => Some("MISTRAL_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        "opencode_go" => Some("OPENCODE_GO_API_KEY"),
        "opencode_zen" => Some("OPENCODE_ZEN_API_KEY"),
        _ => None, // ollama, bedrock — local/ambient credentials
    }
}

/// Checks whether the configured default provider has an API key available
/// (key store, config, or environment). Returns the name of the environment
/// variable the user should set if none was found, or `None` if the provider
/// is ready to use.
pub fn missing_api_key(
    config: &crate::config::Config,
    keys: &crate::config::KeyStore,
) -> Option<String> {
    let provider_id = config.provider.default.as_str();
    let default_env = default_key_env(provider_id)?;

    if keys.get(provider_id).is_some() {
        return None;
    }

    let (api_key, api_key_env) = config.provider.key_overrides(provider_id);
    if api_key.as_deref().is_some_and(|k| !k.is_empty()) {
        return None;
    }

    let env_name = api_key_env.unwrap_or_else(|| default_env.to_string());
    let env_has_key = |name: &str| std::env::var(name).is_ok_and(|v| !v.is_empty());

    if env_has_key(&env_name) {
        return None;
    }
    // OpenCode providers also accept the shared OPENCODE_API_KEY.
    if provider_id.starts_with("opencode") && env_has_key("OPENCODE_API_KEY") {
        return None;
    }

    Some(env_name)
}

/// Build the configured provider as a retry-wrapped trait object — the value
/// the agent talks to. Adding a provider that speaks the OpenAI protocol only
/// requires a `compat_provider` entry and a config struct; anything else
/// implements `CompletionModel` and gets its own match arm here.
pub fn create_provider(
    config: &crate::config::Config,
    keys: &crate::config::KeyStore,
) -> anyhow::Result<Arc<dyn CompletionModel>> {
    let p = &config.provider;
    let id = p.default.as_str();

    let (cfg_key, cfg_env) = p.key_overrides(id);
    let mut api_key = resolve_api_key(
        keys,
        id,
        cfg_env.as_deref().or(default_key_env(id)),
        cfg_key.as_deref(),
    );
    // OpenCode providers also accept the shared OPENCODE_API_KEY.
    if id.starts_with("opencode") && api_key.is_none() {
        api_key = std::env::var("OPENCODE_API_KEY").ok();
    }
    let ctx_override = p.context_window_for(id);

    let inner: Arc<dyn CompletionModel> = match id {
        "openai" => {
            let cfg = p.openai.clone().unwrap_or_default();
            Arc::new(OpenAIProvider::new(crate::config::OpenAIConfig {
                model: cfg.model,
                base_url: cfg.base_url,
                api_key_env: None,
                api_key,
                context_window: ctx_override,
            }))
        }
        "anthropic" => {
            let cfg = p.anthropic.clone().unwrap_or_default();
            Arc::new(AnthropicProvider::new(crate::config::AnthropicConfig {
                model: cfg.model,
                base_url: cfg.base_url,
                api_key_env: None,
                api_key,
                context_window: ctx_override,
            }))
        }
        "gemini" => {
            let cfg = p.gemini.clone().unwrap_or_default();
            Arc::new(GeminiProvider::new(crate::config::GeminiConfig {
                model: cfg.model,
                base_url: cfg.base_url,
                api_key_env: None,
                api_key,
                context_window: ctx_override,
            }))
        }
        "azure" => {
            let cfg = p.azure.clone().unwrap_or_default();
            Arc::new(AzureProvider::new(crate::config::AzureConfig {
                deployment: cfg.deployment,
                api_version: cfg.api_version,
                endpoint: cfg.endpoint,
                api_key_env: None,
                api_key,
                context_window: ctx_override,
            }))
        }
        "bedrock" => Arc::new(BedrockProvider::new(p.bedrock.clone().unwrap_or_default())),
        "ollama" => {
            let cfg = p.ollama.clone().unwrap_or_default();
            let endpoint = cfg
                .endpoint
                .or_else(|| {
                    std::env::var("OLLAMA_HOST")
                        .ok()
                        .map(|h| format!("{}/v1", h.trim_end_matches('/')))
                })
                .unwrap_or_else(|| "http://localhost:11434/v1".to_string());
            let ctx = ctx_override.unwrap_or_else(|| compat_context_window("ollama", &cfg.model));
            Arc::new(OpenAICompatProvider::with_context_window(
                "ollama",
                crate::config::OpenAIConfig {
                    model: cfg.model,
                    base_url: endpoint,
                    api_key_env: None,
                    api_key: Some("ollama".to_string()),
                    context_window: None,
                },
                ctx,
            ))
        }
        _ => {
            let Some((name, base_url)) = compat_provider(id) else {
                anyhow::bail!(
                    "unsupported provider: {id}. Valid: anthropic, openai, gemini, groq, ollama, \
                     azure, bedrock, mistral, deepseek, openrouter, xai, opencode_go, opencode_zen"
                );
            };
            // compat_provider and model_for cover the same ids.
            let model = p.model_for(id).expect("compat provider has a model");
            let ctx = ctx_override.unwrap_or_else(|| compat_context_window(name, &model));
            Arc::new(OpenAICompatProvider::with_context_window(
                name,
                crate::config::OpenAIConfig {
                    model,
                    base_url: base_url.to_string(),
                    api_key_env: None,
                    api_key,
                    context_window: None,
                },
                ctx,
            ))
        }
    };

    Ok(Arc::new(Retrying(inner)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_err(status: u16) -> anyhow::Error {
        ApiError {
            provider: "test",
            status,
            message: "boom".to_string(),
        }
        .into()
    }

    #[test]
    fn retryable_on_rate_limit_and_server_errors() {
        for status in [408, 429, 500, 502, 503, 504, 529] {
            assert!(is_retryable(&api_err(status)), "should retry: {status}");
        }
        // Also when the ApiError is wrapped with context.
        let wrapped = api_err(429).context("request failed");
        assert!(is_retryable(&wrapped), "should retry through context");
    }

    #[test]
    fn not_retryable_on_auth_or_client_errors() {
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !is_retryable(&api_err(status)),
                "should not retry: {status}"
            );
        }
        for msg in [
            "missing ANTHROPIC_API_KEY environment variable",
            "unsupported provider: foo",
        ] {
            assert!(
                !is_retryable(&anyhow::anyhow!("{msg}")),
                "should not retry: {msg}"
            );
        }
    }

    #[test]
    fn create_provider_rejects_unknown_provider() {
        let mut config = crate::config::Config::default();
        config.provider.default = "definitely-not-a-provider".to_string();
        let keys = crate::config::KeyStore::default();
        let err = create_provider(&config, &keys).unwrap_err();
        assert!(err.to_string().contains("unsupported provider"));
    }

    #[test]
    fn compat_providers_resolve_and_report_their_name() {
        let keys = crate::config::KeyStore::default();
        for id in [
            "groq",
            "mistral",
            "deepseek",
            "openrouter",
            "xai",
            "opencode_go",
            "opencode_zen",
            "ollama",
        ] {
            let mut config = crate::config::Config::default();
            config.provider.default = id.to_string();
            let provider = create_provider(&config, &keys).expect(id);
            assert_eq!(provider.provider_name(), id);
            assert!(provider.context_window() > 0);
        }
    }

    #[test]
    fn compat_context_window_provider_tuning() {
        // Groq llama-3.x is 128K, generic llama is 8K.
        assert_eq!(compat_context_window("groq", "llama-3.3-70b"), 128_000);
        assert_eq!(compat_context_window("groq", "llama-2-7b"), 8_192);
        // DeepSeek on OpenCode Zen is capped at 200K vs 1M native.
        assert_eq!(
            compat_context_window("opencode_zen", "deepseek-v3"),
            200_000
        );
        assert_eq!(compat_context_window("deepseek", "deepseek-v3"), 1_000_000);
        // Unknown models fall back to the generic guess.
        assert_eq!(
            compat_context_window("xai", "claude-sonnet-4"),
            context_window_for_model("claude-sonnet-4")
        );
    }
}
