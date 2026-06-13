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
}

impl Default for OpenAIConfig {
    fn default() -> Self {
        Self {
            model: default_openai_model(),
            base_url: default_openai_base_url(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            model: default_anthropic_model(),
            base_url: default_anthropic_base_url(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for GeminiConfig {
    fn default() -> Self {
        Self {
            model: default_gemini_model(),
            base_url: default_gemini_base_url(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for GroqConfig {
    fn default() -> Self {
        Self {
            model: default_groq_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self {
            model: default_bedrock_model(),
            region: None,
            context_window: None,
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
}

impl Default for MistralConfig {
    fn default() -> Self {
        Self {
            model: default_mistral_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            model: default_deepseek_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            model: default_openrouter_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for XAIConfig {
    fn default() -> Self {
        Self {
            model: default_xai_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for OpenCodeGoConfig {
    fn default() -> Self {
        Self {
            model: default_opencode_go_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
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
}

impl Default for OpenCodeZenConfig {
    fn default() -> Self {
        Self {
            model: default_opencode_zen_model(),
            api_key_env: None,
            api_key: None,
            context_window: None,
        }
    }
}

fn default_opencode_zen_model() -> String {
    "opencode/deepseek-v3".to_string()
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
}

fn default_permission_mode() -> String {
    "prompt".to_string()
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
            default: "prompt".to_string(),
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            let cfg = Config::default();
            cfg.save()?;
            Ok(cfg)
        }
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
}
