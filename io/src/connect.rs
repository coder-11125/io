use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};

type Lines = tokio::io::Lines<BufReader<tokio::io::Stdin>>;

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
async fn ask_auth_oauth(lines: &mut Lines) -> anyhow::Result<bool> {
    let answer = ask(
        lines,
        "Authentication (api key / oauth login) [api key]",
        "api key",
    )
    .await?;
    Ok(answer.trim().eq_ignore_ascii_case("oauth") || answer.trim().eq_ignore_ascii_case("login"))
}

pub async fn run() -> anyhow::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let lines = &mut lines;
    let idx = select_provider()?;
    let p = &PROVIDERS[idx];

    let mut config = io_runtime::config::Config::load()?;
    let mut keys = io_runtime::config::KeyStore::load();

    match p.id {
        "openai" => {
            if ask_auth_oauth(lines).await? {
                crate::login::run(p.id).await?;
                let token = io_runtime::oauth::oauth_access_token(p.id).await?;
                let model = pick_model(
                    lines,
                    fetch_openai_models("https://api.openai.com/v1", &token).await,
                    "gpt-4o",
                )
                .await?;
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
                let key_input = ask(
                    lines,
                    &format!("API key or env var name [{}]", p.default_key_env),
                    p.default_key_env,
                )
                .await?;
                let (api_key_env, api_key_inline) = split_key_input(&key_input, p.id, &mut keys);
                let resolved = resolve_key(&api_key_env, &keys, p.id);
                let model = pick_model(
                    lines,
                    fetch_openai_models("https://api.openai.com/v1", &resolved).await,
                    "gpt-4o",
                )
                .await?;
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
            if ask_auth_oauth(lines).await? {
                crate::login::run(p.id).await?;
                let token = io_runtime::oauth::oauth_access_token(p.id).await?;
                let model = pick_model(
                    lines,
                    fetch_anthropic_models(&token, true).await,
                    "claude-sonnet-4-20250514",
                )
                .await?;
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
                let key_input = ask(
                    lines,
                    &format!("API key or env var name [{}]", p.default_key_env),
                    p.default_key_env,
                )
                .await?;
                let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
                let resolved = resolve_key(&api_key_env, &keys, p.id);
                let model = pick_model(
                    lines,
                    fetch_anthropic_models(&resolved, false).await,
                    "claude-sonnet-4-20250514",
                )
                .await?;
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
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let model = pick_model(
                lines,
                fetch_gemini_models(&resolved).await,
                "gemini-2.5-pro",
            )
            .await?;
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
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let model = pick_model(
                lines,
                fetch_openai_models("https://api.groq.com/openai/v1", &resolved).await,
                "llama-3.3-70b-versatile",
            )
            .await?;
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
            let endpoint = ask(
                lines,
                "Endpoint [http://localhost:11434/v1]",
                "http://localhost:11434/v1",
            )
            .await?;
            let model =
                pick_model(lines, fetch_openai_models(&endpoint, "").await, "llama3.2").await?;
            config.provider.ollama = Some(io_runtime::config::OllamaConfig {
                model,
                endpoint: Some(endpoint),
                context_window: None,
            });
        }
        "azure" => {
            let endpoint = ask(
                lines,
                "Azure endpoint (https://<resource>.openai.azure.com)",
                "",
            )
            .await?;
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let model = ask(lines, "Deployment name [gpt-4o]", "gpt-4o").await?;
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
            let region = ask(lines, "AWS region [us-east-1]", "us-east-1").await?;
            let model = ask(
                lines,
                "Model ID [anthropic.claude-3-5-sonnet-20241022-v2:0]",
                "anthropic.claude-3-5-sonnet-20241022-v2:0",
            )
            .await?;
            config.provider.bedrock = Some(io_runtime::config::BedrockConfig {
                model,
                region: Some(region),
                context_window: None,
                cost_input_per_1k: None,
                cost_output_per_1k: None,
            });
        }
        "mistral" => {
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let model = pick_model(
                lines,
                fetch_openai_models("https://api.mistral.ai/v1", &resolved).await,
                "mistral-large-latest",
            )
            .await?;
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
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let model = pick_model(
                lines,
                fetch_openai_models("https://api.deepseek.com/v1", &resolved).await,
                "deepseek-chat",
            )
            .await?;
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
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let model = pick_model(
                lines,
                fetch_openai_models("https://openrouter.ai/api/v1", &resolved).await,
                "anthropic/claude-sonnet-4",
            )
            .await?;
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
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let model = pick_model(
                lines,
                fetch_openai_models("https://api.x.ai/v1", &resolved).await,
                "grok-3-beta",
            )
            .await?;
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
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let model = pick_model(
                lines,
                fetch_openai_models("https://opencode.ai/zen/go/v1", &resolved).await,
                "deepseek-v3",
            )
            .await?;
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
            let key_input = ask(
                lines,
                &format!("API key or env var name [{}]", p.default_key_env),
                p.default_key_env,
            )
            .await?;
            let (api_key_env, _) = split_key_input(&key_input, p.id, &mut keys);
            let resolved = resolve_key(&api_key_env, &keys, p.id);
            let model = pick_model(
                lines,
                fetch_openai_models("https://opencode.ai/zen/v1", &resolved).await,
                "opencode/claude-sonnet-4",
            )
            .await?;
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

    println!();
    println!("Saved. Active provider: {}", p.id);
    println!();

    Ok(())
}

// ── interactive provider picker ───────────────────────────────────────────────

fn select_provider() -> anyhow::Result<usize> {
    println!();
    println!("  Select a provider:");
    println!();
    let labels: Vec<&str> = PROVIDERS.iter().map(|p| p.label).collect();
    io_tui::picker::pick(&labels, None)
}

// ── helpers ───────────────────────────────────────────────────────────────────

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

async fn pick_model(
    lines: &mut Lines,
    models: Vec<String>,
    fallback: &str,
) -> anyhow::Result<String> {
    if models.is_empty() {
        return ask(lines, &format!("Model [{fallback}]"), fallback).await;
    }

    println!();
    println!("Available models:");
    for (i, m) in models.iter().enumerate() {
        println!("  {:>3}.  {}", i + 1, m);
    }
    println!();

    let answer = ask(lines, "Model number or name [1]", "1").await?;

    if let Ok(n) = answer.trim().parse::<usize>() {
        return Ok(models
            .get(n.saturating_sub(1))
            .cloned()
            .unwrap_or_else(|| fallback.to_string()));
    }

    if answer.is_empty() {
        Ok(models[0].clone())
    } else {
        Ok(answer)
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

async fn ask(lines: &mut Lines, question: &str, default: &str) -> anyhow::Result<String> {
    print!("{question}: ");
    std::io::stdout().flush()?;
    let line = lines.next_line().await?.unwrap_or_default();
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed)
    }
}
