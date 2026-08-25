use crate::pricing::ModelPricing;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub permissions: PermissionConfig,
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_theme() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_provider")]
    pub default: String,
    #[serde(default)]
    pub openai: Option<OpenAIConfig>,
    #[serde(default)]
    pub anthropic: Option<AnthropicConfig>,
    #[serde(default)]
    pub gemini: Option<GeminiConfig>,
    #[serde(default)]
    pub groq: Option<GroqConfig>,
    #[serde(default)]
    pub ollama: Option<OllamaConfig>,
    #[serde(default)]
    pub azure: Option<AzureConfig>,
    #[serde(default)]
    pub bedrock: Option<BedrockConfig>,
    #[serde(default)]
    pub mistral: Option<MistralConfig>,
    #[serde(default)]
    pub deepseek: Option<DeepSeekConfig>,
    #[serde(default)]
    pub openrouter: Option<OpenRouterConfig>,
    #[serde(default)]
    pub xai: Option<XAIConfig>,
    #[serde(default)]
    pub opencode_go: Option<OpenCodeGoConfig>,
    #[serde(default)]
    pub opencode_zen: Option<OpenCodeZenConfig>,
}

fn default_provider() -> String {
    "openai".to_string()
}

impl ProviderConfig {
    /// Whether the provider is configured to authenticate via OAuth login
    /// instead of an API key.
    pub fn uses_oauth(&self, id: &str) -> bool {
        match id {
            "openai" => self
                .openai
                .as_ref()
                .is_some_and(|c| c.auth == AuthMethod::OAuth),
            "anthropic" => self
                .anthropic
                .as_ref()
                .is_some_and(|c| c.auth == AuthMethod::OAuth),
            _ => false,
        }
    }

    /// Configured `(api_key, api_key_env)` overrides for a provider id.
    /// This is the single place that maps provider ids to their config slots
    /// for credential lookup — keep new providers in sync here.
    pub fn key_overrides(&self, id: &str) -> (Option<String>, Option<String>) {
        macro_rules! kf {
            ($slot:expr) => {
                $slot
                    .as_ref()
                    .map(|c| (c.api_key.clone(), c.api_key_env.clone()))
                    .unwrap_or((None, None))
            };
        }
        match id {
            "openai" => kf!(self.openai),
            "anthropic" => kf!(self.anthropic),
            "gemini" => kf!(self.gemini),
            "groq" => kf!(self.groq),
            "azure" => kf!(self.azure),
            "mistral" => kf!(self.mistral),
            "deepseek" => kf!(self.deepseek),
            "openrouter" => kf!(self.openrouter),
            "xai" => kf!(self.xai),
            "opencode_go" => kf!(self.opencode_go),
            "opencode_zen" => kf!(self.opencode_zen),
            // ollama, bedrock — local/ambient credentials
            _ => (None, None),
        }
    }

    /// The configured model (deployment for Azure) for a provider id,
    /// falling back to that provider's default model when unconfigured.
    pub fn model_for(&self, id: &str) -> Option<String> {
        let model = match id {
            "openai" => self.openai.clone().unwrap_or_default().model,
            "anthropic" => self.anthropic.clone().unwrap_or_default().model,
            "gemini" => self.gemini.clone().unwrap_or_default().model,
            "groq" => self.groq.clone().unwrap_or_default().model,
            "ollama" => self.ollama.clone().unwrap_or_default().model,
            "azure" => self.azure.clone().unwrap_or_default().deployment,
            "bedrock" => self.bedrock.clone().unwrap_or_default().model,
            "mistral" => self.mistral.clone().unwrap_or_default().model,
            "deepseek" => self.deepseek.clone().unwrap_or_default().model,
            "openrouter" => self.openrouter.clone().unwrap_or_default().model,
            "xai" => self.xai.clone().unwrap_or_default().model,
            "opencode_go" => self.opencode_go.clone().unwrap_or_default().model,
            "opencode_zen" => self.opencode_zen.clone().unwrap_or_default().model,
            _ => return None,
        };
        Some(model)
    }

