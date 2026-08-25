use clap::{Parser, Subcommand};
use io_runtime::types::SessionId;
use std::str::FromStr;

mod agent;
mod config_cmd;
mod connect;
mod cost;
mod input;
mod login;
mod model;
mod stream;
mod tui;

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
    List,
    Show { id: String },
    Delete { id: String },
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

    match cli.command {
        Some(Commands::Session { action }) => handle_session(action).await?,
        Some(Commands::Config { action }) => config_cmd::handle_config(action)?,
        Some(Commands::Init) => config_cmd::handle_init()?,
        Some(Commands::Login { provider }) => login::run(&provider).await?,
        Some(Commands::Model {
            action: ModelAction::Refresh,
        }) => println!("{}", model::refresh_context_window().await?),
        None => {
            if let Some(prompt) = cli.prompt {
                tui::run_single_shot(&prompt, cli.model.as_deref()).await?;
            } else {
                tui::run_interactive(cli.new, cli.r#continue, cli.model.as_deref()).await?;
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
            } else {
                for s in sessions {
                    println!(
                        "{}  (created: {}, updated: {})",
                        s.id, s.created_at, s.updated_at
                    );
                }
            }
        }
        SessionAction::Show { id } => {
            let session_id =
                SessionId::from_str(&id).map_err(|_| anyhow::anyhow!("invalid session id"))?;
            let session = store.load_session(session_id)?;
            println!("Session: {}", session.id);
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
            let session_id =
                SessionId::from_str(&id).map_err(|_| anyhow::anyhow!("invalid session id"))?;
            store.delete_session(session_id)?;
            println!("Session {id} deleted.");
        }
    }
    Ok(())
}
