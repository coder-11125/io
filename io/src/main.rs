use std::io::Write;
use std::str::FromStr;
use clap::{Parser, Subcommand};
use io_runtime::types::SessionId;

mod agent;
mod connect;
mod picker;
mod model;
mod readline;

fn print_banner() {
    println!("IO");
}

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
        Some(Commands::Config { action }) => handle_config(action)?,
        Some(Commands::Init) => handle_init()?,
        None => {
            if let Some(prompt) = cli.prompt {
                run_single_shot(&prompt).await?;
            } else {
                run_interactive(cli.new, cli.r#continue, cli.model.as_deref()).await?;
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
                    println!("{}  (created: {}, updated: {})", s.id, s.created_at, s.updated_at);
                }
            }
        }
        SessionAction::Show { id } => {
            let session_id = SessionId::from_str(&id)
                .map_err(|_| anyhow::anyhow!("invalid session id"))?;
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
            let session_id = SessionId::from_str(&id)
                .map_err(|_| anyhow::anyhow!("invalid session id"))?;
            store.delete_session(session_id)?;
            println!("Session {id} deleted.");
        }
    }
    Ok(())
}

fn handle_config(action: ConfigAction) -> anyhow::Result<()> {
    match action {
        ConfigAction::Show => {
            let config = io_runtime::config::Config::load()?;
            println!("{}", toml::to_string_pretty(&config).unwrap_or_default());
        }
        ConfigAction::Set { key, value } => {
            let mut config = io_runtime::config::Config::load()?;
            match key.as_str() {
                "provider.default" => config.provider.default = value.clone(),
                "session.auto_compact" => config.session.auto_compact = value.parse().unwrap_or(true),
                "session.memory_enabled" => config.session.memory_enabled = value.parse().unwrap_or(true),
                "permissions.default" => config.permissions.default = value.clone(),
                _ => anyhow::bail!("unknown config key: {key}"),
            }
            config.save()?;
            println!("Set {key} = {value}");
        }
    }
    Ok(())
}

