use crate::connect::{fetch_anthropic_models, fetch_gemini_models, fetch_openai_models, PROVIDERS};
use io_runtime::config::{Config, KeyStore};

fn model_for<'a>(config: &'a Config, provider_id: &str) -> &'a str {
    let p = &config.provider;
    let m: Option<&str> = match provider_id {
        "openai" => p.openai.as_ref().map(|c| c.model.as_str()),
        "anthropic" => p.anthropic.as_ref().map(|c| c.model.as_str()),
        "gemini" => p.gemini.as_ref().map(|c| c.model.as_str()),
        "groq" => p.groq.as_ref().map(|c| c.model.as_str()),
        "ollama" => p.ollama.as_ref().map(|c| c.model.as_str()),
        "azure" => p.azure.as_ref().map(|c| c.deployment.as_str()),
        "bedrock" => p.bedrock.as_ref().map(|c| c.model.as_str()),
        "mistral" => p.mistral.as_ref().map(|c| c.model.as_str()),
        "deepseek" => p.deepseek.as_ref().map(|c| c.model.as_str()),
        "openrouter" => p.openrouter.as_ref().map(|c| c.model.as_str()),
        "xai" => p.xai.as_ref().map(|c| c.model.as_str()),
        "opencode_go" => p.opencode_go.as_ref().map(|c| c.model.as_str()),
        "opencode_zen" => p.opencode_zen.as_ref().map(|c| c.model.as_str()),
        _ => None,
    };
    m.unwrap_or("")
}

async fn fetch_for(provider_id: &'static str, config: Config, keys: KeyStore) -> Vec<String> {
    let key = keys.get(provider_id).unwrap_or("").to_string();
    let p = &config.provider;

    match provider_id {
        "openai" => {
            let credential = if p.uses_oauth("openai") {
                io_runtime::oauth::oauth_access_token("openai")
                    .await
                    .unwrap_or_default()
            } else {
                key
            };
            fetch_openai_models("https://api.openai.com/v1", &credential).await
        }
        "anthropic" => {
            if p.uses_oauth("anthropic") {
                match io_runtime::oauth::oauth_access_token("anthropic").await {
                    Ok(token) => fetch_anthropic_models(&token, true).await,
                    Err(_) => vec![],
                }
            } else {
                fetch_anthropic_models(&key, false).await
            }
        }
        "gemini" => fetch_gemini_models(&key).await,
        "groq" => fetch_openai_models("https://api.groq.com/openai/v1", &key).await,
        "ollama" => {
            let ep = p
                .ollama
                .as_ref()
                .and_then(|c| c.endpoint.as_deref())
                .unwrap_or("http://localhost:11434/v1")
                .to_string();
            fetch_openai_models(&ep, "").await
        }
        "mistral" => fetch_openai_models("https://api.mistral.ai/v1", &key).await,
        "deepseek" => fetch_openai_models("https://api.deepseek.com/v1", &key).await,
        "openrouter" => fetch_openai_models("https://openrouter.ai/api/v1", &key).await,
        "xai" => fetch_openai_models("https://api.x.ai/v1", &key).await,
        "opencode_go" => fetch_openai_models("https://opencode.ai/zen/go/v1", &key).await,
        "opencode_zen" => fetch_openai_models("https://opencode.ai/zen/v1", &key).await,
        _ => vec![],
    }
}

fn set_context_window(config: &mut Config, provider_id: &str, tokens: u64) {
    let p = &mut config.provider;
    macro_rules! set {
        ($slot:expr) => {
            $slot.get_or_insert_with(Default::default).context_window = Some(tokens)
        };
    }
    match provider_id {
        "openai" => set!(p.openai),
        "anthropic" => set!(p.anthropic),
        "gemini" => set!(p.gemini),
        "groq" => set!(p.groq),
        "ollama" => set!(p.ollama),
        "azure" => set!(p.azure),
        "bedrock" => set!(p.bedrock),
        "mistral" => set!(p.mistral),
        "deepseek" => set!(p.deepseek),
        "openrouter" => set!(p.openrouter),
        "xai" => set!(p.xai),
        "opencode_go" => set!(p.opencode_go),
        "opencode_zen" => set!(p.opencode_zen),
        _ => {}
    }
}

/// Persist a discovered pricing override. Ollama has no pricing slot — it's
/// local/free and never billed.
fn set_pricing_override(
    config: &mut Config,
    provider_id: &str,
    pricing: &io_runtime::ModelPricing,
) {
    let p = &mut config.provider;
    macro_rules! set {
        ($slot:expr) => {{
            let c = $slot.get_or_insert_with(Default::default);
            c.cost_input_per_1k = Some(pricing.input_cost_per_1k);
            c.cost_output_per_1k = Some(pricing.output_cost_per_1k);
        }};
    }
    match provider_id {
        "openai" => set!(p.openai),
        "anthropic" => set!(p.anthropic),
        "gemini" => set!(p.gemini),
        "groq" => set!(p.groq),
        "azure" => set!(p.azure),
        "bedrock" => set!(p.bedrock),
        "mistral" => set!(p.mistral),
        "deepseek" => set!(p.deepseek),
        "openrouter" => set!(p.openrouter),
        "xai" => set!(p.xai),
        "opencode_go" => set!(p.opencode_go),
        "opencode_zen" => set!(p.opencode_zen),
        _ => {}
    }
}

