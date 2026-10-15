//! Interactive TUI and single-shot runner: agent construction, the per-turn
//! streaming/cancellation/permission dance, and slash command dispatch.

use crate::cost::show_cost_summary;
use crate::input::{
    clear_rows_above, resolve_at_mentions_with_images, splash_read_line, tui_read_line,
};
use crate::stream::{
    blink_and_print, push_line, LineBuf, PendingPerm, PendingPermission, SCROLL_STEP,
};
use crate::{
    agent, connect, doctor, info_cmd, login, mcp_cmd, memory_cmd, model, permissions, status,
    statusline, style_cmd,
};
use io_runtime::types::SessionId;
use io_runtime::PermissionReply;
use io_tui::picker;
use io_tui::render::{
    clear_prompt_input, draw_prompt_bar, enter_tui, exit_tui, prepare_streaming,
    render_scroll_view, tool_detail, PROMPT_BAR_HEIGHT,
};
use std::io::Write;

// ── Session choice ─────────────────────────────────────────────────────────────

pub(crate) enum SessionChoice {
    New,
    Continue,
    Existing(SessionId),
}

/// Run a flow with mouse capture off. `/connect` and `/login` spend their
/// whole flow in text-entry popups where mouse tracking never contributes
/// anything — and on terminals that report mouse motion continuously, an
/// in-flight mouse-motion escape sequence can race a bare Esc keypress,
/// making Esc look unresponsive until the ambiguous bytes drain. No other
/// terminal state (raw mode, alternate screen) is touched.
async fn without_mouse_capture<T>(
    flow: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    let result = flow.await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    result
}

/// OAuth sign-in flow for `/login`: pick a provider, then run its login flow.
/// Renders entirely as in-TUI popups — never leaves the alternate screen.
async fn login_flow() -> anyhow::Result<String> {
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
    let idx = picker::pick_no_esc(&labels, None)?;
    login::run(providers[idx].0).await
}

// ── Project root detection ─────────────────────────────────────────────────────

pub(crate) fn detect_project_root() -> std::path::PathBuf {
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

/// "`session_start` hooks already fired" for the current session, shared by
/// every agent built for it: `/model`, `/agent` and friends rebuild the agent
/// but must not re-announce the session. `/new` resets it.
/// Read `settings.json` once at startup: report anything unusable and start in
/// `permissions.defaultMode`.
pub(crate) fn apply_startup_settings(root: &std::path::Path) -> Vec<String> {
    let settings = io_runtime::settings::Settings::load(root);
    status::set_vim_enabled(settings.editor_mode.as_deref() == Some("vim"));
    if let (Some(mode), false) = (settings.default_mode, crate::run_opts::get().explicit_mode) {
        status::set_initial_mode(mode);
    }
    settings
        .problems
        .iter()
        .map(|p| format!("settings: {p}"))
        .collect()
}

fn session_gate() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    static GATE: std::sync::OnceLock<std::sync::Arc<std::sync::atomic::AtomicBool>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(Default::default).clone()
}

