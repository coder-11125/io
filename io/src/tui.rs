//! The interactive TUI and single-shot runner: agent construction, the
//! per-turn streaming/cancellation/permission dance, and `@path` mentions.

use crate::cost::show_cost_summary;
use crate::{agent, connect, model};
use io_runtime::types::SessionId;
use io_tui::picker;
use io_tui::readline;
use io_tui::render::{
    clear_prompt_input, draw_prompt_bar, enter_tui, exit_tui, prepare_streaming,
    render_scroll_view, render_tool_done, render_tool_start, tool_detail, PROMPT_BAR_HEIGHT,
};
use std::io::Write;

/// Which session the agent should run in.
enum SessionChoice {
    New,
    Continue,
    Existing(SessionId),
}

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

async fn build_agent(
    session: SessionChoice,
    agent_config: &io_agents::AgentConfig,
) -> anyhow::Result<io_runtime::Agent> {
    let config = io_runtime::config::Config::load()?;
    let keys = io_runtime::config::KeyStore::load();
    let model_id = config.provider.active_model();
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
    ))
}

const MAX_AT_FILE_BYTES: usize = 100 * 1024;

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

type PendingPermission = std::sync::Arc<
    std::sync::Mutex<Option<tokio::sync::oneshot::Sender<io_runtime::PermissionReply>>>,
>;

/// Shared plain-text line buffer used for in-session scrollback.
type LineBuf = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>;

const MAX_SCROLL_LINES: usize = 5000;
const SCROLL_STEP: usize = 3;

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

    let build_config = io_agents::builtin::by_id("build").expect("build agent must exist");
    let agent = build_agent(SessionChoice::New, &build_config).await?;
    agent.set_prompt_fn(std::sync::Arc::new(prompt_on_stdin));
    let response = agent.run_turn(&resolve_at_mentions(prompt)).await?;
    println!("{response}");
    Ok(())
}

// ── Slash commands (for TUI completion popup) ──────────────────────────────────

use io_tui::SLASH_COMMANDS;

fn filter_slash_commands(buf: &str) -> Vec<usize> {
    if buf.is_empty() || !buf.starts_with('/') {
        return vec![];
    }
    let lower = buf.to_ascii_lowercase();
    SLASH_COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| name.starts_with(&lower))
        .map(|(i, _)| i)
        .collect()
}

// ── TUI input helpers ──────────────────────────────────────────────────────────

/// Maximum number of popup rows to show above the prompt bar.
const MAX_POPUP_ROWS: u16 = 10;

/// Clear `count` rows above the prompt bar, starting from the row just above it.
fn clear_rows_above(count: u16) -> std::io::Result<()> {
    use crossterm::{cursor, execute, terminal};
    if count == 0 {
        return Ok(());
    }
    let (_, h) = crossterm::terminal::size()?;
    let top_row = h.saturating_sub(PROMPT_BAR_HEIGHT + 1); // first row above prompt bar
    let start = top_row.saturating_sub(count.saturating_sub(1));
    for row in start..=top_row {
        execute!(
            std::io::stdout(),
            cursor::MoveTo(0, row),
            terminal::Clear(crossterm::terminal::ClearType::CurrentLine),
        )?;
    }
    Ok(())
}