/// Fetch real metadata for the active provider's model — context window,
/// pricing, and tool-call support — from models.dev's public catalog (Ollama
/// uses its own native `/api/show` lookup instead; see
/// `io_runtime::provider::discovery`) — and persist what it found as that
/// provider's config overrides. Returns a human-readable result line.
pub async fn refresh_context_window() -> anyhow::Result<String> {
    let config = Config::load()?;
    let provider_id = config.provider.default.clone();
    let model = model_for(&config, &provider_id).to_string();

    let not_found = || {
        anyhow::anyhow!(
            "{model} not found in {provider_id}'s catalog — set it manually with \
             `io config set provider.{provider_id}.context_window <tokens>`"
        )
    };

    // Explicit user-triggered refresh: always bypass the cache for the latest data.
    let Some(info) =
        io_runtime::provider::discovery::fetch_model_info(&provider_id, &config, true).await?
    else {
        return Err(not_found());
    };

    let mut parts = Vec::new();
    let mut config = Config::load()?;
    if let Some(tokens) = info.context_window {
        set_context_window(&mut config, &provider_id, tokens);
        parts.push(format!("context {tokens} tokens"));
    }
    if let Some(ref pricing) = info.pricing {
        set_pricing_override(&mut config, &provider_id, pricing);
        parts.push(format!(
            "pricing ${:.4}/${:.4} per 1K in/out",
            pricing.input_cost_per_1k, pricing.output_cost_per_1k
        ));
    }
    if parts.is_empty() {
        return Err(not_found());
    }
    config.save()?;

    let mut msg = format!("{provider_id} · {model}: {}", parts.join(", "));
    if info.tool_call == Some(false) {
        msg.push_str(
            "\n⚠ this model does not support tool calling — io's agents rely on tools \
             and may not work correctly",
        );
    }
    Ok(msg)
}

/// Best-effort background fill for the active provider's model, meant to run
/// as a detached task at startup (see `tui::run_interactive`). If
/// context_window and/or pricing aren't already configured, fetches them from
/// the (cached — up to 24h stale is fine) models.dev catalog and persists
/// them. Never overwrites a value that's already set, whether set manually or
/// by a previous fill — `/context` is the explicit, always-fresh, always-
/// overwrites path. Never surfaces an error: this must stay silent (no
/// stdout — the TUI owns the screen) and never block or fail startup, so any
/// failure (offline, rate-limited, provider not in the catalog) is simply a
/// no-op.
pub async fn auto_fill_missing_model_info() {
    let Ok(config) = Config::load() else { return };
    let provider_id = config.provider.default.clone();

    let has_context = config.provider.context_window_for(&provider_id).is_some();
    let needs_pricing =
        provider_id != "ollama" && config.provider.pricing_override_for(&provider_id).is_none();
    if has_context && !needs_pricing {
        return; // nothing missing — skip the fetch entirely
    }

    let Ok(Some(info)) =
        io_runtime::provider::discovery::fetch_model_info(&provider_id, &config, false).await
    else {
        return;
    };

    let Ok(mut config) = Config::load() else {
        return;
    };
    let mut changed = false;
    if !has_context {
        if let Some(tokens) = info.context_window {
            set_context_window(&mut config, &provider_id, tokens);
            changed = true;
        }
    }
    if needs_pricing {
        if let Some(ref pricing) = info.pricing {
            set_pricing_override(&mut config, &provider_id, pricing);
            changed = true;
        }
    }
    if changed {
        let _ = config.save();
    }
}

fn set_model(config: &mut Config, provider_id: &str, model_id: &str) {
    let m = model_id.to_string();
    let p = &mut config.provider;
    match provider_id {
        "openai" => {
            p.openai.get_or_insert_with(Default::default).model = m;
        }
        "anthropic" => {
            p.anthropic.get_or_insert_with(Default::default).model = m;
        }
        "gemini" => {
            p.gemini.get_or_insert_with(Default::default).model = m;
        }
        "groq" => {
            p.groq.get_or_insert_with(Default::default).model = m;
        }
        "ollama" => {
            p.ollama.get_or_insert_with(Default::default).model = m;
        }
        "azure" => {
            p.azure.get_or_insert_with(Default::default).deployment = m;
        }
        "bedrock" => {
            p.bedrock.get_or_insert_with(Default::default).model = m;
        }
        "mistral" => {
            p.mistral.get_or_insert_with(Default::default).model = m;
        }
        "deepseek" => {
            p.deepseek.get_or_insert_with(Default::default).model = m;
        }
        "openrouter" => {
            p.openrouter.get_or_insert_with(Default::default).model = m;
        }
        "xai" => {
            p.xai.get_or_insert_with(Default::default).model = m;
        }
        "opencode_go" => {
            p.opencode_go.get_or_insert_with(Default::default).model = m;
        }
        "opencode_zen" => {
            p.opencode_zen.get_or_insert_with(Default::default).model = m;
        }
        _ => {}
    }
}