fn handle_init() -> anyhow::Result<()> {
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

fn active_model_id(config: &io_runtime::config::Config) -> String {
    let p = &config.provider;
    let model = match p.default.as_str() {
        "openai"       => p.openai.as_ref().map(|c| c.model.as_str()),
        "anthropic"    => p.anthropic.as_ref().map(|c| c.model.as_str()),
        "gemini"       => p.gemini.as_ref().map(|c| c.model.as_str()),
        "groq"         => p.groq.as_ref().map(|c| c.model.as_str()),
        "ollama"       => p.ollama.as_ref().map(|c| c.model.as_str()),
        "azure"        => p.azure.as_ref().map(|c| c.deployment.as_str()),
        "bedrock"      => p.bedrock.as_ref().map(|c| c.model.as_str()),
        "mistral"      => p.mistral.as_ref().map(|c| c.model.as_str()),
        "deepseek"     => p.deepseek.as_ref().map(|c| c.model.as_str()),
        "openrouter"   => p.openrouter.as_ref().map(|c| c.model.as_str()),
        "xai"          => p.xai.as_ref().map(|c| c.model.as_str()),
        "opencode_go"  => p.opencode_go.as_ref().map(|c| c.model.as_str()),
        "opencode_zen" => p.opencode_zen.as_ref().map(|c| c.model.as_str()),
        _              => None,
    };
    model.unwrap_or(p.default.as_str()).to_string()
}

async fn build_agent(new_session: bool, continue_session: bool, system_prompt: String) -> anyhow::Result<io_runtime::Agent> {
    let config = io_runtime::config::Config::load()?;
    let keys = io_runtime::config::KeyStore::load();
    let model_id = active_model_id(&config);
    let provider = std::sync::Arc::new(io_runtime::provider::create_provider(&config, &keys)?);
    let memory = io_runtime::memory::SessionStore::new()?;
    let permissions = io_runtime::sandbox::PermissionChecker::from(&config.permissions);

    let session_id = if new_session || !continue_session {
        None
    } else {
        let sessions = memory.list_sessions()?;
        sessions.first().map(|s| s.id)
    };

    let mut tools = io_runtime::tools::default_registry();
    let spawn_tool = io_runtime::SpawnAgentTool::new(
        provider.clone(),
        model_id.clone(),
        config.session.max_tokens,
    );
    tools.register(Box::new(spawn_tool));

    Ok(io_runtime::Agent::new(provider, tools, memory, permissions, system_prompt, session_id, model_id, config.session.max_tokens, config.session.auto_compact))
}

async fn show_cost_summary(agent: &io_runtime::Agent) -> anyhow::Result<()> {
    let store = io_runtime::memory::SessionStore::new()?;
    let session_id = agent.session_id().await;
    let session = store.load_session(session_id)?;

    let provider = &session.metadata.provider;
    let model = &session.metadata.model;
    let pricing_category = io_runtime::get_provider_pricing_category(provider, model);

    let mut total_input_tokens: u32 = 0;
    let mut total_output_tokens: u32 = 0;
    let mut total_cost: f64 = 0.0;
    let mut priced_turns: usize = 0;
    let mut missing_cost_turns: usize = 0;

    println!("\n--- Cost Summary ---");
    println!("Session:  {}", &session_id.to_string()[..8]);
    println!("Provider: {provider}");
    println!("Model:    {model}");
    println!("Turns:    {}", session.turns.len());
    println!();

    if session.turns.is_empty() {
        println!("No turns in this session yet.");
        println!();
        return Ok(());
    }

    let no_cost_label = match pricing_category {
        io_runtime::ProviderPricingCategory::Free => "free / self-hosted",
        io_runtime::ProviderPricingCategory::SubscriptionBased => "subscription billing",
        io_runtime::ProviderPricingCategory::PassThrough => "proxy — cost via backend",
        io_runtime::ProviderPricingCategory::ModelNotInTable => "model not in pricing table",
        io_runtime::ProviderPricingCategory::ProviderNotInTable => "provider not in pricing table",
        io_runtime::ProviderPricingCategory::Priced => "cost unavailable",
    };

    for (i, turn) in session.turns.iter().enumerate() {
        match &turn.usage {
            None => {
                println!("Turn {:>3}: no token data recorded", i + 1);
            }
            Some(usage) => {
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
                if let Some(cost) = usage.cost {
                    total_cost += cost;
                    priced_turns += 1;
                    println!(
                        "Turn {:>3}: {:>7} in + {:>7} out = ${:.6}",
                        i + 1,
                        usage.input_tokens,
                        usage.output_tokens,
                        cost
                    );
                } else {
                    if pricing_category == io_runtime::ProviderPricingCategory::Priced
                        || pricing_category == io_runtime::ProviderPricingCategory::ModelNotInTable
                    {
                        missing_cost_turns += 1;
                    }
                    println!(
                        "Turn {:>3}: {:>7} in + {:>7} out  ({})",
                        i + 1,
                        usage.input_tokens,
                        usage.output_tokens,
                        no_cost_label
                    );
                }
            }
        }
    }

    println!();
    println!("--- Totals ---");
    println!("Input tokens:  {total_input_tokens}");
    println!("Output tokens: {total_output_tokens}");

    match pricing_category {
        io_runtime::ProviderPricingCategory::Free => {
            println!("Total cost:    $0.00 (self-hosted / no API charges)");
        }
        io_runtime::ProviderPricingCategory::SubscriptionBased => {
            println!("Total cost:    n/a (subscription-billed provider)");
        }
        io_runtime::ProviderPricingCategory::PassThrough => {
            println!("Total cost:    n/a (proxy provider — check your backend for charges)");
        }
        _ => {
            if priced_turns > 0 && missing_cost_turns == 0 {
                println!("Total cost:    ${total_cost:.6}");
            } else if priced_turns > 0 {
                println!(
                    "Total cost:    ${total_cost:.6} (partial — {missing_cost_turns} turn(s) missing pricing)"
                );
            } else {
                println!("Total cost:    n/a ({no_cost_label})");
            }
        }
    }

    println!();
    Ok(())
}

/// Maximum file size included inline via `@path` mention.
const MAX_AT_FILE_BYTES: usize = 100 * 1024;

/// Scan `input` for `@path` tokens and append their resolved content as
/// `<file>` blocks at the end of the message. The original text (including
/// the `@` mentions) is preserved so the model sees them in context.
fn resolve_at_mentions(input: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '@' && (i == 0 || chars[i - 1].is_whitespace()) {
            let start = i + 1;
            let end = chars[start..]
                .iter()
                .position(|c| c.is_whitespace())
                .map(|p| start + p)
                .unwrap_or(chars.len());
            let path_str: String = chars[start..end].iter().collect();
            if !path_str.is_empty() {
                if let Some(block) = read_at_path(&path_str) {
                    blocks.push(block);
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }

    if blocks.is_empty() {
        return input.to_string();
    }
    format!("{}\n\n{}", input, blocks.join("\n\n"))
}

fn read_at_path(path: &str) -> Option<String> {
    let clean = path.trim_end_matches('/');
    let p = std::path::Path::new(clean);

    if p.is_dir() {
        let entries = std::fs::read_dir(p).ok()?;
        let mut lines: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().into_string().unwrap_or_default();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir { format!("{name}/") } else { name }
            })
            .collect();
        lines.sort_by(|a, b| b.ends_with('/').cmp(&a.ends_with('/')).then(a.cmp(b)));
        Some(format!("<file path=\"{path}\">\n{}\n</file>", lines.join("\n")))
    } else if p.is_file() {
        let raw = std::fs::read(p).ok()?;
        if raw.len() > MAX_AT_FILE_BYTES {
            return Some(format!(
                "<file path=\"{path}\">\n[file too large to inline ({} bytes)]\n</file>",
                raw.len()
            ));
        }
        let text = String::from_utf8_lossy(&raw);
        Some(format!("<file path=\"{path}\">\n{text}\n</file>"))
    } else {
        None
    }
}

async fn run_single_shot(prompt: &str) -> anyhow::Result<()> {
    let system_prompt = io_agents::builtin::by_id("build")
        .expect("build agent must exist")
        .system_prompt;
    let agent = build_agent(false, false, system_prompt).await?;
    let response = agent.run_turn(&resolve_at_mentions(prompt)).await?;
    println!("{response}");
    Ok(())
}

async fn run_interactive(
    new_session: bool,
    continue_session: bool,
    _model: Option<&str>,
) -> anyhow::Result<()> {
    print_banner();

    let mut current_agent = io_agents::builtin::by_id("build")
        .expect("build agent must exist");
    let mut agent = build_agent(new_session, continue_session, current_agent.system_prompt.clone()).await?;

    let sid = agent.session_id().await;
    let short_id = &sid.to_string()[..8];
    println!("session: {short_id}  |  /help  /agent  /connect  /exit");
    println!();

    let mut last_input_tokens: u32 = 0;

    loop {
        print_prompt(&agent, last_input_tokens, current_agent.id);

        let full_agents = io_agents::builtin::full_agents();
        let tab_current = full_agents.iter().position(|a| a.id == current_agent.id).unwrap_or(0);
        let tab_statuses: Vec<String> = full_agents
            .iter()
            .map(|a| format!("  {} · {} · {}", a.id, agent.provider_id, agent.model_id))
            .collect();
        let ctx = readline::ReadLineCtx { tab_statuses, tab_current };

        let output = match tokio::task::spawn_blocking(move || readline::read_line(ctx)).await?? {
            Some(out) => out,
            None => break,
        };

        // Sync agent if Tab cycling changed the selection — rebuild once on Enter, not per keypress.
        if output.agent_idx != tab_current {
            if let Some(picked) = full_agents.into_iter().nth(output.agent_idx) {
                current_agent = picked;
                match build_agent(false, false, current_agent.system_prompt.clone()).await {
                    Ok(new_agent) => agent = new_agent,
                    Err(e) => eprintln!("error switching agent: {e}"),
                }
            }
        }

        let input = output.text.trim().to_string();

        if input.is_empty() { continue; }

        match input.as_str() {
            "/exit" | "/quit" | "/q" => break,
            "/help" => {
                println!("Commands:");
                println!("  /exit, /quit   Exit");
                println!("  /help          Show this help");
                println!("  /agent         Switch agent mode");
                println!("  /connect       Set up a provider interactively");
                println!("  /model         Switch between configured providers");
                println!("  /cost          Show API cost summary for current session");
                println!("  /compact       Summarize and compress conversation history");
                println!("  !<cmd>         Run a shell command");
                println!();
                continue;
            }
            "/cost" => {
                if let Err(e) = show_cost_summary(&agent).await {
                    eprintln!("error showing cost summary: {e}");
                }
                continue;
            }
            "/compact" => {
                print!("  Compacting conversation history...");
                std::io::stdout().flush().ok();
                match agent.compact().await {
                    Ok(result) if result.turns_compacted == 0 => {
                        println!("\r  Nothing to compact — session has no turns.          ");
                    }
                    Ok(result) => {
                        println!(
                            "\r  Compacted {} turn{} into a summary.                    ",
                            result.turns_compacted,
                            if result.turns_compacted == 1 { "" } else { "s" }
                        );
                    }
                    Err(e) => eprintln!("\r  error: {e}"),
                }
                continue;
            }
            "/agent" => {
                match agent::run(current_agent.id) {
                    Ok(new_config) => {
                        println!("  Agent: {}", new_config.name);
                        println!();
                        current_agent = new_config;
                        match build_agent(false, false, current_agent.system_prompt.clone()).await {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => eprintln!("error reloading agent: {e}"),
                        }
                    }
                    Err(e) if e.to_string() != "Cancelled" && e.to_string() != "Interrupted" => {
                        eprintln!("error: {e}");
                    }
                    _ => {}
                }
                continue;
            }
            "/connect" => {
                match connect::run().await {
                    Ok(()) => {
                        match build_agent(false, false, current_agent.system_prompt.clone()).await {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => eprintln!("error reloading provider: {e}"),
                        }
                    }
                    Err(e) => eprintln!("error: {e}"),
                }
                continue;
            }
            "/model" => {
                match model::run().await {
                    Ok(()) => {
                        match build_agent(false, false, current_agent.system_prompt.clone()).await {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => eprintln!("error reloading provider: {e}"),
                        }
                    }
                    Err(e) if e.to_string() != "Cancelled" && e.to_string() != "Interrupted" => {
                        eprintln!("error: {e}");
                    }
                    _ => {}
                }
                continue;
            }
            _ if input.starts_with('!') => {
                let cmd = input[1..].trim();
                let output = run_bash(cmd).await;
                println!("{output}");
                continue;
            }
            _ => {}
        }

        let prompt = resolve_at_mentions(&input);
        let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        agent.set_cancel(cancel_flag.clone());

        let (token_tx, token_rx) = tokio::sync::mpsc::channel::<io_runtime::AgentEvent>(64);
        let print_task = tokio::spawn(blink_and_print(token_rx));

        // Background thread: poll for Esc and signal cancellation.
        let cancel_for_listener = cancel_flag.clone();
        let streaming_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let streaming_done2 = streaming_done.clone();
        let (esc_tx, esc_rx) = tokio::sync::oneshot::channel::<()>();
        let key_listener = tokio::task::spawn_blocking(move || {
            use crossterm::{event, terminal};
            let _ = terminal::enable_raw_mode();
            loop {
                if streaming_done2.load(std::sync::atomic::Ordering::Relaxed) { break; }
                if event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
                    if let Ok(event::Event::Key(k)) = event::read() {
                        if k.code == event::KeyCode::Esc {
                            cancel_for_listener.store(true, std::sync::atomic::Ordering::Relaxed);
                            let _ = esc_tx.send(());
                            break;
                        }
                    }
                }
            }
            let _ = terminal::disable_raw_mode();
        });

        // Dropping the streaming future drops token_tx, closing the channel so
        // print_task drains and exits naturally in both the normal and Esc cases.
        tokio::select! {
            result = agent.run_turn_streaming(&prompt, token_tx) => {
                if let Err(e) = result {
                    if e.to_string() != "cancelled" {
                        eprintln!("error: {e}");
                    }
                }
            }
            _ = esc_rx => {}
        }
        streaming_done.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = key_listener.await;

        let (agent_thoughts, turn_input_tokens) = print_task.await.ok().unwrap_or((None, 0));
        if turn_input_tokens > 0 { last_input_tokens = turn_input_tokens; }
        println!();
        if let Some(ref t) = agent_thoughts {
            render_thoughts(t);
        }
    }

    Ok(())
}

