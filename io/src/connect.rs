use io_tui::{modal, picker};

pub(crate) struct Provider {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) default_key_env: &'static str,
}

pub(crate) const PROVIDERS: &[Provider] = &[
    Provider {
        id: "anthropic",
        label: "Anthropic",
        default_key_env: "ANTHROPIC_API_KEY",
    },
    Provider {
        id: "openai",
        label: "OpenAI",
        default_key_env: "OPENAI_API_KEY",
    },
    Provider {
        id: "gemini",
        label: "Google Gemini",
        default_key_env: "GEMINI_API_KEY",
    },
    Provider {
        id: "groq",
        label: "Groq",
        default_key_env: "GROQ_API_KEY",
    },
    Provider {
        id: "ollama",
        label: "Ollama",
        default_key_env: "",
    },
    Provider {
        id: "azure",
        label: "Azure OpenAI",
        default_key_env: "AZURE_OPENAI_API_KEY",
    },
    Provider {
        id: "bedrock",
        label: "AWS Bedrock",
        default_key_env: "",
    },
    Provider {
        id: "mistral",
        label: "Mistral AI",
        default_key_env: "MISTRAL_API_KEY",
    },
    Provider {
        id: "deepseek",
        label: "DeepSeek",
        default_key_env: "DEEPSEEK_API_KEY",
    },
    Provider {
        id: "openrouter",
        label: "OpenRouter",
        default_key_env: "OPENROUTER_API_KEY",
    },
    Provider {
        id: "xai",
        label: "xAI (Grok)",
        default_key_env: "XAI_API_KEY",
    },
    Provider {
        id: "opencode_go",
        label: "OpenCode Go",
        default_key_env: "OPENCODE_GO_API_KEY",
    },
    Provider {
        id: "opencode_zen",
        label: "OpenCode Zen",
        default_key_env: "OPENCODE_API_KEY",
    },
];

fn is_env_var_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Ask whether to authenticate with an API key or an OAuth login.
fn ask_auth_oauth() -> anyhow::Result<bool> {
    Ok(picker::pick_no_esc(&["API key", "OAuth login"], Some(0))? == 1)
}

/// Prompt for an API key or the name of an env var that holds it.
fn ask_key(default_env: &str) -> anyhow::Result<String> {
    modal::text_prompt(&["API key or env var name:"], default_env, false)
}

