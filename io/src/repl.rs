//! The interactive REPL and single-shot runner: agent construction, the
//! per-turn streaming/cancellation/permission dance, and `@path` mentions.

use crate::cost::show_cost_summary;
use crate::render::{
    print_prompt, render_markdown, render_thoughts, render_tool_done, render_tool_start,
    tool_detail,
};
use crate::{agent, connect, model, picker, readline};
use io_runtime::types::SessionId;
use std::io::Write;

fn print_banner() {
    println!("IO");
}

/// Which session the agent should run in.
enum SessionChoice {
    /// Start a fresh session.
    New,
    /// Resume the most recently updated session (--continue).
    Continue,
    /// Keep a specific session — used when switching agent/provider/model
    /// mid-conversation so history is preserved.
    Existing(SessionId),
}

async fn build_agent(
    session: SessionChoice,
    system_prompt: String,
) -> anyhow::Result<io_runtime::Agent> {
    let config = io_runtime::config::Config::load()?;
    let keys = io_runtime::config::KeyStore::load();
    let model_id = config.provider.active_model();
    let provider = io_runtime::provider::create_provider(&config, &keys)?;
    let memory = io_runtime::memory::SessionStore::new()?;
    let permissions = std::sync::Arc::new(io_runtime::sandbox::PermissionChecker::from(
        &config.permissions,
    ));

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
    );
    tools.register(Box::new(spawn_tool));

    Ok(io_runtime::Agent::new(
        provider,
        tools,
        memory,
        permissions,
        system_prompt,
        session_id,
        model_id,
        config.session.max_tokens,
        config.session.auto_compact,
    ))
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
                if is_dir {
                    format!("{name}/")
                } else {
                    name
                }
            })
            .collect();
        lines.sort_by(|a, b| b.ends_with('/').cmp(&a.ends_with('/')).then(a.cmp(b)));
        Some(format!(
            "<file path=\"{path}\">\n{}\n</file>",
            lines.join("\n")
        ))
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

/// Slot holding the responder for an in-flight permission prompt. Filled by
/// the event printer when the agent asks, answered by the key listener.
type PendingPermission = std::sync::Arc<
    std::sync::Mutex<Option<tokio::sync::oneshot::Sender<io_runtime::PermissionReply>>>,
>;

/// Blocking stdin permission prompt for single-shot mode (no raw-mode UI).
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

pub async fn run_single_shot(prompt: &str) -> anyhow::Result<()> {
    let config = io_runtime::config::Config::load()?;
    let keys = io_runtime::config::KeyStore::load();
    if let Some(env) = io_runtime::provider::missing_api_key(&config, &keys) {
        anyhow::bail!(
            "no API key found for provider \"{}\".\nExport {env}, or run `io` and use /connect to set one up.",
            config.provider.default
        );
    }

    let system_prompt = io_agents::builtin::by_id("build")
        .expect("build agent must exist")
        .system_prompt;
    let agent = build_agent(SessionChoice::New, system_prompt).await?;
    agent.set_prompt_fn(std::sync::Arc::new(prompt_on_stdin));
    let response = agent.run_turn(&resolve_at_mentions(prompt)).await?;
    println!("{response}");
    Ok(())
}