/// Draw a file-completion popup above the prompt bar.
/// Returns the number of rows drawn.
fn draw_file_popup(
    items: &[String],
    selected: Option<usize>,
    scroll: usize,
) -> std::io::Result<u16> {
    use crossterm::{
        cursor, execute,
        style::{Color, Print, ResetColor, SetForegroundColor},
    };
    if items.is_empty() {
        clear_rows_above(0)?;
        return Ok(0);
    }

    let total = items.len();
    let has_above = scroll > 0;
    let mut item_rows = (MAX_POPUP_ROWS as usize).saturating_sub(usize::from(has_above));
    let has_below = scroll + item_rows < total;
    if has_below {
        item_rows = item_rows.saturating_sub(1);
    }
    let window = &items[scroll..(scroll + item_rows).min(total)];
    let count = usize::from(has_above) + window.len() + usize::from(has_below);

    let (_, h) = crossterm::terminal::size()?;
    let top_row = h.saturating_sub(PROMPT_BAR_HEIGHT + 1);
    let start_row = top_row.saturating_sub(count.saturating_sub(1) as u16);

    clear_rows_above(count as u16)?;

    let mut render_idx = 0usize;
    for row in start_row..=top_row {
        execute!(std::io::stdout(), cursor::MoveTo(0, row))?;

        if render_idx == 0 && has_above {
            execute!(
                std::io::stdout(),
                SetForegroundColor(Color::DarkGrey),
                Print(format!("  \u{2191} {} above", scroll)),
                ResetColor,
            )?;
            render_idx += 1;
        } else {
            let item_i = render_idx.saturating_sub(usize::from(has_above));
            if item_i < window.len() {
                let item = &window[item_i];
                let abs_idx = scroll + item_i;
                let is_dir = item.ends_with('/');
                if selected == Some(abs_idx) {
                    execute!(
                        std::io::stdout(),
                        SetForegroundColor(Color::Cyan),
                        Print("\u{25b6} "),
                        ResetColor,
                        Print(item),
                    )?;
                } else if is_dir {
                    execute!(
                        std::io::stdout(),
                        SetForegroundColor(Color::Cyan),
                        Print(format!("  {item}")),
                        ResetColor,
                    )?;
                } else {
                    execute!(
                        std::io::stdout(),
                        SetForegroundColor(Color::DarkGrey),
                        Print(format!("  {item}")),
                        ResetColor,
                    )?;
                }
                render_idx += 1;
            } else if has_below && item_i == window.len() {
                let remaining = total.saturating_sub(scroll + window.len());
                execute!(
                    std::io::stdout(),
                    SetForegroundColor(Color::DarkGrey),
                    Print(format!("  \u{2193} {} below", remaining)),
                    ResetColor,
                )?;
                render_idx += 1;
            }
        }
    }
    std::io::stdout().flush()?;
    Ok(count as u16)
}

/// Draw a bordered slash-command picker above the prompt bar.
/// Returns the number of rows drawn (including border rows).
fn draw_slash_popup(matches: &[usize], selected: Option<usize>) -> std::io::Result<u16> {
    use crossterm::{
        cursor, execute,
        style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    };
    if matches.is_empty() {
        clear_rows_above(0)?;
        return Ok(0);
    }

    let n = matches.len().min(MAX_POPUP_ROWS as usize);
    let (w, h) = crossterm::terminal::size()?;

    let name_w = matches
        .iter()
        .map(|&i| SLASH_COMMANDS[i].0.len())
        .max()
        .unwrap_or(0);
    let desc_w = matches
        .iter()
        .map(|&i| SLASH_COMMANDS[i].1.len())
        .max()
        .unwrap_or(0);
    // Item inner content: " ▶ /name   desc " — indicator(1) + space + name(padded) + 2 + desc
    let inner_w = (1 + 1 + name_w + 2 + desc_w + 1).min(w as usize - 2);
    let box_w = (inner_w + 2) as u16;

    // total_rows = n items + top border + bottom border
    let total_rows = n as u16 + 2;
    let top_row = h.saturating_sub(PROMPT_BAR_HEIGHT + total_rows);

    // Clear the maximum possible popup height so a previously larger popup
    // doesn't leave ghost rows when the match list shrinks.
    clear_rows_above(MAX_POPUP_ROWS + 2)?;

    // Top border
    execute!(
        std::io::stdout(),
        cursor::MoveTo(0, top_row),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("╭{}╮", "─".repeat(inner_w))),
        ResetColor,
    )?;

    for (i, &idx) in matches[..n].iter().enumerate() {
        let row = top_row + 1 + i as u16;
        let (name, desc) = SLASH_COMMANDS[idx];
        let is_sel = selected == Some(i);
        let indicator = if is_sel { "▶" } else { " " };
        let pad = " ".repeat(name_w - name.len() + 2);
        let content = format!("{indicator} {name}{pad}{desc}");
        // Truncate at char boundary to inner_w
        let mut end = content.len().min(inner_w);
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        let padded = format!("{:<width$}", &content[..end], width = inner_w);

        execute!(
            std::io::stdout(),
            cursor::MoveTo(0, row),
            SetForegroundColor(Color::DarkGrey),
            Print("│"),
            ResetColor,
        )?;
        if is_sel {
            execute!(
                std::io::stdout(),
                SetBackgroundColor(Color::DarkGrey),
                SetForegroundColor(Color::White),
                Print(&padded),
                ResetColor,
            )?;
        } else {
            // Name in default color, desc in dark grey
            let name_part = format!("{indicator} {name}{pad}");
            execute!(
                std::io::stdout(),
                Print(&name_part),
                SetForegroundColor(Color::DarkGrey),
                Print(desc),
                ResetColor,
            )?;
        }
        execute!(
            std::io::stdout(),
            cursor::MoveTo(box_w - 1, row),
            SetForegroundColor(Color::DarkGrey),
            Print("│"),
            ResetColor,
        )?;
    }

    // Bottom border
    execute!(
        std::io::stdout(),
        cursor::MoveTo(0, top_row + n as u16 + 1),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("╰{}╯", "─".repeat(inner_w))),
        ResetColor,
    )?;

    std::io::stdout().flush()?;
    Ok(total_rows)
}

