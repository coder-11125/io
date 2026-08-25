//! Interactive TUI and single-shot runner: agent construction, the per-turn
//! streaming/cancellation/permission dance, and slash command dispatch.

use crate::cost::show_cost_summary;
use crate::input::{clear_rows_above, resolve_at_mentions, splash_read_line, tui_read_line};
use crate::stream::{blink_and_print, push_line, LineBuf, PendingPermission, SCROLL_STEP};
use crate::{agent, connect, login, model};
use io_runtime::types::SessionId;
use io_tui::picker;
use io_tui::render::{
    clear_prompt_input, draw_prompt_bar, enter_tui, exit_tui, prepare_streaming,
    render_scroll_view, tool_detail, PROMPT_BAR_HEIGHT,
};
use std::io::Write;

// ── Session choice ─────────────────────────────────────────────────────────────

enum SessionChoice {
    New,
    Continue,
    Existing(SessionId),
}

/// Run an interactive flow with the TUI suspended: leave the alternate screen
/// and disable raw mode so line-based prompts (API keys, pasted OAuth codes)
/// and browser URLs behave normally, then re-enter the TUI and clear the stale
/// alternate-screen content. The flow's result is preserved either way.
async fn run_suspended<T>(
    flow: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    exit_tui()?;
    let result = flow.await;
    enter_tui()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
    )?;
    result
}

/// OAuth sign-in flow for `/login`: pick a provider, then run its login flow.
async fn login_flow() -> anyhow::Result<()> {
    let providers = [
        (
            "openai",
            "OpenAI (ChatGPT) — sign in with your ChatGPT account",
        ),
        (
            "anthropic",
            "Anthropic (Claude) — sign in with a Claude subscription",
        ),
    ];
    let labels: Vec<&str> = providers.iter().map(|(_, l)| *l).collect();
    let idx = picker::pick(&labels, None)?;
    login::run(providers[idx].0).await
}

// ── Project root detection ─────────────────────────────────────────────────────

fn detect_project_root() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut dir = cwd.clone();
    loop {
        if dir.join(".git").exists() || dir.join(".io").exists() {
            return dir;
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return cwd,
        }
    }
}

// ── Config helpers ─────────────────────────────────────────────────────────────

fn apply_model_spec(config: &mut io_runtime::config::Config, spec: &str) {
    let (provider, model) = match spec.split_once('/') {
        Some((p, m)) => (p, Some(m)),
        None => (spec, None),
    };
    config.provider.default = provider.to_string();
    let Some(m) = model else { return };
    let m = m.to_string();
    let p = &mut config.provider;
    match provider {
        "openai" => p.openai.get_or_insert_with(Default::default).model = m,
        "anthropic" => p.anthropic.get_or_insert_with(Default::default).model = m,
        "gemini" => p.gemini.get_or_insert_with(Default::default).model = m,
        "groq" => p.groq.get_or_insert_with(Default::default).model = m,
        "ollama" => p.ollama.get_or_insert_with(Default::default).model = m,
        "azure" => p.azure.get_or_insert_with(Default::default).deployment = m,
        "bedrock" => p.bedrock.get_or_insert_with(Default::default).model = m,
        "mistral" => p.mistral.get_or_insert_with(Default::default).model = m,
        "deepseek" => p.deepseek.get_or_insert_with(Default::default).model = m,
        "openrouter" => p.openrouter.get_or_insert_with(Default::default).model = m,
        "xai" => p.xai.get_or_insert_with(Default::default).model = m,
        "opencode_go" => p.opencode_go.get_or_insert_with(Default::default).model = m,
        "opencode_zen" => p.opencode_zen.get_or_insert_with(Default::default).model = m,
        _ => {}
    }
}

// ── Agent construction ─────────────────────────────────────────────────────────

