//! `io config …` and `io init` subcommand handlers.

use crate::ConfigAction;

pub fn handle_config(action: ConfigAction) -> anyhow::Result<()> {
    match action {
        ConfigAction::Show => {
            let config = io_runtime::config::Config::load()?;
            println!("{}", toml::to_string_pretty(&config).unwrap_or_default());
        }
        ConfigAction::Set { key, value } => {
            let mut config = io_runtime::config::Config::load()?;
            set_config_key(&mut config, &key, &value)?;
            config.save()?;
            println!("Set {key} = {value}");
        }
    }
    Ok(())
}

/// Apply a `config set <key> <value>` assignment. Supports the top-level
/// session/permission keys plus per-provider `model` and `api_key_env`
/// (e.g. `provider.anthropic.model`). Unknown keys and unparsable values
/// are errors rather than silent defaults.
fn set_config_key(
    config: &mut io_runtime::config::Config,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    fn parse_bool(key: &str, value: &str) -> anyhow::Result<bool> {
        value
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid value for {key}: expected true or false"))
    }

    /// Parse a command list from `config set`. Accepts a JSON array
    /// (`'["ls", "git"]'`) or a comma-separated string (`"ls, git"`).
    fn parse_command_list(key: &str, value: &str) -> anyhow::Result<Vec<String>> {
        let trimmed = value.trim();
        if trimmed.starts_with('[') {
            let list: Vec<String> = serde_json::from_str(trimmed)
                .map_err(|_| anyhow::anyhow!("invalid value for {key}: expected a JSON array"))?;
            Ok(list)
        } else {
            Ok(trimmed
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect())
        }
    }

    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["provider", "default"] => config.provider.default = value.to_string(),
        ["session", "auto_compact"] => config.session.auto_compact = parse_bool(key, value)?,
        ["session", "memory_enabled"] => config.session.memory_enabled = parse_bool(key, value)?,
        ["session", "max_turns"] => {
            config.session.max_turns = value
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid value for {key}: expected a number"))?;
        }
        ["session", "max_tokens"] => {
            config.session.max_tokens = value
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid value for {key}: expected a number"))?;
        }
        ["permissions", "default"] => {
            if !matches!(value, "allow" | "agent" | "prompt" | "deny") {
                anyhow::bail!("invalid value for {key}: expected allow, agent, prompt, or deny");
            }
            config.permissions.default = value.to_string();
        }
        ["permissions", "allow_network_fetch"] => {
            config.permissions.allow_network_fetch = parse_bool(key, value)?;
        }
        ["permissions", "allowed_commands"] => {
            config.permissions.allowed_commands = parse_command_list(key, value)?;
        }
        ["permissions", "denied_commands"] => {
            config.permissions.denied_commands = parse_command_list(key, value)?;
        }
        ["theme"] => {
            if !io_tui::render::THEME_NAMES.contains(&value) {
                anyhow::bail!(
                    "unknown theme: {value}\nAvailable: {}",
                    io_tui::render::THEME_NAMES.join(", ")
                );
            }
            config.theme = value.to_string();
        }
        ["provider", provider, field @ ("model" | "api_key_env" | "auth" | "deployment")] => {
            set_provider_field(config, provider, field, value)?;
        }
        _ => anyhow::bail!(
            "unknown config key: {key}\nSupported: provider.default, provider.<name>.model, \
             provider.<name>.api_key_env, provider.<name>.auth (api_key|oauth), \
             provider.azure.deployment, session.auto_compact, \
             session.memory_enabled, session.max_turns, session.max_tokens, \
             permissions.default, permissions.allow_network_fetch, \
             permissions.allowed_commands, permissions.denied_commands, theme"
        ),
    }
    Ok(())
}

