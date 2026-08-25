//! Best-effort discovery of real per-model metadata.
//!
//! Primary source: [models.dev](https://models.dev)'s public model catalog
//! (`https://models.dev/api.json`), a community-maintained JSON database
//! covering, per model: context length (`limit.context`), per-token pricing
//! (`cost.input` / `cost.output`, in USD per million tokens), and whether the
//! model supports tool calling (`tool_call`) — for nearly every provider we
//! support. No API key needed, since it's a static public catalog rather than
//! a per-provider authenticated call. Provider ids mostly match ours;
//! `models_dev_provider_id` maps the handful that don't (`gemini` → `google`,
//! `bedrock` → `amazon-bedrock`).
//!
//! Ollama is the one exception: the catalog only lists a hosted
//! `ollama-cloud` product, not locally-pulled models, so it keeps its own
//! native `/api/show` lookup — context length from
//! `model_info["<arch>.context_length"]`, tool-call support (best-effort,
//! since older Ollama versions don't report it) from `capabilities`. Ollama
//! is local/free, so it never has pricing.
//!
//! `Ok(None)` (model/provider not found in the catalog — e.g. a custom Azure
//! deployment name or a fine-tune) is not an error: the static
//! `context_window_for_model` guess / `pricing.rs` table, or a manual config
//! override, still apply.
//!
//! The catalog (~4MB) is cached at `~/.io/models-cache.json` for 24h so
//! routine lookups (including the background auto-fill at startup, see
//! `io::model::auto_fill_missing_model_info`) don't re-download it every
//! time. A stale cache is still used as a fallback when a fresh fetch fails
//! (offline, rate-limited) — better than nothing. `fetch_model_info`'s
//! `force` flag bypasses the cache for explicit user-triggered refreshes
//! (`/context`, `io model refresh`), which should always get the latest data.

use crate::config::Config;
use crate::pricing::ModelPricing;
use chrono::{DateTime, Utc};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const CACHE_TTL_HOURS: i64 = 24;

/// Per-model metadata discovered from a provider catalog. Any field may be
/// `None` if the catalog didn't report it for this particular model.
#[derive(Debug, Clone, Default)]
pub struct ModelInfo {
    pub context_window: Option<u64>,
    pub pricing: Option<ModelPricing>,
    /// Whether the model supports tool calling — `io`'s agents depend on it.
    pub tool_call: Option<bool>,
}

/// Our provider id -> models.dev's provider id, where they differ.
fn models_dev_provider_id(provider_id: &str) -> &str {
    match provider_id {
        "gemini" => "google",
        "bedrock" => "amazon-bedrock",
        other => other,
    }
}

/// Fetch real metadata for `provider_id`'s configured model. `force` bypasses
/// the on-disk cache and always hits the network — use it for explicit
/// user-triggered refreshes; leave it `false` for opportunistic background
/// lookups, which are happy with anything up to 24h old.
pub async fn fetch_model_info(
    provider_id: &str,
    config: &Config,
    force: bool,
) -> anyhow::Result<Option<ModelInfo>> {
    if provider_id == "ollama" {
        return fetch_ollama(config).await;
    }

    let Some(model) = config.provider.model_for(provider_id) else {
        return Ok(None);
    };

    let catalog = fetch_catalog(force).await?;

    let dev_id = models_dev_provider_id(provider_id);
    let entry = &catalog[dev_id]["models"][model.as_str()];
    if entry.is_null() {
        return Ok(None);
    }

    Ok(Some(ModelInfo {
        context_window: entry["limit"]["context"].as_u64(),
        pricing: model_pricing(entry),
        tool_call: entry["tool_call"].as_bool(),
    }))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedCatalog {
    fetched_at: DateTime<Utc>,
    catalog: serde_json::Value,
}

fn cache_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~"))
        .join(".io")
        .join("models-cache.json")
}

fn load_cache() -> Option<CachedCatalog> {
    let contents = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&contents).ok()
}

fn is_fresh(fetched_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - fetched_at < chrono::Duration::hours(CACHE_TTL_HOURS)
}

fn save_cache(catalog: &serde_json::Value) {
    let path = cache_path();
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let entry = CachedCatalog {
        fetched_at: Utc::now(),
        catalog: catalog.clone(),
    };
    if let Ok(json) = serde_json::to_string(&entry) {
        let _ = std::fs::write(path, json);
    }
}

/// The models.dev catalog, from cache when fresh enough (or always, if not
/// `force`d), otherwise fetched live and re-cached. A stale cache is used as
/// a last-resort fallback when a live fetch fails, so a temporary outage
/// doesn't break lookups entirely.
async fn fetch_catalog(force: bool) -> anyhow::Result<serde_json::Value> {
    if !force {
        if let Some(cached) = load_cache() {
            if is_fresh(cached.fetched_at, Utc::now()) {
                return Ok(cached.catalog);
            }
        }
    }

    match fetch_catalog_live().await {
        Ok(catalog) => {
            save_cache(&catalog);
            Ok(catalog)
        }
        Err(e) => match load_cache() {
            Some(cached) => Ok(cached.catalog),
            None => Err(e),
        },
    }
}