pub(crate) async fn build_agent(
    session: SessionChoice,
    agent_config: &io_agents::AgentConfig,
    model_override: Option<&str>,
) -> anyhow::Result<io_runtime::Agent> {
    let mut config = io_runtime::config::Config::load()?;
    if let Some(spec) = model_override {
        apply_model_spec(&mut config, spec);
    }
    let run_opts = crate::run_opts::get();
    if run_opts.bypass_permissions {
        config.permissions.default = "allow".to_string();
    }
    let keys = io_runtime::config::KeyStore::load();
    let model_id = config.provider.active_model();
    let pricing_override = config
        .provider
        .pricing_override_for(&config.provider.default);
    let provider = io_runtime::provider::create_provider(&config, &keys)?;
    let memory = io_runtime::memory::SessionStore::new()?;
    let project_root = detect_project_root();
    let cwd = std::env::current_dir().unwrap_or_else(|_| project_root.clone());
    let (project_context, memory_files) = io_runtime::instructions::load(&project_root, &cwd);
    // Hooks come from the global config only (see `io_runtime::hooks`) and
    // run in the project root.
    let hooks = io_runtime::Hooks::new(config.hooks.clone(), project_root.clone())
        .with_session_gate(session_gate());
    let settings = io_runtime::settings::Settings::load(&project_root);
    let sandbox = settings.sandbox.enabled.then(|| {
        io_runtime::os_sandbox::SandboxPolicy::new(
            project_root.clone(),
            settings.sandbox.network,
            settings.sandbox.writable_paths.clone(),
        )
    });
    let mut checker = permissions::apply(
        io_runtime::sandbox::PermissionChecker::from(&config.permissions)
            .with_project_root(project_root.clone())
            .with_interaction_mode(status::current_mode()),
        &settings,
        &project_root,
    );
    if agent_config.auto_allow_writes {
        checker = checker.with_allowed_tools(io_runtime::sandbox::FILE_WRITE_TOOLS);
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
    // `fetch` answers a `prompt` about a page by running a one-shot
    // completion with the session's own model.
    tools.register(Box::new(
        io_runtime::tools::FetchTool::new().with_extractor(provider.clone()),
    ));
    let spawn_tool = io_runtime::SpawnAgentTool::new(
        provider.clone(),
        model_id.clone(),
        config.session.max_tokens,
        permissions.clone(),
        project_context.clone(),
    )
    .with_hooks(&hooks)
    .with_sandbox(sandbox.clone());
    tools.register(Box::new(spawn_tool));
    if let Some(policy) = sandbox {
        tools.register(Box::new(
            io_runtime::tools::BashTool::new().with_sandbox(policy),
        ));
    }
    let mcp = mcp_cmd::manager();
    for tool in mcp.tools() {
        tools.register(tool);
    }
    permissions.mark_read_only(&mcp.read_only_tool_names());

    status::update_badge(status::current_mode(), &config.thinking);

    Ok(io_runtime::Agent::new(
        provider,
        tools,
        memory,
        permissions,
        format!(
            "{}{}{}",
            agent_config.full_system_prompt(),
            run_opts
                .append_system_prompt
                .as_deref()
                .map(|extra| format!("\n\n{extra}"))
                .unwrap_or_default(),
            io_runtime::output_style::prompt_for(
                settings.output_style.as_deref(),
                &io_runtime::output_style::discover(&project_root, dirs::home_dir().as_deref()),
            )
        ),
        project_context,
        session_id,
        model_id,
        config.session.max_tokens,
        config.session.auto_compact,
        pricing_override,
    )
    .with_hooks(hooks)
    .with_max_iterations(run_opts.max_turns.unwrap_or(0))
    .with_nested_memory(
        project_root,
        io_runtime::instructions::loaded_set(&memory_files),
    )
    .with_thinking(config.thinking.clone()))
}

// ── Permission dialogs ─────────────────────────────────────────────────────────

/// One option in a permission dialog: its label and hint, the reply it
/// stands for, and what to echo after it is chosen.
struct Choice {
    label: &'static str,
    hint: &'static str,
    reply: PermissionReply,
    echo: &'static str,
}

struct PermissionDialog {
    title: String,
    choices: Vec<Choice>,
    /// The option preselected when the dialog opens (always the safe one).
    initial: usize,
}

fn permission_dialog(pending: &PendingPerm) -> PermissionDialog {
    if pending.name == "exit_plan_mode" {
        // The plan itself was printed above when the call started.
        return PermissionDialog {
            title: "Ready to leave plan mode and start on this plan?".to_string(),
            choices: vec![
                Choice {
                    label: "Yes, and accept edits",
                    hint: "approve the plan; file edits run without asking",
                    reply: PermissionReply::AllowSession,
                    echo: "plan approved — accepting edits",
                },
                Choice {
                    label: "Yes, and approve edits manually",
                    hint: "approve the plan; keep the usual edit prompts",
                    reply: PermissionReply::AllowOnce,
                    echo: "plan approved",
                },
                Choice {
                    label: "No, keep planning",
                    hint: "stay in plan mode and revise",
                    reply: PermissionReply::Deny,
                    echo: "plan not approved — still planning",
                },
            ],
            initial: 2,
        };
    }
    let title = if pending.detail.is_empty() {
        format!("allow \"{}\"?", pending.name)
    } else {
        format!("allow \"{}\" ({})?", pending.name, pending.detail)
    };
    PermissionDialog {
        title,
        choices: vec![
            Choice {
                label: "Yes, once",
                hint: "allow this one call",
                reply: PermissionReply::AllowOnce,
                echo: "yes",
            },
            Choice {
                label: "Always",
                hint: "allow for the rest of the session",
                reply: PermissionReply::AllowSession,
                echo: "always",
            },
            Choice {
                label: "No",
                hint: "deny",
                reply: PermissionReply::Deny,
                echo: "no",
            },
        ],
        // Default to "No" (deny) — the safe choice.
        initial: 2,
    }
}

/// Show `dialog` and return the reply plus what to echo. Esc, `q`, Ctrl+C or
/// a picker failure all count as the last option, which is always a denial.
fn ask_permission(dialog: &PermissionDialog) -> (PermissionReply, &'static str) {
    let items: Vec<(&str, &str)> = dialog.choices.iter().map(|c| (c.label, c.hint)).collect();
    let chosen = picker::pick_permission(&dialog.title, &items, dialog.initial)
        .ok()
        .and_then(|i| dialog.choices.get(i))
        .unwrap_or_else(|| dialog.choices.last().expect("dialogs have options"));
    (chosen.reply, chosen.echo)
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

    for note in apply_startup_settings(&detect_project_root()) {
        eprintln!("{note}");
    }
    for note in mcp_cmd::start(false, &detect_project_root()).await {
        eprintln!("{note}");
    }
    let build_config = io_agents::builtin::by_id("build").expect("build agent must exist");
    let agent = build_agent(SessionChoice::New, &build_config, model_override).await?;
    agent.set_prompt_fn(std::sync::Arc::new(prompt_on_stdin));
    let (prompt, images) = resolve_at_mentions_with_images(prompt);
    let response = agent.run_turn_with_images(&prompt, images).await;
    agent.end_session("single_shot").await;
    println!("{}", response?);
    Ok(())
}

// ── Interactive REPL ───────────────────────────────────────────────────────────

/// Show the tail of a resumed conversation in the scrollback.
fn show_replay(
    turns: &[io_runtime::types::Turn],
    line_buf: &LineBuf,
    theme: &io_tui::render::Theme,
) -> anyhow::Result<()> {
    line_buf.lock().unwrap().clear();
    for line in crate::session_cmd::replay_lines(turns, 6) {
        push_line(line_buf, line);
    }
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
    )?;
    render_scroll_view(&line_buf.lock().unwrap(), 0, theme, PROMPT_BAR_HEIGHT)?;
    Ok(())
}