fn set_provider_field(
    config: &mut io_runtime::config::Config,
    provider: &str,
    field: &str,
    value: &str,
) -> anyhow::Result<()> {
    let p = &mut config.provider;
    let value = value.to_string();

    // OAuth login is only meaningful for providers that support it
    // (openai = ChatGPT, anthropic = Claude). Handle it separately so the
    // shared macro below stays applicable to every provider config.
    if field == "auth" {
        let method = match value.as_str() {
            "oauth" => io_runtime::config::AuthMethod::OAuth,
            "api_key" => io_runtime::config::AuthMethod::ApiKey,
            _ => anyhow::bail!("invalid value for {provider}.auth: expected api_key or oauth"),
        };
        match provider {
            "openai" => p.openai.get_or_insert_with(Default::default).auth = method,
            "anthropic" => p.anthropic.get_or_insert_with(Default::default).auth = method,
            _ => anyhow::bail!("provider.{provider} does not support OAuth login"),
        }
        return Ok(());
    }

    // Each arm materializes the provider's config (with defaults) if absent,
    // then assigns the requested field.
    macro_rules! set_field {
        ($slot:expr) => {{
            let cfg = $slot.get_or_insert_with(Default::default);
            match field {
                "model" => cfg.model = value,
                "api_key_env" => cfg.api_key_env = Some(value),
                _ => anyhow::bail!("provider.{provider} has no field '{field}'"),
            }
        }};
    }

    match provider {
        "openai" => set_field!(p.openai),
        "anthropic" => set_field!(p.anthropic),
        "gemini" => set_field!(p.gemini),
        "groq" => set_field!(p.groq),
        "mistral" => set_field!(p.mistral),
        "deepseek" => set_field!(p.deepseek),
        "openrouter" => set_field!(p.openrouter),
        "xai" => set_field!(p.xai),
        "opencode_go" => set_field!(p.opencode_go),
        "opencode_zen" => set_field!(p.opencode_zen),
        "ollama" => {
            let cfg = p.ollama.get_or_insert_with(Default::default);
            match field {
                "model" => cfg.model = value,
                _ => anyhow::bail!("provider.ollama has no field '{field}' (no API key needed)"),
            }
        }
        "azure" => {
            let cfg = p.azure.get_or_insert_with(Default::default);
            match field {
                "deployment" | "model" => cfg.deployment = value,
                "api_key_env" => cfg.api_key_env = Some(value),
                _ => anyhow::bail!("provider.azure has no field '{field}'"),
            }
        }
        "bedrock" => {
            let cfg = p.bedrock.get_or_insert_with(Default::default);
            match field {
                "model" => cfg.model = value,
                _ => {
                    anyhow::bail!("provider.bedrock has no field '{field}' (uses AWS credentials)")
                }
            }
        }
        _ => anyhow::bail!("unknown provider: {provider}"),
    }
    Ok(())
}

pub fn handle_init() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let io_dir = root.join(".io");
    std::fs::create_dir_all(&io_dir)?;

    let config_path = io_dir.join("config.toml");
    if !config_path.exists() {
        let config = io_runtime::config::Config::default();
        let contents = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, contents)?;
        println!("Initialized io in {}", root.display());
    } else {
        println!("io already initialized in {}", root.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_set_supports_permission_lists_and_network_fetch() {
        let mut config = io_runtime::config::Config::default();
        set_config_key(
            &mut config,
            "permissions.allowed_commands",
            "[\"ls\", \"git\"]",
        )
        .unwrap();
        assert_eq!(config.permissions.allowed_commands, vec!["ls", "git"]);
        // Comma-separated form works too.
        set_config_key(
            &mut config,
            "permissions.allowed_commands",
            "ls, git, cargo",
        )
        .unwrap();
        assert_eq!(
            config.permissions.allowed_commands,
            vec!["ls", "git", "cargo"]
        );
        set_config_key(&mut config, "permissions.denied_commands", "[\"rm\"]").unwrap();
        assert_eq!(config.permissions.denied_commands, vec!["rm"]);
        set_config_key(&mut config, "permissions.allow_network_fetch", "true").unwrap();
        assert!(config.permissions.allow_network_fetch);
        // Invalid values are errors, not silent defaults.
        assert!(set_config_key(&mut config, "permissions.allow_network_fetch", "yes").is_err());
        assert!(set_config_key(&mut config, "permissions.allowed_commands", "[not json]").is_err());
        assert!(set_config_key(&mut config, "permissions.default", "always").is_err());
    }
}