async fn build_agent(
    session: SessionChoice,
    agent_config: &io_agents::AgentConfig,
    model_override: Option<&str>,
) -> anyhow::Result<io_runtime::Agent> {
    let mut config = io_runtime::config::Config::load()?;
    if let Some(spec) = model_override {
        apply_model_spec(&mut config, spec);
    }
    let keys = io_runtime::config::KeyStore::load();
    let model_id = config.provider.active_model();
    let pricing_override = config
        .provider
        .pricing_override_for(&config.provider.default);
    let provider = io_runtime::provider::create_provider(&config, &keys)?;
    let memory = io_runtime::memory::SessionStore::new()?;
    let project_root = detect_project_root();
    let project_context = io_runtime::load_project_context(&project_root);
    let mut checker = io_runtime::sandbox::PermissionChecker::from(&config.permissions)
        .with_project_root(project_root);
    if agent_config.auto_allow_writes {
        checker = checker.with_allowed_tools(&["write", "edit"]);
    }
    let permissions = std::sync::Arc::new(checker);

    let session_id = match session {
        SessionChoice::New => None,
        SessionChoice::Continue => {
            let sessions = memory.list_sessions()?;
            sessions.first().map(|s| s.id)
        }
        SessionChoice::Existing(id) => Some(id),
    };

    let mut tools = io_runtime::tools::default_registry();
    let spawn_tool = io_runtime::SpawnAgentTool::new(
        provider.clone(),
        model_id.clone(),
        config.session.max_tokens,
        permissions.clone(),
        project_context.clone(),
    );
    tools.register(Box::new(spawn_tool));

    Ok(io_runtime::Agent::new(
        provider,
        tools,
        memory,
        permissions,
        agent_config.system_prompt.clone(),
        project_context,
        session_id,
        model_id,
        config.session.max_tokens,
        config.session.auto_compact,
        pricing_override,
    ))
}

// ── Single-shot ────────────────────────────────────────────────────────────────

fn prompt_on_stdin(name: &str, input: &serde_json::Value) -> io_runtime::PermissionReply {
    let detail = tool_detail(name, input);
    if detail.is_empty() {
        print!("  allow tool \"{name}\"? [y]es / [a]lways / [N]o: ");
    } else {
        print!("  allow tool \"{name}\" ({detail})? [y]es / [a]lways / [N]o: ");
    }
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return io_runtime::PermissionReply::Deny;
    }
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => io_runtime::PermissionReply::AllowOnce,
        "a" | "always" => io_runtime::PermissionReply::AllowSession,
        _ => io_runtime::PermissionReply::Deny,
    }
}

pub async fn run_single_shot(prompt: &str, model_override: Option<&str>) -> anyhow::Result<()> {
    let mut config = io_runtime::config::Config::load()?;
    if let Some(spec) = model_override {
        apply_model_spec(&mut config, spec);
    }
    let keys = io_runtime::config::KeyStore::load();
    if let Some(env) = io_runtime::provider::missing_api_key(&config, &keys) {
        anyhow::bail!(
            "no API key found for provider \"{}\".\nExport {env}, or run `io` and use /connect to set one up.",
            config.provider.default
        );
    }

    let build_config = io_agents::builtin::by_id("build").expect("build agent must exist");
    let agent = build_agent(SessionChoice::New, &build_config, model_override).await?;
    agent.set_prompt_fn(std::sync::Arc::new(prompt_on_stdin));
    let response = agent.run_turn(&resolve_at_mentions(prompt)).await?;
    println!("{response}");
    Ok(())
}

// ── Interactive REPL ───────────────────────────────────────────────────────────