async fn fetch_catalog_live() -> anyhow::Result<serde_json::Value> {
    let resp = client()?.get(MODELS_DEV_URL).send().await?;
    if !resp.status().is_success() {
        return Err(super::api_error("models.dev", resp).await);
    }
    Ok(resp.json().await?)
}

/// models.dev reports `cost.input`/`cost.output` in USD per million tokens;
/// `ModelPricing` is USD per 1,000 tokens.
fn model_pricing(entry: &serde_json::Value) -> Option<ModelPricing> {
    let input = entry["cost"]["input"].as_f64()?;
    let output = entry["cost"]["output"].as_f64()?;
    Some(ModelPricing::new(input / 1000.0, output / 1000.0))
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()?)
}

async fn fetch_ollama(config: &Config) -> anyhow::Result<Option<ModelInfo>> {
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
    let context_window = json["model_info"].as_object().and_then(|m| {
        m.iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, v)| v.as_u64())
    });
    // Newer Ollama versions report supported capabilities; older ones omit
    // the field entirely, in which case tool support stays unknown.
    let tool_call = json["capabilities"]
        .as_array()
        .map(|caps| caps.iter().any(|c| c.as_str() == Some("tools")));

    Ok(Some(ModelInfo {
        context_window,
        pricing: None,
        tool_call,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_mapping() {
        assert_eq!(models_dev_provider_id("gemini"), "google");
        assert_eq!(models_dev_provider_id("bedrock"), "amazon-bedrock");
        assert_eq!(models_dev_provider_id("anthropic"), "anthropic");
        assert_eq!(models_dev_provider_id("openrouter"), "openrouter");
    }

    fn sample_entry() -> serde_json::Value {
        serde_json::json!({
            "limit": { "context": 200000, "output": 64000 },
            "cost": { "input": 3, "output": 15, "cache_read": 0.3 },
            "tool_call": true
        })
    }

    #[test]
    fn catalog_lookup_reads_limit_context() {
        let catalog = serde_json::json!({ "anthropic": { "models": { "claude-sonnet-4-5": sample_entry() } } });
        let dev_id = models_dev_provider_id("anthropic");
        let entry = &catalog[dev_id]["models"]["claude-sonnet-4-5"];
        assert_eq!(entry["limit"]["context"].as_u64(), Some(200000));
        let missing = &catalog[dev_id]["models"]["not-a-real-model"];
        assert!(missing.is_null());
    }

    #[test]
    fn pricing_converts_per_million_to_per_1k() {
        let entry = sample_entry();
        let pricing = model_pricing(&entry).unwrap();
        assert!((pricing.input_cost_per_1k - 0.003).abs() < 1e-9);
        assert!((pricing.output_cost_per_1k - 0.015).abs() < 1e-9);
    }

    #[test]
    fn pricing_missing_output_is_none() {
        let entry = serde_json::json!({ "cost": { "input": 3 } });
        assert!(model_pricing(&entry).is_none());
    }

    #[test]
    fn tool_call_flag_reads_through() {
        let entry = sample_entry();
        assert_eq!(entry["tool_call"].as_bool(), Some(true));
    }

    #[test]
    fn ollama_model_info_key_is_architecture_namespaced() {
        let json = serde_json::json!({
            "model_info": { "llama.context_length": 131072, "llama.vocab_size": 128256 },
            "capabilities": ["completion", "tools"]
        });
        let ctx = json["model_info"].as_object().and_then(|m| {
            m.iter()
                .find(|(k, _)| k.ends_with(".context_length"))
                .and_then(|(_, v)| v.as_u64())
        });
        assert_eq!(ctx, Some(131072));
        let tool_call = json["capabilities"]
            .as_array()
            .map(|caps| caps.iter().any(|c| c.as_str() == Some("tools")));
        assert_eq!(tool_call, Some(true));
    }

    #[test]
    fn cache_freshness_boundary() {
        let now = Utc::now();
        assert!(is_fresh(now - chrono::Duration::hours(1), now));
        assert!(is_fresh(now - chrono::Duration::hours(23), now));
        assert!(!is_fresh(now - chrono::Duration::hours(25), now));
        assert!(!is_fresh(now - chrono::Duration::hours(24), now));
    }

    #[test]
    fn ollama_missing_capabilities_is_unknown_not_false() {
        let json = serde_json::json!({ "model_info": {} });
        let tool_call = json["capabilities"]
            .as_array()
            .map(|caps| caps.iter().any(|c| c.as_str() == Some("tools")));
        assert_eq!(tool_call, None);
    }
}