/// Walk a byte offset back to the nearest valid UTF-8 char boundary.
fn char_floor(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Streaming parser that splits `<think>...</think>` blocks out of text deltas.
/// Handles tags split across multiple deltas by buffering a small lookahead.
struct ThinkParser {
    pending: String,
    in_think: bool,
}

impl ThinkParser {
    fn new() -> Self {
        Self { pending: String::new(), in_think: false }
    }

    /// Feed a streaming delta. Returns `(display_text, thinking_text)`.
    fn feed(&mut self, delta: &str) -> (String, String) {
        self.pending.push_str(delta);
        let mut display = String::new();
        let mut thoughts = String::new();

        loop {
            if self.in_think {
                if let Some(end) = self.pending.find("</think>") {
                    thoughts.push_str(&self.pending[..end]);
                    self.pending = self.pending[end + "</think>".len()..].to_string();
                    self.in_think = false;
                } else {
                    // Keep enough bytes to detect a tag split across chunks.
                    let raw = self.pending.len().saturating_sub("</think>".len());
                    let safe = char_floor(&self.pending, raw);
                    thoughts.push_str(&self.pending[..safe]);
                    self.pending = self.pending[safe..].to_string();
                    break;
                }
            } else if let Some(start) = self.pending.find("<think>") {
                display.push_str(&self.pending[..start]);
                self.pending = self.pending[start + "<think>".len()..].to_string();
                self.in_think = true;
            } else {
                let raw = self.pending.len().saturating_sub("<think>".len());
                let safe = char_floor(&self.pending, raw);
                display.push_str(&self.pending[..safe]);
                self.pending = self.pending[safe..].to_string();
                break;
            }
        }

        (display, thoughts)
    }

    /// Flush any remaining buffered bytes at end-of-stream.
    fn flush(&mut self) -> (String, String) {
        let text = std::mem::take(&mut self.pending);
        if self.in_think { (String::new(), text) } else { (text, String::new()) }
    }
}

fn process_ev(
    ev: io_runtime::AgentEvent,
    text: &mut String,
    think: &mut String,
    parser: &mut ThinkParser,
) {
    use io_runtime::AgentEvent;
    match ev {
        AgentEvent::Text(delta) => {
            let (display, thought) = parser.feed(&delta);
            text.push_str(&display);
            think.push_str(&thought);
        }
        AgentEvent::Thinking(delta) => {
            think.push_str(&delta);
        }
        AgentEvent::ToolStart { name, input } => render_tool_start(&name, &input),
        AgentEvent::ToolDone { name, output, success } => render_tool_done(&name, &output, success),
        AgentEvent::Usage { .. } => {} // captured at call site
        AgentEvent::AutoCompact { turns_compacted } => {
            print!("\r\n  [auto-compact] Compacted {turns_compacted} turn{} into a summary.\r\n",
                if turns_compacted == 1 { "" } else { "s" });
            let _ = std::io::stdout().flush();
        }
    }
}

async fn blink_and_print(mut rx: tokio::sync::mpsc::Receiver<io_runtime::AgentEvent>) -> (Option<String>, u32) {
    let on = "  +  +  +  +  +";
    let mut phase = false;

    let first = loop {
        tokio::select! {
            ev = rx.recv() => break ev,
            _ = tokio::time::sleep(std::time::Duration::from_millis(350)) => {
                let s = if phase { on } else { "                " };
                print!("\r{s}");
                let _ = std::io::stdout().flush();
                phase = !phase;
            }
        }
    };

    print!("\r{}\r", " ".repeat(on.len()));
    let _ = std::io::stdout().flush();

    let mut text_buf = String::new();
    let mut think_buf = String::new();
    let mut parser = ThinkParser::new();
    let mut input_tokens: u32 = 0;

    if let Some(ev) = first {
        if let io_runtime::AgentEvent::Usage { input_tokens: n, .. } = &ev {
            input_tokens = *n;
        }
        process_ev(ev, &mut text_buf, &mut think_buf, &mut parser);
        while let Some(ev) = rx.recv().await {
            if let io_runtime::AgentEvent::Usage { input_tokens: n, .. } = &ev {
                input_tokens = *n;
            }
            process_ev(ev, &mut text_buf, &mut think_buf, &mut parser);
        }
    }

    let (rem_text, rem_think) = parser.flush();
    text_buf.push_str(&rem_text);
    think_buf.push_str(&rem_think);

    if !text_buf.is_empty() {
        print!("\r\n");
        let _ = std::io::stdout().flush();
        render_markdown(&text_buf);
    }

    let thoughts = if think_buf.trim().is_empty() { None } else { Some(think_buf) };
    (thoughts, input_tokens)
}

/// Render a compact context bar: `ctx [████░░░░░░] 13% of 200K`
fn render_context_bar(input_tokens: u32, context_window: u64) -> String {
    let pct = ((input_tokens as f64 / context_window as f64) * 100.0).min(100.0) as usize;
    let bar_width = 10usize;
    let filled = (pct * bar_width / 100).min(bar_width);
    let empty = bar_width - filled;
    let window_label = if context_window >= 1_000_000 {
        format!("{}M", context_window / 1_000_000)
    } else {
        format!("{}K", context_window / 1_000)
    };
    format!("ctx [{}{}] {}% of {}", "█".repeat(filled), "░".repeat(empty), pct, window_label)
}

fn print_prompt(agent: &io_runtime::Agent, input_tokens: u32, agent_id: &str) {
    use crossterm::style::Stylize;
    use std::io::Write;

    let provider_model = format!("  {} · {} · {}", agent_id, agent.provider_id, agent.model_id);
    let status = if input_tokens > 0 {
        let bar = render_context_bar(input_tokens, agent.context_window());
        format!("{}  |  {}", provider_model, bar)
    } else {
        provider_model
    };
    print!("{}\n>>> ", status.dark_grey());
    let _ = std::io::stdout().flush();
}

fn render_markdown(text: &str) {
    use std::io::Write;

    let mut skin = termimad::MadSkin::default();
    skin.paragraph.align = termimad::Alignment::Left;
    skin.paragraph.left_margin = 0;
    skin.code_block.align = termimad::Alignment::Left;
    skin.code_block.left_margin = 0;
    for h in &mut skin.headers {
        h.align = termimad::Alignment::Left;
        h.left_margin = 0;
    }
    skin.headers[0].set_fg(termimad::crossterm::style::Color::Green);
    skin.headers[0].add_attr(termimad::crossterm::style::Attribute::Bold);
    skin.bold.set_fg(termimad::crossterm::style::Color::Green);
    skin.inline_code.set_fg(termimad::crossterm::style::Color::Yellow);
    skin.code_block.set_fg(termimad::crossterm::style::Color::Yellow);
    skin.italic.set_fg(termimad::crossterm::style::Color::White);
    skin.table.align = termimad::Alignment::Left;
    skin.table.left_margin = 0;

    // Write each line with \r\n so rendering works in raw mode
    // (raw mode \n doesn't carriage-return, causing horizontal drift)
    let rendered = format!("{}", skin.text(text, None));
    let mut out = std::io::stdout();
    for line in rendered.lines() {
        use crossterm::QueueableCommand;
        let _ = out.queue(crossterm::style::Print(line));
        let _ = out.queue(crossterm::style::Print("\r\n"));
    }
    let _ = out.flush();
}

fn render_thoughts(thoughts: &str) {
    use crossterm::{
        execute,
        style::{Color, Print, ResetColor, SetForegroundColor},
    };
    use std::io::stdout;

    let trimmed = thoughts.trim();
    if trimmed.is_empty() {
        return;
    }

    let prefix = "(thought): ";
    let indent = " ".repeat(prefix.len());

    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::DarkCyan),
        Print(prefix),
        ResetColor,
    );

    let mut lines = trimmed.lines();
    if let Some(first) = lines.next() {
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::DarkGrey),
            Print(first),
            Print("\n"),
            ResetColor,
        );
        for line in lines {
            let _ = execute!(
                stdout(),
                Print(&indent),
                SetForegroundColor(Color::DarkGrey),
                Print(line),
                Print("\n"),
                ResetColor,
            );
        }
    }
    println!();
}