    /// Configured context-window override for a provider id, if any.
    pub fn context_window_for(&self, id: &str) -> Option<u64> {
        match id {
            "openai" => self.openai.as_ref()?.context_window,
            "anthropic" => self.anthropic.as_ref()?.context_window,
            "gemini" => self.gemini.as_ref()?.context_window,
            "groq" => self.groq.as_ref()?.context_window,
            "ollama" => self.ollama.as_ref()?.context_window,
            "azure" => self.azure.as_ref()?.context_window,
            "bedrock" => self.bedrock.as_ref()?.context_window,
            "mistral" => self.mistral.as_ref()?.context_window,
            "deepseek" => self.deepseek.as_ref()?.context_window,
            "openrouter" => self.openrouter.as_ref()?.context_window,
            "xai" => self.xai.as_ref()?.context_window,
            "opencode_go" => self.opencode_go.as_ref()?.context_window,
            "opencode_zen" => self.opencode_zen.as_ref()?.context_window,
            _ => None,
        }
    }

    /// Configured per-token pricing override for a provider id, if both the
    /// input and output rates are set. Takes precedence over the static
    /// pricing table in `pricing.rs`. Ollama has no slot for this — it's
    /// local/free and never billed.
    pub fn pricing_override_for(&self, id: &str) -> Option<ModelPricing> {
        macro_rules! pf {
            ($slot:expr) => {{
                let c = $slot.as_ref()?;
                (c.cost_input_per_1k, c.cost_output_per_1k)
            }};
        }
        let (input, output) = match id {
            "openai" => pf!(self.openai),
            "anthropic" => pf!(self.anthropic),
            "gemini" => pf!(self.gemini),
            "groq" => pf!(self.groq),
            "azure" => pf!(self.azure),
            "bedrock" => pf!(self.bedrock),
            "mistral" => pf!(self.mistral),
            "deepseek" => pf!(self.deepseek),
            "openrouter" => pf!(self.openrouter),
            "xai" => pf!(self.xai),
            "opencode_go" => pf!(self.opencode_go),
            "opencode_zen" => pf!(self.opencode_zen),
            _ => return None,
        };
        Some(ModelPricing::new(input?, output?))
    }

    /// Model id of the active (default) provider — used for display,
    /// pricing, and session metadata.
    pub fn active_model(&self) -> String {
        self.model_for(&self.default)
            .unwrap_or_else(|| self.default.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIConfig {
    #[serde(default = "default_openai_model")]
    pub model: String,
    #[serde(default = "default_openai_base_url")]
    pub base_url: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this provider+model (tokens).
    /// When set, overrides the model-name-based guess in `context_window_for_model`.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
    /// Authentication method: API key (default) or OAuth subscription login.
    #[serde(default)]
    pub auth: AuthMethod,
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            model: default_openai_model(),
            base_url: default_openai_base_url(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
            auth: AuthMethod::ApiKey,
        }
    }
}

fn default_openai_model() -> String {
    "gpt-4o".to_string()
}
fn default_openai_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    #[serde(default = "default_anthropic_model")]
    pub model: String,
    #[serde(default = "default_anthropic_base_url")]
    pub base_url: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
    /// Authentication method: API key (default) or OAuth subscription login.
    #[serde(default)]
    pub auth: AuthMethod,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            model: default_anthropic_model(),
            base_url: default_anthropic_base_url(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
            auth: AuthMethod::ApiKey,
        }
    }
}