pub async fn run_interactive(
    new_session: bool,
    continue_session: bool,
    model: Option<&str>,
) -> anyhow::Result<()> {
    let mut config = io_runtime::config::Config::load()?;
    if let Some(spec) = model {
        apply_model_spec(&mut config, spec);
    }
    let mut theme = io_tui::render::get_theme(&config.theme);
    let line_buf: LineBuf = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::with_capacity(512),
    ));
    let keys = io_runtime::config::KeyStore::load();
    if let Some(env) = io_runtime::provider::missing_api_key(&config, &keys) {
        use crossterm::style::Stylize;
        println!(
            "{}",
            format!(
                "warning: no API key found for provider \"{}\" — requests will fail.\n         run /connect to set one up, or export {env}.",
                config.provider.default
            )
            .yellow()
        );
        println!();
    }

    let mut current_agent = io_agents::builtin::by_id("build").expect("build agent must exist");
    let startup_session = if continue_session && !new_session {
        SessionChoice::Continue
    } else {
        SessionChoice::New
    };
    let mut agent = build_agent(startup_session, &current_agent, model).await?;

    enter_tui()?;

    let mut last_input_tokens: u32 = 0;
    let mut is_splash = true;

    loop {
        let full_agents = io_agents::builtin::full_agents();
        let mut tab_current = full_agents
            .iter()
            .position(|a| a.id == current_agent.id)
            .unwrap_or(0);

        let from_splash = is_splash;
        let input = if is_splash {
            match splash_read_line(&full_agents, &mut tab_current, &agent, &theme) {
                Ok(Some(s)) => {
                    is_splash = false;
                    crossterm::execute!(
                        std::io::stdout(),
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                    )?;
                    s
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("error: {e}");
                    break;
                }
            }
        } else {
            match tui_read_line(
                &full_agents,
                &mut tab_current,
                &agent,
                last_input_tokens,
                agent.context_window(),
                current_agent.id,
                &theme,
                &line_buf,
            ) {
                Ok(Some(s)) => s,
                Ok(None) => break,
                Err(e) => {
                    let _ = draw_prompt_bar(
                        &format!("error: {e}"),
                        current_agent.name,
                        agent.provider_id,
                        &agent.model_id,
                        last_input_tokens,
                        agent.context_window(),
                        &theme,
                    );
                    continue;
                }
            }
        };

        if tab_current
            != full_agents
                .iter()
                .position(|a| a.id == current_agent.id)
                .unwrap_or(0)
        {
            if let Some(picked) = full_agents.into_iter().nth(tab_current) {
                current_agent = picked;
                let sid = agent.session_id().await;
                match build_agent(SessionChoice::Existing(sid), &current_agent, None).await {
                    Ok(new_agent) => agent = new_agent,
                    Err(e) => {
                        let _ = draw_prompt_bar(
                            &format!("error switching agent: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                        continue;
                    }
                }
            }
        }

        let input = input.trim().to_string();
        if input.is_empty() {
            continue;
        }

        match input.as_str() {
            "/exit" | "/quit" | "/q" => break,
            "/help" => {
                clear_rows_above(0)?;
                let help_lines = [
                    "Commands:",
                    "  /exit, /quit   Exit",
                    "  /help          Show this help",
                    "  /new           Start a new session",
                    "  /agent         Switch agent mode",
                    "  /connect       Set up a provider interactively",
                    "  /login         Sign in with OAuth (ChatGPT / Claude)",
                    "  /model         Switch between configured providers",
                    "  /theme         Switch UI theme",
                    "  /cost          Show API cost summary for current session",
                    "  /context       Fetch real context window, pricing, and tool support",
                    "  /compact       Summarize and compress conversation history",
                    "  !<cmd>         Run a shell command",
                ];
                let (_, h) = crossterm::terminal::size()?;
                let start_row = h.saturating_sub(PROMPT_BAR_HEIGHT + 1 + help_lines.len() as u16);
                for (i, line) in help_lines.iter().enumerate() {
                    use crossterm::{cursor, execute, style::Print, terminal};
                    execute!(
                        std::io::stdout(),
                        cursor::MoveTo(0, start_row + i as u16),
                        terminal::Clear(crossterm::terminal::ClearType::CurrentLine),
                        Print(line),
                    )?;
                }
                std::io::stdout().flush()?;
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/cost" => {
                if let Err(e) = show_cost_summary(&agent).await {
                    let _ = draw_prompt_bar(
                        &format!("error: {e}"),
                        current_agent.name,
                        agent.provider_id,
                        &agent.model_id,
                        last_input_tokens,
                        agent.context_window(),
                        &theme,
                    );
                }
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/context" => {
                match model::refresh_context_window().await {
                    Ok(msg) => {
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                        {
                            Ok(new_agent) => {
                                agent = new_agent;
                                let _ = draw_prompt_bar(
                                    &msg,
                                    current_agent.name,
                                    agent.provider_id,
                                    &agent.model_id,
                                    last_input_tokens,
                                    agent.context_window(),
                                    &theme,
                                );
                            }
                            Err(e) => {
                                let _ = draw_prompt_bar(
                                    &format!("error reloading provider: {e}"),
                                    current_agent.name,
                                    agent.provider_id,
                                    &agent.model_id,
                                    last_input_tokens,
                                    agent.context_window(),
                                    &theme,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                }
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/compact" => {
                match agent.compact().await {
                    Ok(result) if result.turns_compacted == 0 => {
                        let _ = draw_prompt_bar(
                            "Nothing to compact — session has no turns.",
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                    Ok(result) => {
                        let _ = draw_prompt_bar(
                            &format!(
                                "Compacted {} turn{} into a summary.",
                                result.turns_compacted,
                                if result.turns_compacted == 1 { "" } else { "s" }
                            ),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                    Err(e) => {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                }
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/agent" => {
                match agent::run(current_agent.id) {
                    Ok(new_config) => {
                        current_agent = new_config;
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                        {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => {
                                let _ = draw_prompt_bar(
                                    &format!("error reloading agent: {e}"),
                                    current_agent.name,
                                    agent.provider_id,
                                    &agent.model_id,
                                    last_input_tokens,
                                    agent.context_window(),
                                    &theme,
                                );
                            }
                        }
                    }
                    Err(e) if !e.is::<picker::Dismissed>() => {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                    _ => {}
                }
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/connect" => {
                match run_suspended(connect::run()).await {
                    Ok(()) => {
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                        {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => {
                                let _ = draw_prompt_bar(
                                    &format!("error reloading provider: {e}"),
                                    current_agent.name,
                                    agent.provider_id,
                                    &agent.model_id,
                                    last_input_tokens,
                                    agent.context_window(),
                                    &theme,
                                );
                            }
                        }
                    }
                    Err(e) if !e.is::<picker::Dismissed>() => {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                    _ => {}
                }
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/login" => {
                match run_suspended(login_flow()).await {
                    Ok(()) => {
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                        {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => {
                                let _ = draw_prompt_bar(
                                    &format!("error reloading provider: {e}"),
                                    current_agent.name,
                                    agent.provider_id,
                                    &agent.model_id,
                                    last_input_tokens,
                                    agent.context_window(),
                                    &theme,
                                );
                            }
                        }
                    }
                    Err(e) if !e.is::<picker::Dismissed>() => {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                    _ => {}
                }
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/model" => {
                match model::run().await {
                    Ok(()) => {
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                        {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => {
                                let _ = draw_prompt_bar(
                                    &format!("error reloading provider: {e}"),
                                    current_agent.name,
                                    agent.provider_id,
                                    &agent.model_id,
                                    last_input_tokens,
                                    agent.context_window(),
                                    &theme,
                                );
                            }
                        }
                    }
                    Err(e) if !e.is::<picker::Dismissed>() => {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                    _ => {}
                }
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/new" => {
                match build_agent(SessionChoice::New, &current_agent, None).await {
                    Ok(new_agent) => {
                        agent = new_agent;
                        last_input_tokens = 0;
                        line_buf.lock().unwrap().clear();
                        is_splash = true;
                    }
                    Err(e) => {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                        if from_splash {
                            is_splash = true;
                        }
                    }
                }
                continue;
            }
            "/theme" => {
                match io_tui::theme::run(theme.name) {
                    Ok(name) => {
                        theme = io_tui::render::get_theme(name);
                        is_splash = true;
                    }
                    Err(e) if !e.is::<picker::Dismissed>() => {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                    _ => {
                        if from_splash {
                            is_splash = true;
                        }
                    }
                }
                continue;
            }
            _ if input.starts_with('!') => {
                let cmd = input[1..].trim();
                let output = run_bash(cmd).await;
                prepare_streaming()?;
                {
                    use crossterm::{
                        execute,
                        style::{Color, Print, ResetColor, SetForegroundColor},
                    };
                    const MAX_LINES: usize = 50;
                    const MAX_CHARS: usize = 200;
                    let lines: Vec<&str> = output.lines().collect();
                    let truncated = lines.len() > MAX_LINES;
                    for line in lines.iter().take(MAX_LINES) {
                        let s: String = line
                            .trim_end_matches('\r')
                            .chars()
                            .take(MAX_CHARS)
                            .collect();
                        execute!(
                            std::io::stdout(),
                            SetForegroundColor(Color::DarkGrey),
                            Print(format!("  {s}\r\n")),
                            ResetColor,
                        )?;
                        push_line(&line_buf, format!("  {s}"));
                    }
                    if truncated {
                        let remaining = lines.len() - MAX_LINES;
                        let msg = format!(
                            "  … {} more line{}",
                            remaining,
                            if remaining == 1 { "" } else { "s" }
                        );
                        execute!(
                            std::io::stdout(),
                            SetForegroundColor(Color::DarkGrey),
                            Print(format!("{msg}\r\n")),
                            ResetColor,
                        )?;
                        push_line(&line_buf, msg);
                    }
                    std::io::stdout().flush()?;
                }
                render_scroll_view(&line_buf.lock().unwrap(), 0, &theme, PROMPT_BAR_HEIGHT)?;
                continue;
            }
            _ => {}
        }

        let prompt = resolve_at_mentions(&input);
        let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        agent.set_cancel(cancel_flag.clone());

        let (token_tx, token_rx) = tokio::sync::mpsc::channel::<io_runtime::AgentEvent>(64);
        let pending_perm: PendingPermission = std::sync::Arc::new(std::sync::Mutex::new(None));

        clear_prompt_input()?;
        prepare_streaming()?;

        {
            use crossterm::{
                execute,
                style::{Print, ResetColor, SetForegroundColor},
            };
            execute!(
                std::io::stdout(),
                SetForegroundColor(theme.muted),
                Print(format!("  {}\r\n\r\n", input)),
                ResetColor,
            )?;
        }
        push_line(&line_buf, format!("  > {}", input));
        push_line(&line_buf, String::new());

        let print_task = tokio::spawn(blink_and_print(
            token_rx,
            pending_perm.clone(),
            theme,
            line_buf.clone(),
        ));

        let cancel_for_listener = cancel_flag.clone();
        let pending_for_listener = pending_perm.clone();
        let streaming_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let streaming_done2 = streaming_done.clone();
        let (esc_tx, esc_rx) = tokio::sync::oneshot::channel::<()>();
        let stream_scroll = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stream_scroll2 = stream_scroll.clone();
        let line_buf_scroll = line_buf.clone();
        let key_listener = tokio::task::spawn_blocking(move || {
            use crossterm::event;
            use io_runtime::PermissionReply;
            loop {
                if streaming_done2.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }

                // A permission request pauses the stream until answered. Open
                // the arrow picker modal as soon as one is pending — no key
                // press is required to trigger it.
                let pending = pending_for_listener.lock().unwrap().take();
                if let Some(pending) = pending {
                    let title = if pending.detail.is_empty() {
                        format!("allow \"{}\"?", pending.name)
                    } else {
                        format!("allow \"{}\" ({})?", pending.name, pending.detail)
                    };
                    // Default to "No" (deny) — the safe choice.
                    let reply = match picker::pick_permission(
                        &title,
                        &[
                            ("Yes, once", "allow this one call"),
                            ("Always", "allow for the rest of the session"),
                            ("No", "deny"),
                        ],
                        2,
                    ) {
                        Ok(0) => PermissionReply::AllowOnce,
                        Ok(1) => PermissionReply::AllowSession,
                        // No, Esc, q, Ctrl+C, or picker failure.
                        _ => PermissionReply::Deny,
                    };
                    let label = match reply {
                        PermissionReply::AllowOnce => "yes",
                        PermissionReply::AllowSession => "always",
                        PermissionReply::Deny => "no",
                    };
                    print!("  → {label}\r\n");
                    let _ = std::io::stdout().flush();
                    let _ = pending.respond.send(reply);
                    continue;
                }

                if event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
                    match event::read() {
                        Ok(crossterm::event::Event::Key(k))
                            if k.code == crossterm::event::KeyCode::Esc =>
                        {
                            cancel_for_listener.store(true, std::sync::atomic::Ordering::Relaxed);
                            let _ = esc_tx.send(());
                            break;
                        }
                        Ok(crossterm::event::Event::Mouse(m)) => {
                            let cur = stream_scroll2.load(std::sync::atomic::Ordering::Relaxed);
                            let n = line_buf_scroll.lock().unwrap().len();
                            let next = match m.kind {
                                crossterm::event::MouseEventKind::ScrollUp => {
                                    (cur + SCROLL_STEP).min(n)
                                }
                                crossterm::event::MouseEventKind::ScrollDown if cur > 0 => {
                                    cur.saturating_sub(SCROLL_STEP)
                                }
                                _ => cur,
                            };
                            if next != cur {
                                stream_scroll2.store(next, std::sync::atomic::Ordering::Relaxed);
                                let _ = io_tui::render::render_scroll_view(
                                    &line_buf_scroll.lock().unwrap(),
                                    next,
                                    &theme,
                                    PROMPT_BAR_HEIGHT,
                                );
                                let _ =
                                    crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide);
                            }
                        }
                        Ok(crossterm::event::Event::Resize(_, _)) => {
                            let _ = io_tui::render::handle_resize();
                        }
                        _ => {}
                    }
                }
            }
        });

        tokio::select! {
            result = agent.run_turn_streaming(&prompt, token_tx) => {
                if let Err(e) = result {
                    if !e.is::<io_runtime::Cancelled>() {
                        let _ = draw_prompt_bar(
                            &format!("error: {e}"),
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                }
            }
            _ = esc_rx => {}
        }
        streaming_done.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = key_listener.await;

        let (agent_thoughts, turn_input_tokens) = print_task.await.unwrap_or((None, 0));
        if turn_input_tokens > last_input_tokens {
            last_input_tokens = turn_input_tokens;
        }
        if let Some(ref t) = agent_thoughts {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                push_line(&line_buf, String::new());
                let prefix = "(thought): ";
                let indent = " ".repeat(prefix.len());
                let ac = io_tui::render::ansi_fg(theme.accent);
                let mu = io_tui::render::ansi_fg(theme.muted);
                let mut thought_lines = trimmed.lines();
                if let Some(first) = thought_lines.next() {
                    push_line(
                        &line_buf,
                        format!("\x01{ac}{prefix}\x1b[0m{mu}{first}\x1b[0m"),
                    );
                    for line in thought_lines {
                        push_line(&line_buf, format!("\x01{indent}{mu}{line}\x1b[0m"));
                    }
                }
                push_line(&line_buf, String::new());
            }
        }
        render_scroll_view(&line_buf.lock().unwrap(), 0, &theme, PROMPT_BAR_HEIGHT)?;
        draw_prompt_bar(
            "",
            current_agent.name,
            agent.provider_id,
            &agent.model_id,
            last_input_tokens,
            agent.context_window(),
            &theme,
        )?;
    }

    exit_tui()?;
    Ok(())
}

// ── Shell passthrough ──────────────────────────────────────────────────────────

async fn run_bash(cmd: &str) -> String {
    const ALLOWED_SHELLS: &[&str] = &[
        "/bin/sh",
        "/bin/bash",
        "/usr/bin/bash",
        "/bin/zsh",
        "/usr/bin/zsh",
        "/usr/local/bin/bash",
        "/usr/local/bin/zsh",
        "/usr/local/bin/sh",
    ];
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| ALLOWED_SHELLS.contains(&s.as_str()))
        .unwrap_or_else(|| "/bin/sh".to_string());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(&shell)
            .arg("-lc")
            .arg(cmd)
            .output(),
    )
    .await;

    match output {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.status.success() {
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                text.push_str(&format!("\nexit code: {}", out.status.code().unwrap_or(-1)));
            }
            text
        }
        Ok(Err(e)) => format!("error: {e}"),
        Err(_) => "command timed out".to_string(),
    }
}