fn render_tool_start(name: &str, input: &serde_json::Value) {
    use crossterm::style::{Stylize, Color};
    let label = format!(" {name} ").with(Color::Black).on(Color::DarkGrey);
    let detail = match name {
        "bash"  => input.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "read"  => input.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "write" => input.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "edit"  => input.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "glob"  => input.get("pattern").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "grep"  => {
            let pat  = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("{pat}  in  {path}")
        }
        "spawn_agent" => {
            let agent_id = input.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
            let task = input.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let agent_name = io_agents::builtin::by_id(agent_id)
                .map(|c| c.name)
                .unwrap_or(agent_id);
            let task_preview = {
                let mut indices = task.char_indices();
                match indices.nth(60) {
                    Some((idx, _)) => format!("{}…", &task[..idx]),
                    None => task.to_string(),
                }
            };
            format!("[{agent_name}]  {task_preview}")
        }
        _ => String::new(),
    };
    if detail.is_empty() {
        print!("  {label}\r\n");
    } else {
        print!("  {label}  {}\r\n", detail.dark_grey());
    }
    let _ = std::io::stdout().flush();
}

fn render_tool_done(name: &str, output: &str, success: bool) {
    if !success {
        use crossterm::style::Stylize;
        print!("  {}\r\n", format!("error: {output}").red());
        let _ = std::io::stdout().flush();
        return;
    }
    match name {
        "write" | "edit" => render_diff(output),
        _ => {}
    }
}