fn default_anthropic_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}
fn default_anthropic_base_url() -> String {
    "https://api.anthropic.com/v1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiConfig {
    #[serde(default = "default_gemini_model")]
    pub model: String,
    #[serde(default = "default_gemini_base_url")]
    pub base_url: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            model: default_gemini_model(),
            base_url: default_gemini_base_url(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_gemini_model() -> String {
    "gemini-2.5-pro".to_string()
}
fn default_gemini_base_url() -> String {
    "https://generativelanguage.googleapis.com/v1beta".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroqConfig {
    #[serde(default = "default_groq_model")]
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for GroqConfig {
    fn default() -> Self {
        Self {
            model: default_groq_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_groq_model() -> String {
    "llama-3.3-70b-versatile".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    #[serde(default = "default_ollama_model")]
    pub model: String,
    pub endpoint: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            model: default_ollama_model(),
            endpoint: None,
            context_window: None,
        }
    }
}

fn default_ollama_model() -> String {
    "llama3.2".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzureConfig {
    #[serde(default = "default_azure_deployment")]
    pub deployment: String,
    #[serde(default = "default_azure_api_version")]
    pub api_version: String,
    pub endpoint: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for AzureConfig {
    fn default() -> Self {
        Self {
            deployment: default_azure_deployment(),
            api_version: default_azure_api_version(),
            endpoint: None,
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_azure_deployment() -> String {
    "gpt-4o".to_string()
}
fn default_azure_api_version() -> String {
    "2024-12-01-preview".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BedrockConfig {
    #[serde(default = "default_bedrock_model")]
    pub model: String,
    pub region: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self {
            model: default_bedrock_model(),
            region: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_bedrock_model() -> String {
    "amazon.nova-pro-v1:0".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MistralConfig {
    #[serde(default = "default_mistral_model")]
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for MistralConfig {
    fn default() -> Self {
        Self {
            model: default_mistral_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_mistral_model() -> String {
    "mistral-large-latest".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekConfig {
    #[serde(default = "default_deepseek_model")]
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            model: default_deepseek_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_deepseek_model() -> String {
    "deepseek-chat".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRouterConfig {
    #[serde(default = "default_openrouter_model")]
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            model: default_openrouter_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_openrouter_model() -> String {
    "openai/gpt-4o".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XAIConfig {
    #[serde(default = "default_xai_model")]
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for XAIConfig {
    fn default() -> Self {
        Self {
            model: default_xai_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_xai_model() -> String {
    "grok-3-beta".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeGoConfig {
    #[serde(default = "default_opencode_go_model")]
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for OpenCodeGoConfig {
    fn default() -> Self {
        Self {
            model: default_opencode_go_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_opencode_go_model() -> String {
    "deepseek-v3".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeZenConfig {
    #[serde(default = "default_opencode_zen_model")]
    pub model: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Actual context window for this model (tokens).
    /// When set, overrides the built-in model-name-based guess.
    pub context_window: Option<u64>,
    /// Actual per-1K-token input/output pricing (USD) for this model.
    /// When both are set, overrides the static table in `pricing.rs`.
    pub cost_input_per_1k: Option<f64>,
    pub cost_output_per_1k: Option<f64>,
}

impl Default for OpenCodeZenConfig {
    fn default() -> Self {
        Self {
            model: default_opencode_zen_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
            cost_input_per_1k: None,
            cost_output_per_1k: None,
        }
    }
}

fn default_opencode_zen_model() -> String {
    "opencode/deepseek-v3".to_string()
}

/// How a provider authenticates: a traditional API key, or an OAuth login
/// (OpenAI ChatGPT / Anthropic Claude subscription).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    #[default]
    #[serde(rename = "api_key")]
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth,
}

// ── Session / Permission configs ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_true")]
    pub auto_compact: bool,
    #[serde(default = "default_true")]
    pub memory_enabled: bool,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_true() -> bool {
    true
}
fn default_max_turns() -> usize {
    100
}
fn default_max_tokens() -> u32 {
    16384
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionConfig {
    #[serde(default = "default_permission_mode")]
    pub default: String,
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    #[serde(default)]
    pub denied_commands: Vec<String>,
    /// Opt-in lenience: in `agent` mode, read-only network fetches that only
    /// write to stdout (`curl URL`, `wget -O- URL`) run without prompting.
    /// File-writing, upload, and custom-method network commands still prompt.
    /// Defaults to false so network egress stays gated unless the user opts in.
    #[serde(default)]
    pub allow_network_fetch: bool,
}

fn default_permission_mode() -> String {
    "agent".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig {
                default: default_provider(),
                openai: Some(OpenAIConfig::default()),
                anthropic: Some(AnthropicConfig::default()),
                gemini: Some(GeminiConfig::default()),
                groq: None,
                ollama: None,
                azure: None,
                bedrock: None,
                mistral: None,
                deepseek: None,
                openrouter: None,
                xai: None,
                opencode_go: None,
                opencode_zen: None,
            },
            session: SessionConfig::default(),
            permissions: PermissionConfig::default(),
            theme: default_theme(),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            auto_compact: true,
            memory_enabled: true,
            max_turns: 100,
            max_tokens: 16384,
        }
    }
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            default: default_permission_mode(),
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            allow_network_fetch: false,
        }
    }
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            Self::load_with_schema_upgrade(&config_path, &contents)
        } else {
            let cfg = Config::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    /// Deserialize a config file, rewriting it in place when it lags the
    /// current schema.
    ///
    /// When the file is missing keys the current `Config` knows about (for
    /// example after an upgrade added a new option like
    /// `permissions.allow_network_fetch`), the file is rewritten with the
    /// missing keys filled in from the defaults — while preserving every
    /// existing value, including inline `api_key` entries.
    ///
    /// This is strictly additive: unknown keys and comments are left alone
    /// when there is no drift, and nothing the user wrote is ever overwritten
    /// or removed. OAuth tokens (`~/.io/oauth.toml`) and the key store
    /// (`~/.io/keys.toml`) live in separate files and are never touched.
    ///
    /// Optional provider sections the file does not contain are not
    /// resurrected: a missing `[provider.groq]` means "not configured", not
    /// "use defaults". Existing sections do get new fields merged in.
    fn load_with_schema_upgrade(path: &std::path::Path, contents: &str) -> anyhow::Result<Self> {
        let config: Config = toml::from_str(contents)?;
        let original: toml::Table = contents.parse()?;
        let mut merged = original.clone();

        let mut defaults: toml::Table =
            toml::from_str(&toml::to_string_pretty(&Config::default())?)?;
        // Never resurrect optional provider slots the user omitted.
        if let Some(toml::Value::Table(def_provider)) = defaults.get_mut("provider") {
            let present = original
                .get("provider")
                .and_then(toml::Value::as_table)
                .map(|t| t.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            def_provider.retain(|key, _| key == "default" || present.iter().any(|k| k == key));
        }

        merge_missing(&mut merged, &defaults);
        if merged != original {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, toml::to_string_pretty(&merged)?)?;
        }
        Ok(config)
    }

    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".io")
            .join("config.toml")
    }

    pub fn project_config_path(project_root: &std::path::Path) -> PathBuf {
        project_root.join(".io").join("config.toml")
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        Ok(())
    }
}

/// Recursively insert keys from `defaults` that `raw` is missing. Existing
/// values — api keys, provider settings, whatever the user has — always win.
fn merge_missing(raw: &mut toml::Table, defaults: &toml::Table) {
    for (key, default_val) in defaults {
        match (raw.get_mut(key), default_val) {
            (Some(toml::Value::Table(raw_table)), toml::Value::Table(default_table)) => {
                merge_missing(raw_table, default_table);
            }
            (None, value) => {
                raw.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyStore(std::collections::HashMap<String, String>);

impl KeyStore {
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| toml::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".io")
            .join("keys.toml")
    }

    pub fn get(&self, provider: &str) -> Option<&str> {
        self.0.get(provider).map(String::as_str)
    }

    pub fn set(&mut self, provider: &str, key: String) {
        self.0.insert(provider.to_string(), key);
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, &contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.provider.default, "openai");
        assert!(config.provider.openai.is_some());
        assert!(config.session.auto_compact);
    }

    #[test]
    fn test_config_roundtrip() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.provider.default, config.provider.default);
    }

    #[test]
    fn test_auth_method_serde_and_default() {
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            auth: AuthMethod,
        }
        assert_eq!(AuthMethod::default(), AuthMethod::ApiKey);
        assert_eq!(
            toml::to_string(&Wrap {
                auth: AuthMethod::OAuth
            })
            .unwrap(),
            "auth = \"oauth\"\n"
        );
        assert_eq!(
            toml::from_str::<Wrap>("auth = \"oauth\"").unwrap().auth,
            AuthMethod::OAuth
        );
    }

    #[test]
    fn test_uses_oauth() {
        let mut config = Config::default();
        assert!(!config.provider.uses_oauth("openai"));
        config.provider.openai.as_mut().unwrap().auth = AuthMethod::OAuth;
        assert!(config.provider.uses_oauth("openai"));
        assert!(!config.provider.uses_oauth("anthropic"));
    }

    // ── Schema-upgrade rewrite tests ──────────────────────────────────────────

    /// Unique temp config path per test (parallel-safe, cleaned up by caller).
    fn temp_config_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("io-config-{name}-{}.toml", std::process::id()))
    }

    #[test]
    fn schema_upgrade_adds_missing_keys_and_keeps_values() {
        let path = temp_config_path("upgrade");
        let contents = r#"
            [provider]
            default = "anthropic"

            [provider.anthropic]
            model = "claude-sonnet-4-20250514"
            api_key = "sk-ant-inline-test"

            [session]
            auto_compact = true
            max_turns = 50
        "#;
        std::fs::write(&path, contents).unwrap();

        let config = Config::load_with_schema_upgrade(&path, contents).unwrap();

        // New schema keys come in with defaults.
        assert_eq!(config.permissions.default, "agent");
        assert!(!config.permissions.allow_network_fetch);
        assert_eq!(config.theme, "default");
        // Existing values are preserved.
        assert_eq!(config.provider.default, "anthropic");
        assert_eq!(
            config
                .provider
                .anthropic
                .as_ref()
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-ant-inline-test")
        );
        assert_eq!(config.session.max_turns, 50);

        // The file on disk was rewritten to the current schema…
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(rewritten.contains("allow_network_fetch = false"));
        assert!(rewritten.contains("theme = \"default\""));
        // …and the inline api key survived the rewrite.
        assert!(rewritten.contains("api_key = \"sk-ant-inline-test\""));

        // A second load of the migrated file is a no-op rewrite.
        let _ = Config::load_with_schema_upgrade(&path, &rewritten).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), rewritten);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_upgrade_does_not_resurrect_omitted_providers() {
        let path = temp_config_path("noresurrect");
        let contents = r#"
            [provider]
            default = "groq"

            [provider.groq]
            model = "llama-3.3-70b-versatile"
        "#;
        std::fs::write(&path, contents).unwrap();

        let config = Config::load_with_schema_upgrade(&path, contents).unwrap();
        assert!(config.provider.openai.is_none());
        assert!(config.provider.groq.is_some());
        assert_eq!(config.provider.default, "groq");

        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(!rewritten.contains("[provider.openai]"));
        assert!(rewritten.contains("[provider.groq]"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_upgrade_is_strictly_additive_and_preserves_unknown_keys() {
        let path = temp_config_path("additive");
        let contents = "future_option = \"keep me\"\n[permissions]\ndefault = \"prompt\"\n";
        std::fs::write(&path, contents).unwrap();

        let config = Config::load_with_schema_upgrade(&path, contents).unwrap();
        assert_eq!(config.permissions.default, "prompt");

        let rewritten = std::fs::read_to_string(&path).unwrap();
        // Unknown keys survive and are not clobbered by the rewrite.
        assert!(rewritten.contains("future_option = \"keep me\""));
        assert!(rewritten.contains("allow_network_fetch = false"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_upgrade_current_schema_is_not_rewritten() {
        let path = temp_config_path("current");
        let mut config = Config::default();
        config.permissions.default = "agent".to_string();
        config.permissions.allow_network_fetch = true;
        config.permissions.allowed_commands = vec!["git".to_string(), "curl".to_string()];
        let mut contents = toml::to_string_pretty(&config).unwrap();
        // A comment serialization would drop — proof the file was not rewritten.
        contents = format!("# user comment\n{contents}");
        std::fs::write(&path, &contents).unwrap();

        let _ = Config::load_with_schema_upgrade(&path, &contents).unwrap();
        // No drift → no rewrite, comment preserved byte-for-byte.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_upgrade_malformed_config_errors_without_rewriting() {
        let path = temp_config_path("malformed");
        let contents = "this is not = [valid toml";
        std::fs::write(&path, contents).unwrap();

        assert!(Config::load_with_schema_upgrade(&path, contents).is_err());
        // The broken file is left untouched — never destroyed.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_upgrade_never_touches_keys_or_oauth_files() {
        // Migration writes only the config file; sibling key/oauth stores
        // (separate files with their own 0600 handling) must be untouched.
        let path = temp_config_path("siblings");
        let keys_path = std::env::temp_dir().join(format!(
            "io-config-siblings-{}-keys.toml",
            std::process::id()
        ));
        let oauth_path = std::env::temp_dir().join(format!(
            "io-config-siblings-{}-oauth.toml",
            std::process::id()
        ));
        std::fs::write(&keys_path, "openai = \"sk-sibling-key\"\n").unwrap();
        std::fs::write(&oauth_path, "[tokens]\nopenai = {}\n").unwrap();

        let contents = "[permissions]\ndefault = \"prompt\"\n";
        std::fs::write(&path, contents).unwrap();
        let _ = Config::load_with_schema_upgrade(&path, contents).unwrap();

        assert_eq!(
            std::fs::read_to_string(&keys_path).unwrap(),
            "openai = \"sk-sibling-key\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(&oauth_path).unwrap(),
            "[tokens]\nopenai = {}\n"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&keys_path);
        let _ = std::fs::remove_file(&oauth_path);
    }
}