pub async fn run() -> anyhow::Result<String> {
    let idx = select_provider()?;
    let p = &PROVIDERS[idx];

    let mut config = io_runtime::config::Config::load()?;
    let mut keys = io_runtime::config::KeyStore::load();

    match p.id {
        "openai" => {
            if ask_auth_oauth()? {
                crate::login::run(p.id).await?;
                let token = io_runtime::oauth::oauth_access_token(p.id).await?;
                let models = fetch_with_spinner("Fetching available models…", async move {
                    fetch_openai_models("https://api.openai.com/v1", &token).await
                })?;
                let model = pick_model(models, "gpt-4o")?;
                config.provider.openai = Some(io_runtime::config::OpenAIConfig {
                    model,
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key_env: None,
                    api_key: None,
                    auth: io_runtime::config::AuthMethod::OAuth,
                    context_window: None,
                    cost_input_per_1k: None,
                    cost_output_per_1k: None,
                });
            } else {
                let key_input = ask_key(p.default_key_env)?;
                let (api_key_env, api_key_inline) = split_key_input(&key_input, p.id, &mut keys);
                let resolved = resolve_key(&api_key_env, &keys, p.id);
                let models = fetch_with_spinner("Fetching available models…", async move {
                    fetch_openai_models("https://api.openai.com/v1", &resolved).await
                })?;
                let model = pick_model(models, "gpt-4o")?;
                config.provider.openai = Some(io_runtime::config::OpenAIConfig {
                    model,
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key_env,
                    api_key: api_key_inline,
                    auth: io_runtime::config::AuthMethod::ApiKey,
                    context_window: None,
                    cost_input_per_1k: None,
                    cost_output_per_1k: None,
                });
            }
        }
        "anthropic" => {
            if ask_auth_oauth()? {
                crate::login::run(p.id).await?;
                let token = io_runtime::oauth::oauth_access_token(p.id).await?;
                let models = fetch_with_spinner("Fetching available models…", async move {
                    fetch_anthropic_models(&token, true).await
                })?;
                let model = pick_model(models, "claude-sonnet-4-20250514")?;
                config.provider.anthropic = Some(io_runtime::config::AnthropicConfig {
                    model,
                    base_url: "https://api.anthropic.com/v1".to_string(),
                    api_key_env: None,
                    api_key: None,
                    auth: io_runtime::config::AuthMethod::OAuth,
                    context_window: None,
                    cost_input_per_1k: None,
                    cost_output_per_1k: None,
                });
            } else {
                let key_input = ask_key(p.default_key_env)?;
                let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
                let resolved = resolve_key(&api_key_env, &keys, p.id);
                let models = fetch_with_spinner("Fetching available models…", async move {
                    fetch_anthropic_models(&resolved, false).await
                })?;
                let model = pick_model(models, "claude-sonnet-4-20250514")?;
                config.provider.anthropic = Some(io_runtime::config::AnthropicConfig {
                    model,
                    base_url: "https://api.anthropic.com/v1".to_string(),
                    api_key_env,
                    api_key: None,
                    auth: io_runtime::config::AuthMethod::ApiKey,
                    context_window: None,
                    cost_input_per_1k: None,
                    cost_output_per_1k: None,
                });
            }
        }
        "gemini" => {
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_gemini_models(&resolved).await
            })?;
            let model = pick_model(models, "gemini-2.5-pro")?;
            config.provider.gemini = Some(io_runtime::config::GeminiConfig {
                model,
                base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "groq" => {
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_openai_models("https://api.groq.com/openai/v1", &resolved).await
            })?;
            let model = pick_model(models, "llama-3.3-70b-versatile")?;
            config.provider.groq = Some(io_runtime::config::GroqConfig {
                model,
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "ollama" => {
            let endpoint = modal::text_prompt(&["Endpoint:"], "http://localhost:11434/v1", false)?;
            let ep = endpoint.clone();
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_openai_models(&ep, "").await
            })?;
            let model = pick_model(models, "llama3.2")?;
            config.provider.ollama = Some(io_runtime::config::OllamaConfig {
                model,
                endpoint: Some(endpoint),
                context_window: None,
            });
        }
        "azure" => {
            let endpoint = modal::text_prompt(
                &["Azure endpoint (https://<resource>.openai.azure.com):"],
                "",
                false,
            )?;
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let model = modal::text_prompt(&["Deployment name:"], "gpt-4o", false)?;
            config.provider.azure = Some(io_runtime::config::AzureConfig {
                deployment: model,
                api_version: "2024-12-01-preview".to_string(),
                endpoint: if endpoint.is_empty() {
                    None
                } else {
                    Some(endpoint)
                },
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "bedrock" => {
            let region = modal::text_prompt(&["AWS region:"], "us-east-1", false)?;
            let model = modal::text_prompt(
                &["Model ID:"],
                "anthropic.claude-3-5-sonnet-20241022-v2:0",
                false,
            )?;
            config.provider.bedrock = Some(io_runtime::config::BedrockConfig {
                model,
                region: Some(region),
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "mistral" => {
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_openai_models("https://api.mistral.ai/v1", &resolved).await
            })?;
            let model = pick_model(models, "mistral-large-latest")?;
            config.provider.mistral = Some(io_runtime::config::MistralConfig {
                model,
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "deepseek" => {
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_openai_models("https://api.deepseek.com/v1", &resolved).await
            })?;
            let model = pick_model(models, "deepseek-chat")?;
            config.provider.deepseek = Some(io_runtime::config::DeepSeekConfig {
                model,
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "openrouter" => {
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_openai_models("https://openrouter.ai/api/v1", &resolved).await
            })?;
            let model = pick_model(models, "anthropic/claude-sonnet-4")?;
            config.provider.openrouter = Some(io_runtime::config::OpenRouterConfig {
                model,
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "xai" => {
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_openai_models("https://api.x.ai/v1", &resolved).await
            })?;
            let model = pick_model(models, "grok-3-beta")?;
            config.provider.xai = Some(io_runtime::config::XAIConfig {
                model,
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "opencode_go" => {
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_openai_models("https://opencode.ai/zen/go/v1", &resolved).await
            })?;
            let model = pick_model(models, "deepseek-v3")?;
            config.provider.opencode_go = Some(io_runtime::config::OpenCodeGoConfig {
                model,
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "opencode_zen" => {
            let key_input = ask_key(p.default_key_env)?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let models = fetch_with_spinner("Fetching available models…", async move {
                fetch_openai_models("https://opencode.ai/zen/v1", &resolved).await
            })?;
            let model = pick_model(models, "opencode/claude-sonnet-4")?;
            config.provider.opencode_zen = Some(io_runtime::config::OpenCodeZenConfig {
                model,
                api_key_env,
                api_key: None,
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        _ => {}
    }

    config.provider.default = p.id.to_string();
    config.save()?;
    keys.save()?;

    Ok(format!("Saved. Active provider: {}", p.id))
}

// ── interactive provider picker ───────────────────────────────────────────────

fn select_provider() -> anyhow::Result<usize> {
    let labels: Vec<&str> = PROVIDERS.iter().map(|p| p.label).collect();
    io_tui::picker::pick_no_esc(&labels, None)
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Fetch `fut` in the background while a spinner popup shows `label`. Esc
/// cancels and propagates [`picker::Dismissed`], aborting the whole /connect
/// flow, matching how any other cancelled popup in this flow behaves.
fn fetch_with_spinner<F>(label: &str, fut: F) -> anyhow::Result<Vec<String>>
where
    F: std::future::Future<Output = Vec<String>> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    tokio::spawn(async move {
        let _ = tx.send(fut.await);
    });
    modal::wait_for(&[label], rx)
}

fn split_key_input(
    key_input: &str,
    provider_id: &str,
    keys: &mut io_runtime::config::KeyStore,
) -> (Option<String>, Option<String>) {
    if is_env_var_name(key_input) {
        (Some(key_input.to_string()), None)
    } else {
        keys.set(provider_id, key_input.to_string());
        (None, None)
    }
}

fn resolve_key(
    api_key_env: &Option<String>,
    keys: &io_runtime::config::KeyStore,
    provider_id: &str,
) -> String {
    if let Some(k) = keys.get(provider_id) {
        return k.to_string();
    }
    match api_key_env {
        Some(env) if !env.is_empty() => std::env::var(env).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Let the user pick a fetched model by arrow key, or type a custom one when
/// the list is empty or doesn't contain what they want.
fn pick_model(models: Vec<String>, fallback: &str) -> anyhow::Result<String> {
    const CUSTOM: &str = "Enter a custom model name…";

    if models.is_empty() {
        return modal::text_prompt(&["Model name:"], fallback, false);
    }

    let mut items: Vec<(&str, &str)> = models.iter().map(|m| (m.as_str(), "")).collect();
    items.push((CUSTOM, ""));
    let idx = picker::pick_with_hint_no_esc(&items, None)?;
    if idx == models.len() {
        modal::text_prompt(&["Model name:"], fallback, false)
    } else {
        Ok(models[idx].clone())
    }
}

pub(crate) async fn fetch_openai_models(base_url: &str, api_key: &str) -> Vec<String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(3))
        .timeout(std::time::Duration::from_secs(5))
        .build()
    else {
        return vec![];
    };
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }

    let Ok(resp) = req.send().await else {
        return vec![];
    };
    let Ok(json) = resp.json::<serde_json::Value>().await else {
        return vec![];
    };

    let mut ids: Vec<String> = json["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

pub(crate) async fn fetch_anthropic_models(credential: &str, use_bearer: bool) -> Vec<String> {
    if credential.is_empty() {
        return vec![];
    }

    let client = reqwest::Client::new();
    let mut req = client
        .get("https://api.anthropic.com/v1/models")
        .header("anthropic-version", "2023-06-01");
    if use_bearer {
        req = req.header("Authorization", format!("Bearer {credential}"));
    } else {
        req = req.header("x-api-key", credential);
    }

    let Ok(resp) = req.send().await else {
        return vec![];
    };

    let Ok(json) = resp.json::<serde_json::Value>().await else {
        return vec![];
    };

    let mut ids: Vec<String> = json["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

pub(crate) async fn fetch_gemini_models(api_key: &str) -> Vec<String> {
    if api_key.is_empty() {
        return vec![];
    }

    let url = format!("https://generativelanguage.googleapis.com/v1beta/models?key={api_key}");
    let Ok(resp) = reqwest::get(&url).await else {
        return vec![];
    };
    let Ok(json) = resp.json::<serde_json::Value>().await else {
        return vec![];
    };

    let mut ids: Vec<String> = json["models"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let name = v["name"].as_str()?;
                    let short = name.strip_prefix("models/").unwrap_or(name).to_string();
                    if short.starts_with("gemini") {
                        Some(short)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}