/// Whether a provider should be listed in the `/model` picker: it has an API
/// key, needs no key (ollama/bedrock), or is configured for OAuth login
/// (ChatGPT / Claude subscription) — those authenticate without any API key.
fn provider_available(config: &Config, keys: &KeyStore, provider_id: &str) -> bool {
    keys.get(provider_id).is_some()
        || matches!(provider_id, "ollama" | "bedrock")
        || config.provider.uses_oauth(provider_id)
}

pub async fn run() -> anyhow::Result<()> {
    let config = Config::load()?;
    let keys = KeyStore::load();

    let available: Vec<(&'static str, &'static str)> = PROVIDERS
        .iter()
        .filter(|p| provider_available(&config, &keys, p.id))
        .map(|p| (p.id, p.label))
        .collect();

    if available.is_empty() {
        println!("\n  No providers configured yet. Run /connect to add one.\n");
        return Ok(());
    }

    let active_provider = config.provider.default.clone();
    let active_model = model_for(&config, &active_provider).to_string();

    // Fetch model lists from all configured providers concurrently
    use std::io::Write as _;
    print!("\n  Fetching models...");
    std::io::stdout().flush().ok();

    let handles: Vec<_> = available
        .iter()
        .map(|&(pid, plabel)| {
            let config_c = config.clone();
            let keys_c = keys.clone();
            tokio::spawn(async move {
                let models = tokio::time::timeout(
                    std::time::Duration::from_secs(4),
                    fetch_for(pid, config_c, keys_c),
                )
                .await
                .unwrap_or_default();
                (pid, plabel, models)
            })
        })
        .collect();

    let mut entries: Vec<(String, &'static str, &'static str)> = vec![];
    for h in handles {
        let (pid, plabel, models) = h.await.unwrap_or_else(|_| ("", "", vec![]));
        if models.is_empty() {
            let m = model_for(&config, pid);
            if !m.is_empty() {
                entries.push((m.to_string(), pid, plabel));
            }
        } else {
            for m in models {
                entries.push((m, pid, plabel));
            }
        }
    }

    // Clear the fetching line
    print!("\r                       \r");
    std::io::stdout().flush().ok();

    if entries.is_empty() {
        println!("  No models found. Configure providers with /connect first.\n");
        return Ok(());
    }

    let current_pos = entries
        .iter()
        .position(|(mid, pid, _)| *pid == active_provider.as_str() && mid == &active_model);

    let items: Vec<(&str, &str)> = entries.iter().map(|(m, _, pl)| (m.as_str(), *pl)).collect();

    println!();
    let picked = io_tui::picker::pick_with_hint(&items, current_pos)?;

    let (model_id, provider_id, _) = &entries[picked];
    let mut config = Config::load()?;
    config.provider.default = provider_id.to_string();
    set_model(&mut config, provider_id, model_id);
    config.save()?;

    println!();
    println!("  Active: {} · {}", provider_id, model_id);
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_available_with_api_key() {
        let config = Config::default();
        let mut keys = KeyStore::default();
        keys.set("gemini", "test-key".to_string());
        assert!(provider_available(&config, &keys, "gemini"));
        assert!(!provider_available(&config, &keys, "openai"));
    }

    #[test]
    fn provider_available_without_key_needed() {
        let config = Config::default();
        let keys = KeyStore::default();
        // ollama and bedrock need no API key.
        assert!(provider_available(&config, &keys, "ollama"));
        assert!(provider_available(&config, &keys, "bedrock"));
    }

    #[test]
    fn provider_available_with_oauth() {
        let mut config = Config::default();
        let keys = KeyStore::default();
        // No API key, not oauth yet -> not listed.
        assert!(!provider_available(&config, &keys, "openai"));
        // After OAuth login the provider is listed without any API key.
        config.provider.openai.as_mut().unwrap().auth = io_runtime::config::AuthMethod::OAuth;
        assert!(provider_available(&config, &keys, "openai"));
        // OAuth is per-provider: anthropic is not listed until logged in.
        assert!(!provider_available(&config, &keys, "anthropic"));
        config.provider.anthropic.as_mut().unwrap().auth = io_runtime::config::AuthMethod::OAuth;
        assert!(provider_available(&config, &keys, "anthropic"));
    }
}
