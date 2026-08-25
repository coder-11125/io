//! Best-effort discovery of real context-window sizes from provider APIs.
//!
//! Most providers never expose a model's max context length through any API
//! response — `context_window_for_model` (a static, model-name guess) and the
//! per-provider `context_window` config override are the only levers there.
//! A few providers do publish it though: Gemini's model-info endpoint returns
//! `inputTokenLimit`, and Groq / OpenRouter's model-list endpoints return
//! `context_window` / `context_length` per model. Ollama's local `/api/show`
//! returns a `model_info` map keyed by architecture (e.g.
//! `"llama.context_length"`). For those four, fetch the real number and let
//! the caller persist it as the config override instead of guessing.

use crate::config::{Config, KeyStore};

/// Fetch the real context window for `provider_id`'s configured model.
///
/// `Ok(None)` means the provider doesn't expose this via any API (not an
/// error — just nothing to discover; the static guess or a manual override
/// still applies). `Err` means discovery was attempted but failed (bad key,
/// network error, model not found in the provider's catalog).
pub async fn fetch_context_window(
    provider_id: &str,
    config: &Config,
    keys: &KeyStore,
) -> anyhow::Result<Option<u64>> {
    match provider_id {
        "gemini" => fetch_gemini(config, keys).await,
        "groq" => fetch_groq(config, keys).await,
        "openrouter" => fetch_openrouter(config, keys).await,
        "ollama" => fetch_ollama(config).await,
        _ => Ok(None),
    }
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .build()?)
}

/// Same credential precedence as `create_provider`: key store, then config
/// `api_key`, then the configured (or default) environment variable.
fn resolve_key(
    keys: &KeyStore,
    provider_id: &str,
    cfg_key: Option<&str>,
    cfg_env: Option<&str>,
) -> Option<String> {
    if let Some(k) = keys.get(provider_id) {
        return Some(k.to_string());
    }
    if let Some(env) = cfg_env.or_else(|| super::default_key_env(provider_id)) {
        if let Ok(v) = std::env::var(env) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    cfg_key.map(str::to_string)
}

async fn fetch_gemini(config: &Config, keys: &KeyStore) -> anyhow::Result<Option<u64>> {
    let cfg = config.provider.gemini.clone().unwrap_or_default();
    let api_key = resolve_key(
        keys,
        "gemini",
        cfg.api_key.as_deref(),
        cfg.api_key_env.as_deref(),
    )
    .ok_or_else(|| anyhow::anyhow!("missing Gemini API key"))?;

    let url = format!(
        "{}/models/{}?key={}",
        cfg.base_url.trim_end_matches('/'),
        cfg.model,
        api_key
    );
    let resp = client()?.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(super::api_error("Gemini", resp).await);
    }
    let json: serde_json::Value = resp.json().await?;
    Ok(json["inputTokenLimit"].as_u64())
}

async fn fetch_groq(config: &Config, keys: &KeyStore) -> anyhow::Result<Option<u64>> {
    let cfg = config.provider.groq.clone().unwrap_or_default();
    let api_key = resolve_key(
        keys,
        "groq",
        cfg.api_key.as_deref(),
        cfg.api_key_env.as_deref(),
    )
    .ok_or_else(|| anyhow::anyhow!("missing Groq API key"))?;

    let resp = client()?
        .get("https://api.groq.com/openai/v1/models")
        .bearer_auth(api_key)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(super::api_error("Groq", resp).await);
    }
    let json: serde_json::Value = resp.json().await?;
    Ok(find_model_field(&json, &cfg.model, "context_window"))
}

async fn fetch_openrouter(config: &Config, keys: &KeyStore) -> anyhow::Result<Option<u64>> {
    let cfg = config.provider.openrouter.clone().unwrap_or_default();
    // The catalog is public; an API key isn't required to read it, but send
    // one when we have it since authenticated requests get friendlier limits.
    let api_key = resolve_key(
        keys,
        "openrouter",
        cfg.api_key.as_deref(),
        cfg.api_key_env.as_deref(),
    );

    let mut req = client()?.get("https://openrouter.ai/api/v1/models");
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Err(super::api_error("OpenRouter", resp).await);
    }
    let json: serde_json::Value = resp.json().await?;
    Ok(find_model_field(&json, &cfg.model, "context_length"))
}

/// The `data[]` array shared by Groq's and OpenRouter's OpenAI-style model
/// list — find the entry matching `model` and read `field` off it.
fn find_model_field(json: &serde_json::Value, model: &str, field: &str) -> Option<u64> {
    json["data"]
        .as_array()?
        .iter()
        .find(|v| v["id"].as_str() == Some(model))
        .and_then(|v| v[field].as_u64())
}

async fn fetch_ollama(config: &Config) -> anyhow::Result<Option<u64>> {
    let cfg = config.provider.ollama.clone().unwrap_or_default();
    let endpoint = cfg
        .endpoint
        .or_else(|| std::env::var("OLLAMA_HOST").ok())
        .unwrap_or_else(|| "http://localhost:11434".to_string());
    // `endpoint`/`OLLAMA_HOST` may carry the OpenAI-compat `/v1` suffix used
    // for chat completions; the native `/api/show` lookup wants the bare host.
    let host = endpoint.trim_end_matches('/').trim_end_matches("/v1");

    let resp = client()?
        .post(format!("{host}/api/show"))
        .json(&serde_json::json!({ "name": cfg.model }))
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(super::api_error("Ollama", resp).await);
    }
    let json: serde_json::Value = resp.json().await?;
    // model_info keys are namespaced by architecture (e.g. "llama.context_length",
    // "qwen2.context_length") — take whichever one is present.
    Ok(json["model_info"].as_object().and_then(|m| {
        m.iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, v)| v.as_u64())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_model_field_matches_by_id() {
        let json = serde_json::json!({
            "data": [
                { "id": "llama-3.3-70b-versatile", "context_window": 131072 },
                { "id": "other-model", "context_window": 8192 },
            ]
        });
        assert_eq!(
            find_model_field(&json, "llama-3.3-70b-versatile", "context_window"),
            Some(131072)
        );
        assert_eq!(
            find_model_field(&json, "unknown-model", "context_window"),
            None
        );
    }

    #[test]
    fn ollama_model_info_key_is_architecture_namespaced() {
        let json = serde_json::json!({
            "model_info": { "llama.context_length": 131072, "llama.vocab_size": 128256 }
        });
        let ctx = json["model_info"].as_object().and_then(|m| {
            m.iter()
                .find(|(k, _)| k.ends_with(".context_length"))
                .and_then(|(_, v)| v.as_u64())
        });
        assert_eq!(ctx, Some(131072));
    }
}
