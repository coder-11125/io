use clap::{Parser, Subcommand};
use io_runtime::types::SessionId;

mod agent;
mod clipboard;
mod config_cmd;
mod connect;
mod cost;
mod doctor;
mod git_prompts;
mod headless;
mod history;
mod info_cmd;
mod input;
mod login;
mod mcp_cmd;
mod memory_cmd;
mod model;
mod permissions;
mod run_opts;
mod session_cmd;
mod status;
mod statusline;
mod stream;
mod style_cmd;
mod tui;
mod update;
mod vim;
mod worktree;

#[derive(Parser)]
#[command(name = "io", version, about = "AI coding agent for the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Inline prompt (run in single-shot mode)
    prompt: Option<String>,

    /// Start a fresh session
    #[arg(long)]
    new: bool,

    /// Resume the last session
    #[arg(long)]
    r#continue: bool,

    /// Override the LLM provider/model
    #[arg(long)]
    model: Option<String>,

    /// Headless mode: run the prompt (argument and/or piped stdin), print the
    /// result and exit
    #[arg(short = 'p', long = "print")]
    print: bool,

    /// Headless output: text, json, or stream-json
    #[arg(
        long = "output-format",
        value_enum,
        default_value = "text",
        requires = "print"
    )]
    output_format: headless::OutputFormat,

    /// Most model calls per turn (default 20)
    #[arg(long = "max-turns", value_name = "N")]
    max_turns: Option<usize>,

    /// Rules to pre-approve, e.g. "Bash(git log:*) Read" (commas or spaces)
    #[arg(long = "allowedTools", alias = "allowed-tools", value_name = "RULES")]
    allowed_tools: Option<String>,

    /// Rules to refuse outright
    #[arg(
        long = "disallowedTools",
        alias = "disallowed-tools",
        value_name = "RULES"
    )]
    disallowed_tools: Option<String>,

    /// Extra text for the end of the system prompt
    #[arg(long = "append-system-prompt", value_name = "TEXT")]
    append_system_prompt: Option<String>,

    /// default, acceptEdits, plan or bypassPermissions
    #[arg(long = "permission-mode", value_name = "MODE")]
    permission_mode: Option<String>,

    /// Start in a new git worktree (on its own branch) under .io/worktrees
    #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    worktree: Option<String>,

    /// Resume a session by id (see `io session list`)
    #[arg(short = 'r', long = "resume", value_name = "ID")]
    resume: Option<String>,

    /// Let file tools reach this directory too (repeatable)
    #[arg(long = "add-dir", value_name = "DIR")]
    add_dir: Vec<std::path::PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Session management
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Show or modify configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Initialize io in the current project
    Init,
    /// Sign in with OAuth for a provider that offers subscription access
    /// (openai = ChatGPT, anthropic = Claude) instead of an API key
    Login { provider: String },
    /// Check the installation and environment
    Doctor,
    /// Sign out of an OAuth login (openai, anthropic; both when omitted)
    Logout { provider: Option<String> },
    /// Check for a newer release
    Update {
        /// Install it with cargo
        #[arg(long)]
        install: bool,
    },
    /// Print a shell completion script
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Manage MCP (Model Context Protocol) servers
    Mcp {
        #[command(subcommand)]
        action: mcp_cmd::McpAction,
    },
    /// Model management
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
}

#[derive(Subcommand)]
enum ModelAction {
    /// Fetch the active provider's real context window, pricing, and
    /// tool-call support from the models.dev catalog (Ollama uses its own
    /// local /api/show lookup) and save them as config overrides.
    Refresh,
}