// ── TUI read line ──────────────────────────────────────────────────────────────

/// Read a line of input from the user in TUI mode. The prompt bar at the bottom
/// of the terminal shows the current buffer. Supports /slash completions,
/// @file mentions, and Tab agent cycling.
#[allow(clippy::too_many_arguments)]
fn tui_read_line(
    full_agents: &[io_agents::AgentConfig],
    tab_current: &mut usize,
    agent: &io_runtime::Agent,
    last_input_tokens: u32,
    context_window: u64,
    current_agent_id: &str,
    theme: &io_tui::render::Theme,
    line_buf: &LineBuf,
) -> anyhow::Result<Option<String>> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

    let mut buf = String::new();

    let mut file_all: Option<Vec<String>> = None;
    let mut file_filtered: Vec<String> = vec![];
    let mut file_selected: Option<usize> = None;
    let mut file_scroll: usize = 0;

    let mut slash_matches: Vec<usize> = Vec::new();
    let mut slash_selected: Option<usize> = None;
    let mut popup_rows: u16 = 0;
    let mut scroll_offset: usize = 0;

    // Draw the prompt bar with the currently-selected agent info.
    let bar = |input: &str, tc: usize| -> std::io::Result<()> {
        let name = full_agents
            .get(tc)
            .map(|a| a.name)
            .unwrap_or(current_agent_id);
        draw_prompt_bar(
            input,
            name,
            agent.provider_id,
            &agent.model_id,
            last_input_tokens,
            context_window,
            theme,
        )
    };

    bar("", *tab_current)?;
    crossterm::execute!(std::io::stdout(), crossterm::cursor::Show)?;

    loop {
        let ev = event::read()?;
        match ev {
            Event::Resize(_, _) => {
                io_tui::render::handle_resize()?;
                if file_all.is_some() {
                    popup_rows = draw_file_popup(&file_filtered, file_selected, file_scroll)?;
                } else if !slash_matches.is_empty() {
                    popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                } else {
                    popup_rows = 0;
                    // Reflow content area at the current scroll position.
                    render_scroll_view(&line_buf.lock().unwrap(), scroll_offset, theme)?;
                }
                bar(&buf, *tab_current)?;
            }
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        let n = line_buf.lock().unwrap().len();
                        scroll_offset = scroll_offset.saturating_add(SCROLL_STEP).min(n);
                        render_scroll_view(&line_buf.lock().unwrap(), scroll_offset, theme)?;
                        crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide)?;
                    }
                    MouseEventKind::ScrollDown if scroll_offset > 0 => {
                        scroll_offset = scroll_offset.saturating_sub(SCROLL_STEP);
                        render_scroll_view(&line_buf.lock().unwrap(), scroll_offset, theme)?;
                        if scroll_offset == 0 {
                            bar(&buf, *tab_current)?;
                            crossterm::execute!(std::io::stdout(), crossterm::cursor::Show)?;
                        } else {
                            crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide)?;
                        }
                    }
                    _ => {}
                }
                continue;
            }
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                // Any key press while in scroll mode returns to live view first.
                if scroll_offset > 0 {
                    scroll_offset = 0;
                    render_scroll_view(&line_buf.lock().unwrap(), scroll_offset, theme)?;
                    bar(&buf, *tab_current)?;
                    crossterm::execute!(std::io::stdout(), crossterm::cursor::Show)?;
                }
                match (key.code, key.modifiers) {
                    (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        clear_rows_above(popup_rows)?;
                        crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide)?;
                        return Ok(None);
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        clear_rows_above(popup_rows)?;
                        bar("", *tab_current)?;
                        return Ok(Some(String::new()));
                    }

                    (KeyCode::Enter, _) => {
                        if file_all.is_some() {
                            if let Some(s) = file_selected {
                                if s < file_filtered.len() {
                                    if let Some((at_pos, _)) = readline::at_prefix(&buf) {
                                        buf.truncate(at_pos);
                                        buf.push('@');
                                        buf.push_str(&file_filtered[s]);
                                        file_all = None;
                                        file_filtered.clear();
                                        file_selected = None;
                                        file_scroll = 0;
                                        popup_rows = 0;
                                        bar(&buf, *tab_current)?;
                                        continue;
                                    }
                                }
                            }
                        }
                        if let Some(s) = slash_selected {
                            if s < slash_matches.len() {
                                buf = SLASH_COMMANDS[slash_matches[s]].0.to_string();
                            }
                        }
                        clear_rows_above(popup_rows)?;
                        bar(&buf, *tab_current)?;
                        crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide)?;
                        return Ok(Some(buf));
                    }

                    (KeyCode::Backspace, _) => {
                        buf.pop();
                        let mut new_rows: u16 = 0;
                        if let Some(ref all) = file_all {
                            let prefix = readline::at_prefix(&buf).map(|(_, p)| p).unwrap_or("");
                            file_filtered = readline::filter_files(all, prefix);
                            file_selected = if file_filtered.is_empty() {
                                None
                            } else {
                                Some(0)
                            };
                            file_scroll = 0;
                            new_rows = draw_file_popup(&file_filtered, file_selected, file_scroll)?;
                            slash_matches.clear();
                            slash_selected = None;
                        } else {
                            slash_matches = filter_slash_commands(&buf);
                            slash_selected = None;
                            if !slash_matches.is_empty() {
                                new_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                            } else {
                                clear_rows_above(popup_rows)?;
                            }
                        }
                        popup_rows = new_rows;
                        bar(&buf, *tab_current)?;
                    }

                    (KeyCode::Tab, _) => {
                        if buf.is_empty() {
                            if !full_agents.is_empty() {
                                *tab_current = (*tab_current + 1) % full_agents.len();
                                file_all = None;
                                file_filtered.clear();
                                file_selected = None;
                                file_scroll = 0;
                                slash_matches.clear();
                                slash_selected = None;
                                clear_rows_above(popup_rows)?;
                                popup_rows = 0;
                                bar("", *tab_current)?;
                            }
                        } else if file_all.is_some() && !file_filtered.is_empty() {
                            let next = file_selected
                                .map(|s| (s + 1) % file_filtered.len())
                                .unwrap_or(0);
                            file_selected = Some(next);
                            if next >= file_scroll + (MAX_POPUP_ROWS as usize).saturating_sub(2) {
                                file_scroll = next
                                    .saturating_add(1)
                                    .saturating_sub(MAX_POPUP_ROWS as usize)
                                    .saturating_sub(2);
                            } else if next < file_scroll {
                                file_scroll = next;
                            }
                            popup_rows =
                                draw_file_popup(&file_filtered, file_selected, file_scroll)?;
                            bar(&buf, *tab_current)?;
                        } else if !slash_matches.is_empty() {
                            slash_selected = Some(match slash_selected {
                                None => 0,
                                Some(s) => (s + 1) % slash_matches.len(),
                            });
                            popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                            bar(&buf, *tab_current)?;
                        }
                    }

                    (KeyCode::Esc, _) if file_all.is_some() || slash_selected.is_some() => {
                        file_all = None;
                        file_filtered.clear();
                        file_selected = None;
                        file_scroll = 0;
                        slash_matches.clear();
                        slash_selected = None;
                        clear_rows_above(popup_rows)?;
                        popup_rows = 0;
                        bar(&buf, *tab_current)?;
                    }

                    (KeyCode::Down, _) => {
                        if file_all.is_some() && !file_filtered.is_empty() {
                            let next = file_selected
                                .map(|s| (s + 1) % file_filtered.len())
                                .unwrap_or(0);
                            file_selected = Some(next);
                            if next >= file_scroll + (MAX_POPUP_ROWS as usize).saturating_sub(2) {
                                file_scroll = next
                                    .saturating_add(1)
                                    .saturating_sub(MAX_POPUP_ROWS as usize)
                                    .saturating_sub(2);
                            } else if next < file_scroll {
                                file_scroll = next;
                            }
                            popup_rows =
                                draw_file_popup(&file_filtered, file_selected, file_scroll)?;
                            bar(&buf, *tab_current)?;
                        } else if !slash_matches.is_empty() {
                            slash_selected = Some(match slash_selected {
                                None => 0,
                                Some(s) => (s + 1) % slash_matches.len(),
                            });
                            popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                            bar(&buf, *tab_current)?;
                        }
                    }

                    (KeyCode::Up, _) => {
                        if file_all.is_some() && !file_filtered.is_empty() {
                            let prev = match file_selected {
                                None | Some(0) => file_filtered.len() - 1,
                                Some(s) => s - 1,
                            };
                            file_selected = Some(prev);
                            if prev < file_scroll {
                                file_scroll = prev;
                            } else if prev
                                >= file_scroll + (MAX_POPUP_ROWS as usize).saturating_sub(2)
                            {
                                file_scroll = prev
                                    .saturating_add(1)
                                    .saturating_sub(MAX_POPUP_ROWS as usize)
                                    .saturating_sub(2);
                            }
                            popup_rows =
                                draw_file_popup(&file_filtered, file_selected, file_scroll)?;
                            bar(&buf, *tab_current)?;
                        } else if !slash_matches.is_empty() {
                            let prev = match slash_selected {
                                None | Some(0) => slash_matches.len() - 1,
                                Some(s) => s - 1,
                            };
                            slash_selected = Some(prev);
                            popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                            bar(&buf, *tab_current)?;
                        }
                    }

                    (KeyCode::Char('@'), _) => {
                        buf.push('@');
                        let all = readline::list_files();
                        file_filtered = all.clone();
                        file_all = Some(all);
                        file_selected = if file_filtered.is_empty() {
                            None
                        } else {
                            Some(0)
                        };
                        file_scroll = 0;
                        slash_matches.clear();
                        slash_selected = None;
                        popup_rows = draw_file_popup(&file_filtered, file_selected, file_scroll)?;
                        bar(&buf, *tab_current)?;
                    }

                    (KeyCode::Char(' '), _) if file_all.is_some() => {
                        buf.push(' ');
                        file_all = None;
                        file_filtered.clear();
                        file_selected = None;
                        file_scroll = 0;
                        slash_matches.clear();
                        slash_selected = None;
                        clear_rows_above(popup_rows)?;
                        popup_rows = 0;
                        bar(&buf, *tab_current)?;
                    }

                    (KeyCode::Char(c), _) => {
                        buf.push(c);
                        if file_all.is_some() {
                            if let Some((_, prefix)) = readline::at_prefix(&buf) {
                                if let Some(all) = &file_all {
                                    file_filtered = readline::filter_files(all, prefix);
                                }
                            }
                            file_selected = if file_filtered.is_empty() {
                                None
                            } else {
                                Some(0)
                            };
                            file_scroll = 0;
                            popup_rows =
                                draw_file_popup(&file_filtered, file_selected, file_scroll)?;
                            slash_matches.clear();
                            slash_selected = None;
                        } else {
                            slash_matches = filter_slash_commands(&buf);
                            if slash_matches.is_empty() {
                                slash_selected = None;
                                if popup_rows > 0 {
                                    clear_rows_above(popup_rows)?;
                                    popup_rows = 0;
                                }
                            } else {
                                popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                            }
                        }
                        bar(&buf, *tab_current)?;
                    }

                    _ => {}
                }
            }
            _ => {}
        }
    }
}