pub async fn run_interactive(
    new_session: bool,
    continue_session: bool,
    resume: Option<&str>,
    model: Option<&str>,
) -> anyhow::Result<()> {
    let mut config = io_runtime::config::Config::load()?;
    if let Some(spec) = model {
        apply_model_spec(&mut config, spec);
    }
    // Best-effort, non-blocking: fills in context window / pricing from the
    // models.dev catalog if either is missing for the active provider's
    // model. Never awaited, never surfaces errors — see its doc comment.
    // Takes effect on the next agent (re)build, not the one below.
    tokio::spawn(model::auto_fill_missing_model_info());
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

    // Before the TUI takes the screen: project servers may need a y/n.
    for note in apply_startup_settings(&detect_project_root()) {
        println!("{note}");
    }
    for note in mcp_cmd::start(true, &detect_project_root()).await {
        println!("{note}");
    }

    let mut current_agent = io_agents::builtin::by_id("build").expect("build agent must exist");
    let startup_session = if let Some(id) = resume {
        SessionChoice::Existing(crate::session_cmd::resolve_id(
            &io_runtime::memory::SessionStore::new()?,
            id,
        )?)
    } else if continue_session && !new_session {
        SessionChoice::Continue
    } else {
        SessionChoice::New
    };
    let resuming = resume.is_some() || (continue_session && !new_session);
    let mut agent = build_agent(startup_session, &current_agent, model).await?;

    enter_tui()?;

    let mut last_input_tokens: u32 = 0;
    let mut is_splash = true;
    let mut history = crate::history::History::load();
    // Messages typed while the model was busy, in the order they were sent.
    let mut pending_inputs: std::collections::VecDeque<String> = Default::default();
    // Text carried from the splash box into the full prompt (Alt+Enter).
    let mut seed = String::new();
    if resuming {
        let turns = agent.session_turns().await;
        if !turns.is_empty() {
            show_replay(&turns, &line_buf, &theme)?;
            is_splash = false;
        }
    }

    loop {
        {
            let root = detect_project_root();
            let cwd = std::env::current_dir().unwrap_or_else(|_| root.clone());
            let session_id = agent.session_id().await.to_string();
            statusline::refresh(
                &root,
                &statusline::Snapshot {
                    session_id: &session_id,
                    agent: current_agent.id,
                    provider: agent.provider_id,
                    model: &agent.model_id,
                    mode: agent.interaction_mode().label(),
                    cwd: &cwd,
                    project_dir: &root,
                    context_tokens: last_input_tokens,
                    context_window: agent.context_window(),
                },
            )
            .await;
        }
        let full_agents = io_agents::builtin::full_agents();
        let mut tab_current = full_agents
            .iter()
            .position(|a| a.id == current_agent.id)
            .unwrap_or(0);

        let from_splash = is_splash;
        let input = if let Some(next) = pending_inputs.pop_front() {
            next
        } else if is_splash {
            match splash_read_line(&full_agents, &mut tab_current, &agent, &theme) {
                Ok(Some(s)) => {
                    is_splash = false;
                    crossterm::execute!(
                        std::io::stdout(),
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                    )?;
                    if let Some(typed) = s.strip_prefix(crate::input::CONTINUE_MARKER) {
                        // Alt+Enter on the splash: keep typing in the full prompt.
                        seed = typed.to_string();
                        continue;
                    }
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
                &history,
                &std::mem::take(&mut seed),
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
        history.push(&input);

        match input.as_str() {
            "/exit" | "/quit" | "/q" => break,
            "/help" => {
                print_info_lines(&[
                    "Commands:",
                    "  /exit, /quit   Exit",
                    "  /help          Show this help",
                    "  /new, /clear   Start a new session",
                    "  /resume        Go back to an earlier session",
                    "  /rename <n>    Name this session",
                    "  /fork          Branch this session into a copy",
                    "  /export [f]    Write this session to a Markdown file",
                    "  /agent         Switch agent mode",
                    "  /connect       Set up a provider interactively",
                    "  /login         Sign in with OAuth (ChatGPT / Claude)",
                    "  /model         Switch between configured providers",
                    "  /theme         Switch UI theme",
                    "  /config        Toggle session & permission settings",
                    "  /cost          Show API cost summary for current session",
                    "  /context       Fetch real context window, pricing, and tool support",
                    "  /compact       Summarize and compress conversation history",
                    "  /output-style  Change how the agent talks (concise, explanatory, …)",
                    "  /status        Version, model, mode, settings, rules, memory, MCP",
                    "  /context       Where the context window is going (/context refresh: re-fetch limits)",
                    "  /todos         The model's current task list",
                    "  /hooks         Configured hooks",
                    "  /agents        Available agents and sub-agents",
                    "  /doctor        Check the installation and environment",
                    "  /add-dir <d>   Let file tools reach another directory this session",
                    "  /logout [p]    Sign out of an OAuth login",
                    "  /usage         API usage and cost (same as /cost)",
                    "  /terminal-setup  Multi-line input and image paste setup",
                    "  /bug           How to report a problem",
                    "  /review [t]    Review uncommitted changes, a PR number or a branch",
                    "  /commit [note] Commit the current changes in this repo's style",
                    "  /pr [note]     Open a pull request for this branch",
                    "  /vim           Toggle Vim-style editing in the prompt",
                    "  Ctrl+R         Search earlier prompts · Up/Down browse them",
                    "  Alt+Enter      New line (also Shift+Enter, Ctrl+J, or end a line with \\)",
                    "  Ctrl+O         Show the full transcript with tool calls",
                    "  /init          Have the agent write a CLAUDE.md for this project",
                    "  /memory        Open a memory file (CLAUDE.md) in your editor",
                    "  #<note>        Save a note to memory (a '#', a space, then the note)",
                    "  /permissions   Show or edit allow/ask/deny rules",
                    "  /mcp           List MCP servers (/mcp reconnect restarts them)",
                    "  /rewind        Restore files to an earlier point in the chat (or Esc Esc)",
                    "  /plan          Enter or leave plan mode (read-only research, then approve a plan)",
                    "  /think [x]     Extended thinking: on|off|low|medium|high|<tokens> (or Alt+T)",
                    "  !<cmd>         Run a shell command",
                    "  Shift+Tab      Cycle mode: default → accept edits → plan",
                    "  Ctrl+V         Attach an image from the clipboard (or @image.png)",
                ])?;
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
            "/context refresh" => {
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
            "/rewind" => {
                let turns = agent.session_turns().await;
                if turns.is_empty() {
                    let _ = draw_prompt_bar(
                        "Nothing to rewind — session has no turns.",
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
                    continue;
                }

                let labels: Vec<(String, String)> = turns
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let msg: String = t.user_message.chars().take(60).collect();
                        let msg = msg.replace('\n', " ");
                        (
                            format!("{}. {msg}", i + 1),
                            format!("{} tool call(s)", t.tool_calls.len()),
                        )
                    })
                    .collect();
                let items: Vec<(&str, &str)> = labels
                    .iter()
                    .map(|(l, h)| (l.as_str(), h.as_str()))
                    .collect();

                println!();
                match picker::pick_with_hint(&items, None) {
                    Ok(turn_index) => {
                        let confirm_label = format!(
                            "Rewind — discard {} turn(s), restore file(s) written since",
                            turns.len() - turn_index
                        );
                        println!();
                        match picker::pick(&["Cancel", &confirm_label], None) {
                            Ok(1) => match agent.rewind_to(turn_index).await {
                                Ok(result) => {
                                    let msg = if result.files_skipped.is_empty() {
                                        format!(
                                            "Rewound {} turn(s), restored {} file(s).",
                                            result.turns_dropped,
                                            result.files_restored.len()
                                        )
                                    } else {
                                        format!(
                                            "Rewound {} turn(s), restored {} file(s) — {} file(s) had no snapshot and were left unchanged.",
                                            result.turns_dropped,
                                            result.files_restored.len(),
                                            result.files_skipped.len()
                                        )
                                    };
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
                                        &format!("error: {e}"),
                                        current_agent.name,
                                        agent.provider_id,
                                        &agent.model_id,
                                        last_input_tokens,
                                        agent.context_window(),
                                        &theme,
                                    );
                                }
                            },
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
                match without_mouse_capture(connect::run()).await {
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
                match without_mouse_capture(login_flow()).await {
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
            "/new" | "/clear" => {
                agent.end_session("new").await;
                session_gate().store(false, std::sync::atomic::Ordering::SeqCst);
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
            "/config" => {
                match io_tui::settings::run() {
                    Ok(true) => {
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
                    Ok(false) => {}
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
            _ if input == "/mcp" || input == "/mcp reconnect" => {
                let mut lines = Vec::new();
                if input == "/mcp reconnect" {
                    lines.extend(mcp_cmd::start(false, &detect_project_root()).await);
                    let sid = agent.session_id().await;
                    match build_agent(SessionChoice::Existing(sid), &current_agent, None).await {
                        Ok(new_agent) => agent = new_agent,
                        Err(e) => lines.push(format!("error reloading agent: {e}")),
                    }
                }
                let states = mcp_cmd::manager().states().to_vec();
                if states.is_empty() {
                    lines.push("No MCP servers configured — add one with `io mcp add`.".into());
                }
                lines.extend(mcp_cmd::state_lines(&states));
                print_info_lines(&lines)?;
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            _ if input == "/output-style" || input.starts_with("/output-style ") => {
                let result =
                    style_cmd::run(&input["/output-style".len()..], &detect_project_root());
                let msg = match result {
                    Ok(Some(msg)) => {
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                        {
                            Ok(new_agent) => {
                                agent = new_agent;
                                msg
                            }
                            Err(e) => format!("error reloading agent: {e}"),
                        }
                    }
                    Ok(None) => String::new(),
                    Err(e) => format!("error: {e}"),
                };
                if !msg.is_empty() {
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
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/resume" => {
                let current = agent.session_id().await;
                let picked = io_runtime::memory::SessionStore::new()
                    .and_then(|store| crate::session_cmd::pick_session(&store, current));
                let msg = match picked {
                    Ok(Some(id)) => {
                        agent.end_session("resume").await;
                        session_gate().store(false, std::sync::atomic::Ordering::SeqCst);
                        match build_agent(SessionChoice::Existing(id), &current_agent, None).await {
                            Ok(new_agent) => {
                                agent = new_agent;
                                last_input_tokens = 0;
                                let turns = agent.session_turns().await;
                                show_replay(&turns, &line_buf, &theme)?;
                                is_splash = false;
                                format!("Resumed session {}", crate::session_cmd::short_id(id))
                            }
                            Err(e) => format!("error: {e}"),
                        }
                    }
                    Ok(None) => "No other sessions to resume.".to_string(),
                    Err(e) if e.is::<picker::Dismissed>() => String::new(),
                    Err(e) => format!("error: {e}"),
                };
                if !msg.is_empty() {
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
                if from_splash && is_splash {
                    is_splash = true;
                }
                continue;
            }
            _ if input == "/export" || input.starts_with("/export ") => {
                let session = agent.session_snapshot().await;
                let cwd = std::env::current_dir().unwrap_or_default();
                let path =
                    crate::session_cmd::export_path(&input["/export".len()..], &session, &cwd);
                let msg = match std::fs::write(&path, io_runtime::export::to_markdown(&session)) {
                    Ok(()) => format!("Exported to {}", path.display()),
                    Err(e) => format!("error: {}: {e}", path.display()),
                };
                let _ = draw_prompt_bar(
                    &msg,
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
                continue;
            }
            _ if input == "/rename" || input.starts_with("/rename ") => {
                let name = input["/rename".len()..].trim().to_string();
                let msg = if name.is_empty() {
                    "usage: /rename <name>".to_string()
                } else {
                    match agent.rename_session(&name).await {
                        Ok(()) => format!("Session renamed to \"{name}\""),
                        Err(e) => format!("error: {e}"),
                    }
                };
                let _ = draw_prompt_bar(
                    &msg,
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
                continue;
            }
            "/fork" => {
                let original = agent.session_id().await;
                let msg = match agent.fork_session().await {
                    Ok(id) => {
                        match build_agent(SessionChoice::Existing(id), &current_agent, None).await {
                            Ok(new_agent) => {
                                agent = new_agent;
                                format!(
                                    "Forked: now on {} (the original is {})",
                                    crate::session_cmd::short_id(id),
                                    crate::session_cmd::short_id(original)
                                )
                            }
                            Err(e) => format!("error: {e}"),
                        }
                    }
                    Err(e) => format!("error: {e}"),
                };
                let _ = draw_prompt_bar(
                    &msg,
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
                continue;
            }
            "/status" | "/context" | "/hooks" | "/agents" | "/todos" | "/bug"
            | "/terminal-setup" | "/doctor" => {
                let root = detect_project_root();
                let lines = match input.as_str() {
                    "/status" => status_report(&agent, &current_agent, &root).await,
                    "/context" => info_cmd::context_lines(
                        &agent.context_breakdown().await,
                        agent.context_window(),
                        last_input_tokens,
                    ),
                    "/hooks" => info_cmd::hooks_lines(
                        &io_runtime::config::Config::load()
                            .map(|c| c.hooks)
                            .unwrap_or_default(),
                    ),
                    "/agents" => info_cmd::agents_lines(current_agent.id),
                    "/todos" => info_cmd::todos_lines(&agent.session_turns().await),
                    "/bug" => info_cmd::bug_lines(
                        env!("CARGO_PKG_VERSION"),
                        agent.provider_id,
                        &agent.model_id,
                    ),
                    "/terminal-setup" => info_cmd::terminal_setup_lines(),
                    _ => doctor::render(&doctor::run_checks(&root)),
                };
                print_info_lines(&lines)?;
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/usage" => {
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
            _ if input == "/logout" || input.starts_with("/logout ") => {
                let arg = input["/logout".len()..].trim();
                let msg = match login::logout((!arg.is_empty()).then_some(arg)) {
                    Ok(msg) => {
                        let sid = agent.session_id().await;
                        if let Ok(new_agent) =
                            build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                        {
                            agent = new_agent;
                        }
                        msg
                    }
                    Err(e) => format!("error: {e}"),
                };
                let _ = draw_prompt_bar(
                    &msg,
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
                continue;
            }
            _ if input == "/add-dir" || input.starts_with("/add-dir ") => {
                let arg = input["/add-dir".len()..].trim();
                let msg = if arg.is_empty() {
                    "usage: /add-dir <directory>".to_string()
                } else {
                    match permissions::add_dir(std::path::Path::new(arg)) {
                        Ok(dir) => {
                            let sid = agent.session_id().await;
                            if let Ok(new_agent) =
                                build_agent(SessionChoice::Existing(sid), &current_agent, None)
                                    .await
                            {
                                agent = new_agent;
                            }
                            format!("Added {} for this session", dir.display())
                        }
                        Err(e) => format!("error: {e}"),
                    }
                };
                let _ = draw_prompt_bar(
                    &msg,
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
                continue;
            }
            "/vim" => {
                let on = !status::vim_enabled();
                status::set_vim_enabled(on);
                status::set_vim_label(&agent, None);
                let paths =
                    io_runtime::settings::SettingsPaths::for_project(&detect_project_root());
                let saved = io_runtime::settings::set_value(
                    &paths.user,
                    "editorMode",
                    serde_json::json!(if on { "vim" } else { "normal" }),
                );
                let msg = match saved {
                    Ok(()) if on => "Vim mode on — Esc for Normal mode, i to insert",
                    Ok(()) => "Vim mode off",
                    Err(_) if on => "Vim mode on (could not save the setting)",
                    Err(_) => "Vim mode off (could not save the setting)",
                };
                let _ = draw_prompt_bar(
                    msg,
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
                continue;
            }
            "/transcript" => {
                let lines = crate::session_cmd::transcript_lines(&agent.session_turns().await);
                let _ = io_tui::pager::show(&lines, "transcript");
                // The pager used the whole screen: bring the REPL back.
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                );
                let _ = render_scroll_view(&line_buf.lock().unwrap(), 0, &theme, PROMPT_BAR_HEIGHT);
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/memory" => {
                match memory_cmd::edit_menu(&detect_project_root()) {
                    Ok(Some(msg)) => {
                        // Rebuild so edits to memory files take effect now.
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
                    Ok(None) => {}
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
                is_splash = true;
                continue;
            }
            _ if memory_cmd::is_quick_add(&input) => {
                match memory_cmd::quick_add(&input, &detect_project_root()) {
                    Ok(Some(msg)) => {
                        let sid = agent.session_id().await;
                        if let Ok(new_agent) =
                            build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                        {
                            agent = new_agent;
                        }
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
                    Ok(None) => {}
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
            _ if input == "/permissions" || input.starts_with("/permissions ") => {
                let root = detect_project_root();
                let paths = io_runtime::settings::SettingsPaths::for_project(&root);
                let lines =
                    match permissions::command(&input["/permissions".len()..], &paths, &root) {
                        Ok(lines) => lines,
                        Err(e) => vec![format!("error: {e}")],
                    };
                // Rebuild so new rules take effect now.
                let sid = agent.session_id().await;
                if let Ok(new_agent) =
                    build_agent(SessionChoice::Existing(sid), &current_agent, None).await
                {
                    agent = new_agent;
                }
                print_info_lines(&lines)?;
                if from_splash {
                    is_splash = true;
                }
                continue;
            }
            "/plan" => {
                let target = if agent.interaction_mode() == io_runtime::InteractionMode::Plan {
                    io_runtime::InteractionMode::Default
                } else {
                    io_runtime::InteractionMode::Plan
                };
                status::set_mode(&agent, target);
                let msg = match target {
                    io_runtime::InteractionMode::Plan => {
                        "Plan mode on — read-only until you approve a plan (Shift+Tab to cycle)."
                    }
                    _ => "Plan mode off.",
                };
                let _ = draw_prompt_bar(
                    msg,
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
                continue;
            }
            _ if input == "/think" || input.starts_with("/think ") => {
                let msg = match status::apply_think_command(&agent, &input["/think".len()..]) {
                    Ok(msg) => msg,
                    Err(e) => format!("error: {e}"),
                };
                let _ = draw_prompt_bar(
                    &msg,
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

        let (mut prompt, images) = resolve_at_mentions_with_images(&input);
        if input == "/init" {
            prompt = io_runtime::instructions::INIT_PROMPT.to_string();
        } else if let Some(expanded) = crate::git_prompts::expand(&input) {
            prompt = expanded;
        }
        let image_count = images.len();
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
        if image_count > 0 {
            let note = format!(
                "  + {image_count} image{} attached",
                if image_count == 1 { "" } else { "s" }
            );
            print!("{note}\r\n\r\n");
            let _ = std::io::stdout().flush();
            push_line(&line_buf, note);
        }
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
        // Ctrl+B moves a running `bash` command to the background; anything
        // typed while the model works is queued and sent when it finishes.
        let background = agent.background_handle();
        let bar_agent = current_agent.name;
        let bar_provider = agent.provider_id;
        let bar_model = agent.model_id.clone();
        let bar_tokens = last_input_tokens;
        let bar_window = agent.context_window();
        let key_listener = tokio::task::spawn_blocking(move || {
            use crossterm::event;
            let mut typed = String::new();
            let mut queue: Vec<String> = Vec::new();
            // Show the typed text in the prompt bar without disturbing the
            // output being streamed above it: hold the terminal for the whole
            // redraw and put the cursor back afterwards.
            let show = |typed: &str, queued: usize| {
                let text = if queued > 0 {
                    format!("(+{queued} queued) {typed}")
                } else {
                    typed.to_string()
                };
                let _guard = std::io::stdout().lock();
                let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::SavePosition);
                let _ = draw_prompt_bar(
                    &text,
                    bar_agent,
                    bar_provider,
                    &bar_model,
                    bar_tokens,
                    bar_window,
                    &theme,
                );
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::cursor::RestorePosition,
                    crossterm::cursor::Hide
                );
            };
            loop {
                if streaming_done2.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }

                // A permission request pauses the stream until answered. Open
                // the arrow picker modal as soon as one is pending — no key
                // press is required to trigger it.
                let pending = pending_for_listener.lock().unwrap().take();
                if let Some(pending) = pending {
                    let dialog = permission_dialog(&pending);
                    let (reply, echo) = ask_permission(&dialog);
                    print!("  → {echo}\r\n");
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
                        Ok(crossterm::event::Event::Key(k))
                            if k.kind != crossterm::event::KeyEventKind::Release =>
                        {
                            use crossterm::event::{KeyCode, KeyModifiers};
                            let plain = !k
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                            match k.code {
                                KeyCode::Char('b')
                                    if k.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    if let Some(flag) = &background {
                                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                                KeyCode::Char(c) if plain => {
                                    typed.push(c);
                                    show(&typed, queue.len());
                                }
                                KeyCode::Backspace => {
                                    typed.pop();
                                    show(&typed, queue.len());
                                }
                                KeyCode::Enter if !typed.trim().is_empty() => {
                                    queue.push(std::mem::take(&mut typed));
                                    show(&typed, queue.len());
                                }
                                _ => {}
                            }
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
            (queue, typed)
        });

        tokio::select! {
            result = agent.run_turn_streaming_with_images(&prompt, images, token_tx) => {
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
        if let Ok((queued, unfinished)) = key_listener.await {
            // Cancelling (Esc) drops what was queued; otherwise it goes next.
            if !cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                pending_inputs.extend(queued);
            }
            seed = unfinished;
        }

        // Approving a plan switches modes inside the agent; follow it.
        status::sync_from_agent(&agent);

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

    agent.end_session("exit").await;
    exit_tui()?;
    Ok(())
}

/// The `/status` report.
async fn status_report(
    agent: &io_runtime::Agent,
    current_agent: &io_agents::AgentConfig,
    root: &std::path::Path,
) -> Vec<String> {
    let settings = io_runtime::settings::Settings::load(root);
    let config = io_runtime::config::Config::load().ok();
    let cwd = std::env::current_dir().unwrap_or_else(|_| root.to_path_buf());
    let home = dirs::home_dir();
    let styles = io_runtime::output_style::discover(root, home.as_deref());
    let style = settings
        .output_style
        .clone()
        .filter(|n| styles.iter().any(|s| &s.name == n))
        .unwrap_or_else(|| "default".to_string());
    let session = agent.session_snapshot().await;
    let mcp = mcp_cmd::manager();
    let thinking = agent.thinking();
    let sandbox = if settings.sandbox.enabled {
        if settings.sandbox.network {
            "on (network allowed)"
        } else {
            "on (no network)"
        }
    } else {
        "off"
    };
    let count = |list| settings.rules_in(list).len();
    info_cmd::status_lines(&info_cmd::Status {
        version: env!("CARGO_PKG_VERSION"),
        cwd: &cwd.display().to_string(),
        project_root: &root.display().to_string(),
        session_id: &session.id.to_string()[..8],
        session_name: session.metadata.name.as_deref(),
        turns: session.turns.len(),
        provider: agent.provider_id,
        model: &agent.model_id,
        agent: current_agent.id,
        mode: agent.interaction_mode().label(),
        permissions: config
            .as_ref()
            .map(|c| c.permissions.default.as_str())
            .unwrap_or("?"),
        thinking: &if thinking.enabled {
            format!("on ({} tokens)", thinking.budget_tokens)
        } else {
            "off".to_string()
        },
        output_style: &style,
        settings_files: settings
            .files
            .iter()
            .map(|(scope, _)| scope.label().to_string())
            .collect(),
        rule_counts: (
            count(io_runtime::settings::RuleList::Allow),
            count(io_runtime::settings::RuleList::Ask),
            count(io_runtime::settings::RuleList::Deny),
        ),
        mcp: (mcp.states().len(), mcp.tool_count()),
        sandbox,
        memory_files: io_runtime::instructions::discover(root, &cwd, home.as_deref())
            .into_iter()
            .map(|f| f.label)
            .collect(),
    })
}

/// Show `lines` as a block just above the prompt bar (used by /help, /mcp).
fn print_info_lines<S: AsRef<str>>(lines: &[S]) -> anyhow::Result<()> {
    use crossterm::{cursor, execute, style::Print, terminal};
    clear_rows_above(0)?;
    let (w, h) = terminal::size()?;
    let start_row = h.saturating_sub(PROMPT_BAR_HEIGHT + 1 + lines.len() as u16);
    for (i, line) in lines.iter().enumerate() {
        let line: String = line
            .as_ref()
            .chars()
            .take(w.saturating_sub(1) as usize)
            .collect();
        execute!(
            std::io::stdout(),
            cursor::MoveTo(0, start_row + i as u16),
            terminal::Clear(terminal::ClearType::CurrentLine),
            Print(line),
        )?;
    }
    std::io::stdout().flush()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(name: &str, detail: &str) -> PendingPerm {
        let (respond, _rx) = tokio::sync::oneshot::channel();
        PendingPerm {
            name: name.to_string(),
            detail: detail.to_string(),
            respond,
        }
    }

    #[test]
    fn ordinary_permission_dialogs_default_to_no() {
        let d = permission_dialog(&pending("bash", "rm -r build"));
        assert_eq!(d.title, "allow \"bash\" (rm -r build)?");
        assert_eq!(d.choices.len(), 3);
        assert_eq!(d.choices[d.initial].reply, PermissionReply::Deny);
        assert_eq!(d.choices[0].reply, PermissionReply::AllowOnce);
        assert_eq!(d.choices[1].reply, PermissionReply::AllowSession);
        assert_eq!(
            permission_dialog(&pending("bash", "")).title,
            "allow \"bash\"?"
        );
    }

    #[test]
    fn the_plan_dialog_maps_onto_the_replies_the_agent_expects() {
        let d = permission_dialog(&pending("exit_plan_mode", ""));
        // "AllowSession" accepts edits, "AllowOnce" leaves edit prompts on.
        let replies: Vec<_> = d.choices.iter().map(|c| c.reply).collect();
        assert_eq!(
            replies,
            [
                PermissionReply::AllowSession,
                PermissionReply::AllowOnce,
                PermissionReply::Deny
            ]
        );
        // Dismissing never approves a plan.
        assert_eq!(d.choices[d.initial].reply, PermissionReply::Deny);
        assert_eq!(d.choices.last().unwrap().reply, PermissionReply::Deny);
    }
}