#[derive(Subcommand)]
enum SessionAction {
    /// List recent sessions (id, last used, title, turns, model)
    List,
    /// Print a session's conversation
    Show {
        id: String,
    },
    Delete {
        id: String,
    },
    /// Give a session a name
    Rename {
        id: String,
        name: String,
    },
    /// Write a session as Markdown (to FILE, or a generated name)
    Export {
        id: String,
        file: Option<String>,
    },
    /// Copy a session under a new id and print it
    Fork {
        id: String,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    Show,
    Set { key: String, value: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .init();

    let cli = Cli::parse();
    if let Some(name) = &cli.worktree {
        let repo = worktree::repo_root()?;
        let path = worktree::create(&repo, Some(name.as_str()).filter(|n| !n.is_empty()))?;
        std::env::set_current_dir(&path)?;
        eprintln!("Working in {}", path.display());
    }
    for dir in &cli.add_dir {
        permissions::add_dir(dir)?;
    }
    let mut opts = run_opts::RunOpts {
        allowed_tools: cli
            .allowed_tools
            .as_deref()
            .map(run_opts::split_tool_list)
            .unwrap_or_default(),
        disallowed_tools: cli
            .disallowed_tools
            .as_deref()
            .map(run_opts::split_tool_list)
            .unwrap_or_default(),
        append_system_prompt: cli.append_system_prompt.clone(),
        bypass_permissions: false,
        max_turns: cli.max_turns.filter(|n| *n > 0),
        explicit_mode: cli.permission_mode.is_some(),
    };
    if let Some(word) = &cli.permission_mode {
        match run_opts::parse_permission_mode(word).map_err(anyhow::Error::msg)? {
            run_opts::ModeFlag::Default => {
                status::set_initial_mode(io_runtime::InteractionMode::Default)
            }
            run_opts::ModeFlag::AcceptEdits => {
                status::set_initial_mode(io_runtime::InteractionMode::AcceptEdits)
            }
            run_opts::ModeFlag::Plan => status::set_initial_mode(io_runtime::InteractionMode::Plan),
            run_opts::ModeFlag::Bypass => opts.bypass_permissions = true,
        }
    }
    run_opts::set(opts);

    match cli.command {
        Some(Commands::Session { action }) => handle_session(action).await?,
        Some(Commands::Config { action }) => config_cmd::handle_config(action)?,
        Some(Commands::Init) => config_cmd::handle_init()?,
        Some(Commands::Login { provider }) => println!("{}", login::run(&provider).await?),
        Some(Commands::Doctor) => {
            let checks = doctor::run_checks(&tui_root());
            for line in doctor::render(&checks) {
                println!("{line}");
            }
            if doctor::failures(&checks) > 0 {
                std::process::exit(1);
            }
        }
        Some(Commands::Logout { provider }) => println!("{}", login::logout(provider.as_deref())?),
        Some(Commands::Update { install }) => update::run(install).await?,
        Some(Commands::Completions { shell }) => {
            use clap::CommandFactory;
            clap_complete::generate(shell, &mut Cli::command(), "io", &mut std::io::stdout());
        }
        Some(Commands::Mcp { action }) => mcp_cmd::handle(action).await?,
        Some(Commands::Model {
            action: ModelAction::Refresh,
        }) => println!("{}", model::refresh_context_window().await?),
        None if cli.print => {
            let code = headless::run(headless::Args {
                prompt: cli.prompt,
                format: cli.output_format,
                model: cli.model,
                continue_session: cli.r#continue,
                resume: cli.resume,
            })
            .await?;
            std::process::exit(code);
        }
        None => {
            if let Some(prompt) = cli.prompt {
                tui::run_single_shot(&prompt, cli.model.as_deref()).await?;
            } else {
                tui::run_interactive(
                    cli.new,
                    cli.r#continue,
                    cli.resume.as_deref(),
                    cli.model.as_deref(),
                )
                .await?;
            }
        }
    }

    Ok(())
}

async fn handle_session(action: SessionAction) -> anyhow::Result<()> {
    let store = io_runtime::memory::SessionStore::new()?;
    match action {
        SessionAction::List => {
            let sessions = store.list_sessions()?;
            if sessions.is_empty() {
                println!("No sessions found.");
            }
            for s in sessions {
                println!("{}", session_cmd::list_line(&s));
            }
        }
        SessionAction::Show { id } => {
            let session = store.load_session(session_cmd::resolve_id(&store, &id)?)?;
            println!("Session: {}", session.id);
            if let Some(name) = &session.metadata.name {
                println!("Name: {name}");
            }
            println!("Created: {}", session.created_at);
            println!("Turns: {}", session.turns.len());
            for (i, turn) in session.turns.iter().enumerate() {
                println!("\n--- Turn {} ---", i + 1);
                println!("User: {}", turn.user_message);
                if let Some(ref reply) = turn.assistant_message {
                    println!("Assistant: {}", reply);
                }
            }
        }
        SessionAction::Delete { id } => {
            let session_id = session_cmd::resolve_id(&store, &id)?;
            store.delete_session(session_id)?;
            println!("Session {session_id} deleted.");
        }
        SessionAction::Rename { id, name } => {
            let mut session = store.load_session(session_cmd::resolve_id(&store, &id)?)?;
            session.metadata.name = Some(name.trim().to_string()).filter(|n| !n.is_empty());
            store.save_session(&session).await?;
            println!("Session {} renamed.", session_cmd::short_id(session.id));
        }
        SessionAction::Export { id, file } => {
            let session = store.load_session(session_cmd::resolve_id(&store, &id)?)?;
            let cwd = std::env::current_dir()?;
            let path = session_cmd::export_path(file.as_deref().unwrap_or(""), &session, &cwd);
            std::fs::write(&path, io_runtime::export::to_markdown(&session))?;
            println!("Wrote {}", path.display());
        }
        SessionAction::Fork { id } => {
            let mut session = store.load_session(session_cmd::resolve_id(&store, &id)?)?;
            session.id = SessionId::new();
            session.updated_at = chrono::Utc::now();
            session.metadata.name = session.metadata.name.map(|n| format!("{n} (fork)"));
            store.save_session(&session).await?;
            println!("{}", session.id);
        }
    }
    Ok(())
}

fn tui_root() -> std::path::PathBuf {
    tui::detect_project_root()
}