fn render_diff(diff: &str) {
    use crossterm::{
        execute,
        style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    };
    use std::io::stdout;

    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for raw in diff.lines() {
        if raw.starts_with("--- ") || raw.starts_with("+++ ") {
            // file header — dim grey
            let _ = execute!(stdout(),
                SetForegroundColor(Color::DarkGrey),
                Print(format!("  {raw}\r\n")),
                ResetColor
            );
        } else if let Some(rest) = raw.strip_prefix("@@ ") {
            // parse @@ -a,b +c,d @@ to reset counters
            if let Some((a, b)) = parse_hunk(rest) {
                old_line = a;
                new_line = b;
            }
            let _ = execute!(stdout(),
                SetForegroundColor(Color::DarkCyan),
                Print(format!("  @@ {rest}\r\n")),
                ResetColor
            );
        } else if let Some(content) = raw.strip_prefix('-') {
            let _ = execute!(stdout(),
                SetForegroundColor(Color::Red),
                Print(format!("{:>5} ", old_line)),
                SetForegroundColor(Color::Black),
                SetBackgroundColor(Color::Red),
                Print("-"),
                ResetColor,
                SetBackgroundColor(Color::DarkRed),
                SetForegroundColor(Color::White),
                Print(format!("  {content}")),
                ResetColor,
                Print("\r\n")
            );
            old_line += 1;
        } else if let Some(content) = raw.strip_prefix('+') {
            let _ = execute!(stdout(),
                SetForegroundColor(Color::Green),
                Print(format!("{:>5} ", new_line)),
                SetForegroundColor(Color::Black),
                SetBackgroundColor(Color::Green),
                Print("+"),
                ResetColor,
                SetBackgroundColor(Color::DarkGreen),
                SetForegroundColor(Color::White),
                Print(format!("  {content}")),
                ResetColor,
                Print("\r\n")
            );
            new_line += 1;
        } else if let Some(content) = raw.strip_prefix(' ') {
            print!("{:>5} {:>5}    {content}\r\n", old_line, new_line);
            old_line += 1;
            new_line += 1;
        }
    }
}

fn parse_hunk(s: &str) -> Option<(u32, u32)> {
    // expects "-A,B +C,D @@…" (the "@@ " prefix already stripped)
    let s = s.trim_start_matches('-');
    let mut parts = s.splitn(2, ' ');
    let old_part = parts.next()?;
    let rest = parts.next()?.trim_start_matches('+');
    let a = old_part.split(',').next()?.parse().ok()?;
    let b = rest.split(',').next()?.parse().ok()?;
    Some((a, b))
}


async fn run_bash(cmd: &str) -> String {
    const ALLOWED_SHELLS: &[&str] = &[
        "/bin/sh", "/bin/bash", "/usr/bin/bash",
        "/bin/zsh", "/usr/bin/zsh",
        "/usr/local/bin/bash", "/usr/local/bin/zsh", "/usr/local/bin/sh",
    ];
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| ALLOWED_SHELLS.contains(&s.as_str()))
        .unwrap_or_else(|| "/bin/sh".to_string());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(&shell).arg("-lc").arg(cmd).output(),
    ).await;

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