// ── Splash screen input ────────────────────────────────────────────────────────

/// Read the first message from a centered splash screen.
/// Returns `None` on Ctrl+D (exit), `Some(text)` on Enter with non-empty input.
fn splash_read_line(
    full_agents: &[io_agents::AgentConfig],
    tab_current: &mut usize,
    agent: &io_runtime::Agent,
    theme: &io_tui::render::Theme,
) -> anyhow::Result<Option<String>> {
    use crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
        execute,
    };

    let mut buf = String::new();
    let mut slash_matches: Vec<usize> = Vec::new();
    let mut slash_selected: Option<usize> = None;
    let mut popup_rows: u16 = 0;

    let splash_name = |tc: usize| -> &str {
        full_agents.get(tc).map(|a| a.name).unwrap_or("build")
    };

    let mut layout = io_tui::render::draw_splash(
        &buf,
        splash_name(*tab_current),
        agent.provider_id,
        &agent.model_id,
        theme,
    )?;
    let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
    execute!(std::io::stdout(), cursor::MoveTo(cx, cy), cursor::Show)?;

    loop {
        match event::read()? {
            Event::Resize(_, _) => {
                layout = io_tui::render::draw_splash(
                    &buf,
                    splash_name(*tab_current),
                    agent.provider_id,
                    &agent.model_id,
                    theme,
                )?;
                if !slash_matches.is_empty() {
                    popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                }
                let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
                execute!(std::io::stdout(), cursor::MoveTo(cx, cy), cursor::Show)?;
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                match (key.code, key.modifiers) {
                    (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        clear_rows_above(popup_rows)?;
                        execute!(std::io::stdout(), cursor::Hide)?;
                        return Ok(None);
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        buf.clear();
                        slash_matches.clear();
                        slash_selected = None;
                        clear_rows_above(popup_rows)?;
                        popup_rows = 0;
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Enter, _) => {
                        if let Some(s) = slash_selected {
                            if s < slash_matches.len() {
                                buf = SLASH_COMMANDS[slash_matches[s]].0.to_string();
                            }
                        }
                        clear_rows_above(popup_rows)?;
                        if !buf.trim().is_empty() {
                            execute!(std::io::stdout(), cursor::Hide)?;
                            return Ok(Some(buf));
                        }
                    }
                    (KeyCode::Esc, _) if !slash_matches.is_empty() => {
                        slash_matches.clear();
                        slash_selected = None;
                        clear_rows_above(popup_rows)?;
                        popup_rows = 0;
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Backspace, _) => {
                        buf.pop();
                        slash_matches = filter_slash_commands(&buf);
                        slash_selected = None;
                        if !slash_matches.is_empty() {
                            popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                        } else {
                            clear_rows_above(popup_rows)?;
                            popup_rows = 0;
                        }
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Tab, _) | (KeyCode::Down, _) if !slash_matches.is_empty() => {
                        slash_selected = Some(match slash_selected {
                            None => 0,
                            Some(s) => (s + 1) % slash_matches.len(),
                        });
                        popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Up, _) if !slash_matches.is_empty() => {
                        slash_selected = Some(match slash_selected {
                            None | Some(0) => slash_matches.len() - 1,
                            Some(s) => s - 1,
                        });
                        popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Tab, _) if buf.is_empty() && !full_agents.is_empty() => {
                        *tab_current = (*tab_current + 1) % full_agents.len();
                        io_tui::render::splash_update_status(
                            &layout,
                            splash_name(*tab_current),
                            agent.provider_id,
                            &agent.model_id,
                            theme,
                        )?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Char(c), _) => {
                        buf.push(c);
                        slash_matches = filter_slash_commands(&buf);
                        slash_selected = None;
                        if !slash_matches.is_empty() {
                            popup_rows = draw_slash_popup(&slash_matches, slash_selected)?;
                        } else {
                            clear_rows_above(popup_rows)?;
                            popup_rows = 0;
                        }
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

// ── Interactive REPL ───────────────────────────────────────────────────────────

pub async fn run_interactive(
    new_session: bool,
    continue_session: bool,
    _model: Option<&str>,
) -> anyhow::Result<()> {
    let config = io_runtime::config::Config::load()?;
    let mut theme = io_tui::render::get_theme(&config.theme);
    let line_buf: LineBuf = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::with_capacity(512),
    ));
    let keys = io_runtime::config::KeyStore::load();
    if let Some(env) = io_runtime::provider::missing_api_key(&config, &keys) {
        use crossterm::style::Stylize;
        // Print warning before entering TUI
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
    let mut agent = build_agent(startup_session, &current_agent).await?;

    enter_tui()?;

    let mut last_input_tokens: u32 = 0;
    let mut is_splash = true;

    loop {
        let full_agents = io_agents::builtin::full_agents();
        let mut tab_current = full_agents
            .iter()
            .position(|a| a.id == current_agent.id)
            .unwrap_or(0);

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
                match build_agent(SessionChoice::Existing(sid), &current_agent).await {
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
                    "  /model         Switch between configured providers",
                    "  /theme         Switch UI theme",
                    "  /cost          Show API cost summary for current session",
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
                continue;
            }
            "/agent" => {
                match agent::run(current_agent.id) {
                    Ok(new_config) => {
                        current_agent = new_config;
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent).await {
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
                continue;
            }
            "/connect" => {
                match connect::run().await {
                    Ok(()) => {
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent).await {
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
                continue;
            }
            "/model" => {
                match model::run().await {
                    Ok(()) => {
                        let sid = agent.session_id().await;
                        match build_agent(SessionChoice::Existing(sid), &current_agent).await {
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
                continue;
            }
            "/new" => {
                match build_agent(SessionChoice::New, &current_agent).await {
                    Ok(new_agent) => {
                        agent = new_agent;
                        last_input_tokens = 0;
                        // Reset scroll buffer for the new session.
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
                        let _ = draw_prompt_bar(
                            "",
                            current_agent.name,
                            agent.provider_id,
                            &agent.model_id,
                            last_input_tokens,
                            agent.context_window(),
                            &theme,
                        );
                    }
                }
                continue;
            }
            _ if input.starts_with('!') => {
                let cmd = input[1..].trim();
                let output = run_bash(cmd).await;
                // Show bash output at bottom of scroll region
                {
                    let (_, h) = crossterm::terminal::size()?;
                    let row = h.saturating_sub(PROMPT_BAR_HEIGHT + 1);
                    use crossterm::{cursor, execute, style::Print, terminal};
                    execute!(
                        std::io::stdout(),
                        cursor::MoveTo(0, row),
                        terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                        Print(output),
                    )?;
                    std::io::stdout().flush()?;
                }
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

        // Echo the user's message into the content area before the response streams in.
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
        // Capture user message in scroll buffer.
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
        // Shared scroll offset so the key listener can scroll during streaming.
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
                if event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
                    match event::read() {
                        Ok(crossterm::event::Event::Key(k)) => {
                            let mut slot = pending_for_listener.lock().unwrap();
                            if slot.is_some() {
                                let reply = match k.code {
                                    crossterm::event::KeyCode::Char('y')
                                    | crossterm::event::KeyCode::Char('Y')
                                    | crossterm::event::KeyCode::Enter => {
                                        Some(PermissionReply::AllowOnce)
                                    }
                                    crossterm::event::KeyCode::Char('a')
                                    | crossterm::event::KeyCode::Char('A') => {
                                        Some(PermissionReply::AllowSession)
                                    }
                                    crossterm::event::KeyCode::Char('n')
                                    | crossterm::event::KeyCode::Char('N')
                                    | crossterm::event::KeyCode::Esc => Some(PermissionReply::Deny),
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
                            if k.code == crossterm::event::KeyCode::Esc {
                                cancel_for_listener
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                let _ = esc_tx.send(());
                                break;
                            }
                        }
                        Ok(crossterm::event::Event::Mouse(m)) => {
                            let cur =
                                stream_scroll2.load(std::sync::atomic::Ordering::Relaxed);
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
                                stream_scroll2
                                    .store(next, std::sync::atomic::Ordering::Relaxed);
                                let _ = io_tui::render::render_scroll_view(
                                    &line_buf_scroll.lock().unwrap(),
                                    next,
                                    &theme,
                                );
                                let _ = crossterm::execute!(
                                    std::io::stdout(),
                                    crossterm::cursor::Hide
                                );
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
                        let _ = draw_prompt_bar(&format!("error: {e}"), current_agent.name, agent.provider_id, &agent.model_id, last_input_tokens, agent.context_window(), &theme);
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
        // Push thoughts into the scroll buffer so they appear in the clean re-render.
        if let Some(ref t) = agent_thoughts {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                push_line(&line_buf, String::new());
                let prefix = "(thought): ";
                let indent = " ".repeat(prefix.len());
                let mut thought_lines = trimmed.lines();
                if let Some(first) = thought_lines.next() {
                    push_line(
                        &line_buf,
                        format!("\x01\x1b[36m{prefix}\x1b[90m{first}\x1b[0m"),
                    );
                    for line in thought_lines {
                        push_line(
                            &line_buf,
                            format!("\x01{indent}\x1b[90m{line}\x1b[0m"),
                        );
                    }
                }
                push_line(&line_buf, String::new());
            }
        }
        // Re-render the content area from the structured line buffer so the clean
        // "> prompt / tool tree / markdown" view is visible immediately after each
        // turn — not only after scrolling up and back down.
        render_scroll_view(&line_buf.lock().unwrap(), 0, &theme)?;
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

fn char_floor(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

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
    theme: io_tui::render::Theme,
    line_buf: &LineBuf,
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
        AgentEvent::ToolStart { name, input } => {
            render_tool_start(&name, &input);
            let detail = tool_detail(&name, &input);
            let entry = if detail.is_empty() {
                format!("  ╭ {name}")
            } else {
                format!("  ╭ {name}  {detail}")
            };
            push_line(line_buf, entry);
        }
        AgentEvent::ToolDone {
            name,
            output,
            success,
        } => {
            render_tool_done(&name, &output, success, &theme);
            let icon = if success { "✓" } else { "✗" };
            push_line(line_buf, format!("  ╰ {name}  {icon}"));
        }
        AgentEvent::PermissionRequest {
            name,
            input,
            respond,
        } => {
            use crossterm::style::Stylize;
            let detail = tool_detail(&name, &input);
            if detail.is_empty() {
                print!(
                    "\r\n  allow \"{}\"? [y]es / [a]lways / [n]o: ",
                    name.yellow()
                );
            } else {
                print!(
                    "\r\n  allow \"{}\" ({})? [y]es / [a]lways / [n]o: ",
                    name.yellow(),
                    detail
                );
            }
            let _ = std::io::stdout().flush();
            *pending_perm.lock().unwrap() = Some(respond);
        }
        AgentEvent::Usage { .. } => {}
        AgentEvent::AutoCompact { turns_compacted } => {
            print!(
                "\r\n  [auto-compact] Compacted {turns_compacted} turn{} into a summary.\r\n",
                if turns_compacted == 1 { "" } else { "s" }
            );
            let _ = std::io::stdout().flush();
        }
    }
}

fn push_line(buf: &LineBuf, line: String) {
    let mut g = buf.lock().unwrap();
    g.push_back(line);
    while g.len() > MAX_SCROLL_LINES {
        g.pop_front();
    }
}

async fn blink_and_print(
    mut rx: tokio::sync::mpsc::Receiver<io_runtime::AgentEvent>,
    pending_perm: PendingPermission,
    theme: io_tui::render::Theme,
    line_buf: LineBuf,
) -> (Option<String>, u32) {
    const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut spinner_idx = 0;

    let first = loop {
        tokio::select! {
            ev = rx.recv() => break ev,
            _ = tokio::time::sleep(std::time::Duration::from_millis(80)) => {
                print!("\r{}", SPINNER[spinner_idx]);
                let _ = std::io::stdout().flush();
                spinner_idx = (spinner_idx + 1) % SPINNER.len();
            }
        }
    };

    print!("\r \r");
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
            theme,
            &line_buf,
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
                theme,
                &line_buf,
            );
        }
    }

    let (rem_text, rem_think) = parser.flush();
    text_buf.push_str(&rem_text);
    think_buf.push_str(&rem_think);

    if !text_buf.is_empty() {
        print!("\r\n\r\n");
        let _ = std::io::stdout().flush();
        let ansi_lines = io_tui::render::render_markdown_lines(&text_buf, &theme);
        // Print rendered output to terminal.
        {
            use crossterm::QueueableCommand;
            let mut out = std::io::stdout();
            for line in &ansi_lines {
                let _ = out.queue(crossterm::style::Print(line));
                let _ = out.queue(crossterm::style::Print("\r\n"));
            }
            let _ = out.flush();
        }
        // Store ANSI lines in scroll buffer (prefixed \x01 = pre-rendered).
        push_line(&line_buf, String::new());
        for line in ansi_lines {
            push_line(&line_buf, format!("\x01{}", line));
        }
        push_line(&line_buf, String::new());
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