pub async fn run_interactive(
    new_session: bool,
    continue_session: bool,
    _model: Option<&str>,
) -> anyhow::Result<()> {
    print_banner();

    {
        let config = io_runtime::config::Config::load()?;
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
    }

    let mut current_agent = io_agents::builtin::by_id("build").expect("build agent must exist");
    let startup_session = if continue_session && !new_session {
        SessionChoice::Continue
    } else {
        SessionChoice::New
    };
    let mut agent = build_agent(startup_session, current_agent.system_prompt.clone()).await?;

    let sid = agent.session_id().await;
    let short_id = &sid.to_string()[..8];
    println!("session: {short_id}  |  /help  /agent  /connect  /exit");
    println!();

    let mut last_input_tokens: u32 = 0;

    loop {
        print_prompt(&agent, last_input_tokens, current_agent.id);

        let full_agents = io_agents::builtin::full_agents();
        let tab_current = full_agents
            .iter()
            .position(|a| a.id == current_agent.id)
            .unwrap_or(0);
        let tab_statuses: Vec<String> = full_agents
            .iter()
            .map(|a| format!("  {} · {} · {}", a.id, agent.provider_id, agent.model_id))
            .collect();
        let ctx = readline::ReadLineCtx {
            tab_statuses,
            tab_current,
        };

        let output = match tokio::task::spawn_blocking(move || readline::read_line(ctx)).await?? {
            Some(out) => out,
            None => break,
        };

        // Sync agent if Tab cycling changed the selection — rebuild once on Enter, not per keypress.
        if output.agent_idx != tab_current {
            if let Some(picked) = full_agents.into_iter().nth(output.agent_idx) {
                current_agent = picked;
                let sid = agent.session_id().await;
                match build_agent(
                    SessionChoice::Existing(sid),
                    current_agent.system_prompt.clone(),
                )
                .await
                {
                    Ok(new_agent) => agent = new_agent,
                    Err(e) => eprintln!("error switching agent: {e}"),
                }
            }
        }

        let input = output.text.trim().to_string();

        if input.is_empty() {
            continue;
        }

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
                        let sid = agent.session_id().await;
                        match build_agent(
                            SessionChoice::Existing(sid),
                            current_agent.system_prompt.clone(),
                        )
                        .await
                        {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => eprintln!("error reloading agent: {e}"),
                        }
                    }
                    Err(e) if !e.is::<picker::Dismissed>() => {
                        eprintln!("error: {e}");
                    }
                    _ => {}
                }
                continue;
            }
            "/connect" => {
                match connect::run().await {
                    Ok(()) => {
                        let sid = agent.session_id().await;
                        match build_agent(
                            SessionChoice::Existing(sid),
                            current_agent.system_prompt.clone(),
                        )
                        .await
                        {
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
                        let sid = agent.session_id().await;
                        match build_agent(
                            SessionChoice::Existing(sid),
                            current_agent.system_prompt.clone(),
                        )
                        .await
                        {
                            Ok(new_agent) => agent = new_agent,
                            Err(e) => eprintln!("error reloading provider: {e}"),
                        }
                    }
                    Err(e) if !e.is::<picker::Dismissed>() => {
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
        let pending_perm: PendingPermission = std::sync::Arc::new(std::sync::Mutex::new(None));
        let print_task = tokio::spawn(blink_and_print(token_rx, pending_perm.clone()));

        // Background thread: poll keys — answer permission prompts (y/a/n),
        // otherwise Esc signals cancellation.
        let cancel_for_listener = cancel_flag.clone();
        let pending_for_listener = pending_perm.clone();
        let streaming_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let streaming_done2 = streaming_done.clone();
        let (esc_tx, esc_rx) = tokio::sync::oneshot::channel::<()>();
        let key_listener = tokio::task::spawn_blocking(move || {
            use crossterm::{event, terminal};
            use io_runtime::PermissionReply;
            let _ = terminal::enable_raw_mode();
            loop {
                if streaming_done2.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                if event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
                    if let Ok(event::Event::Key(k)) = event::read() {
                        let mut slot = pending_for_listener.lock().unwrap();
                        if slot.is_some() {
                            // A permission prompt is on screen — interpret the
                            // key as an answer; ignore anything unrecognized.
                            let reply = match k.code {
                                event::KeyCode::Char('y')
                                | event::KeyCode::Char('Y')
                                | event::KeyCode::Enter => Some(PermissionReply::AllowOnce),
                                event::KeyCode::Char('a') | event::KeyCode::Char('A') => {
                                    Some(PermissionReply::AllowSession)
                                }
                                event::KeyCode::Char('n')
                                | event::KeyCode::Char('N')
                                | event::KeyCode::Esc => Some(PermissionReply::Deny),
                                _ => None,
                            };
                            if let Some(reply) = reply {
                                let label = match reply {
                                    PermissionReply::AllowOnce => "yes",
                                    PermissionReply::AllowSession => "always",
                                    PermissionReply::Deny => "no",
                                };
                                print!("{label}\r\n");
                                let _ = std::io::stdout().flush();
                                if let Some(tx) = slot.take() {
                                    let _ = tx.send(reply);
                                }
                            }
                            continue;
                        }
                        drop(slot);
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
                    if !e.is::<io_runtime::Cancelled>() {
                        eprintln!("error: {e}");
                    }
                }
            }
            _ = esc_rx => {}
        }
        streaming_done.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = key_listener.await;

        let (agent_thoughts, turn_input_tokens) = print_task.await.ok().unwrap_or((None, 0));
        if turn_input_tokens > 0 {
            last_input_tokens = turn_input_tokens;
        }
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
        Self {
            pending: String::new(),
            in_think: false,
        }
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
        if self.in_think {
            (String::new(), text)
        } else {
            (text, String::new())
        }
    }
}

fn process_ev(
    ev: io_runtime::AgentEvent,
    text: &mut String,
    think: &mut String,
    parser: &mut ThinkParser,
    pending_perm: &PendingPermission,
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
        AgentEvent::ToolDone {
            name,
            output,
            success,
        } => render_tool_done(&name, &output, success),
        AgentEvent::PermissionRequest { respond, .. } => {
            // The ToolStart line above already shows what will run; the key
            // listener picks the answer up from the shared slot.
            use crossterm::style::Stylize;
            print!("  {} ", "allow? [y]es / [a]lways / [n]o:".yellow());
            let _ = std::io::stdout().flush();
            *pending_perm.lock().unwrap() = Some(respond);
        }
        AgentEvent::Usage { .. } => {} // captured at call site
        AgentEvent::AutoCompact { turns_compacted } => {
            print!(
                "\r\n  [auto-compact] Compacted {turns_compacted} turn{} into a summary.\r\n",
                if turns_compacted == 1 { "" } else { "s" }
            );
            let _ = std::io::stdout().flush();
        }
    }
}

async fn blink_and_print(
    mut rx: tokio::sync::mpsc::Receiver<io_runtime::AgentEvent>,
    pending_perm: PendingPermission,
) -> (Option<String>, u32) {
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
        if let io_runtime::AgentEvent::Usage {
            input_tokens: n, ..
        } = &ev
        {
            input_tokens = *n;
        }
        process_ev(
            ev,
            &mut text_buf,
            &mut think_buf,
            &mut parser,
            &pending_perm,
        );
        while let Some(ev) = rx.recv().await {
            if let io_runtime::AgentEvent::Usage {
                input_tokens: n, ..
            } = &ev
            {
                input_tokens = *n;
            }
            process_ev(
                ev,
                &mut text_buf,
                &mut think_buf,
                &mut parser,
                &pending_perm,
            );
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

    let thoughts = if think_buf.trim().is_empty() {
        None
    } else {
        Some(think_buf)
    };
    (thoughts, input_tokens)
}

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
    use super::ThinkParser;

    fn feed_all(chunks: &[&str]) -> (String, String) {
        let mut p = ThinkParser::new();
        let (mut display, mut thoughts) = (String::new(), String::new());
        for c in chunks {
            let (d, t) = p.feed(c);
            display.push_str(&d);
            thoughts.push_str(&t);
        }
        let (d, t) = p.flush();
        display.push_str(&d);
        thoughts.push_str(&t);
        (display, thoughts)
    }

    #[test]
    fn passes_plain_text_through() {
        let (d, t) = feed_all(&["hello ", "world"]);
        assert_eq!(d, "hello world");
        assert!(t.is_empty());
    }

    #[test]
    fn extracts_think_block() {
        let (d, t) = feed_all(&["a<think>hidden</think>b"]);
        assert_eq!(d, "ab");
        assert_eq!(t, "hidden");
    }

    #[test]
    fn handles_tags_split_across_deltas() {
        let (d, t) = feed_all(&["before<th", "ink>inner", "</th", "ink>after"]);
        assert_eq!(d, "beforeafter");
        assert_eq!(t, "inner");
    }

    #[test]
    fn unterminated_think_flushes_as_thought() {
        let (d, t) = feed_all(&["<think>never closed"]);
        assert!(d.is_empty());
        assert_eq!(t, "never closed");
    }

    #[test]
    fn multibyte_text_survives_lookahead_boundary() {
        let (d, t) = feed_all(&["héllo wörld 日本語", "<think>思考", "</think> done"]);
        assert_eq!(d, "héllo wörld 日本語 done");
        assert_eq!(t, "思考");
    }
}
