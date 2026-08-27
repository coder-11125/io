//! TUI input: readline loops, popup helpers, @file resolution, text editing primitives.

use crate::stream::{LineBuf, SCROLL_STEP};
use io_tui::render::{
    draw_prompt_bar, handle_resize_with_height, render_scroll_view, PROMPT_BAR_HEIGHT,
};
use io_tui::SLASH_COMMANDS;
use std::io::Write;

pub const MAX_AT_FILE_BYTES: usize = 100 * 1024;

/// Maximum number of popup rows to show above the prompt bar.
pub const MAX_POPUP_ROWS: u16 = 10;

// ── @file resolution ───────────────────────────────────────────────────────────

pub fn resolve_at_mentions(input: &str) -> String {
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
        let entries = match std::fs::read_dir(p) {
            Ok(e) => e,
            Err(err) => {
                return Some(format!(
                    "<file path=\"{path}\">\n[error reading directory: {err}]\n</file>"
                ))
            }
        };
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
        let raw = match std::fs::read(p) {
            Ok(r) => r,
            Err(err) => {
                return Some(format!(
                    "<file path=\"{path}\">\n[error reading file: {err}]\n</file>"
                ))
            }
        };
        if raw.len() > MAX_AT_FILE_BYTES {
            return Some(format!(
                "<file path=\"{path}\">\n[file too large to inline ({} bytes)]\n</file>",
                raw.len()
            ));
        }
        let text = String::from_utf8_lossy(&raw);
        Some(format!("<file path=\"{path}\">\n{text}\n</file>"))
    } else {
        Some(format!(
            "<file path=\"{path}\">\n[not found: '{path}']\n</file>"
        ))
    }
}

// ── Slash command filtering ────────────────────────────────────────────────────

pub fn filter_slash_commands(buf: &str) -> Vec<usize> {
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

// ── Text editing helpers ───────────────────────────────────────────────────────

/// Round `idx` down to the nearest UTF-8 char boundary.
pub fn char_floor(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn word_back(s: &str, mut pos: usize) -> usize {
    let b = s.as_bytes();
    while pos > 0 && b[pos - 1] == b' ' {
        pos -= 1;
    }
    while pos > 0 && b[pos - 1] != b' ' {
        pos -= 1;
    }
    pos
}

fn word_forward(s: &str, mut pos: usize) -> usize {
    let b = s.as_bytes();
    while pos < s.len() && b[pos] != b' ' {
        pos += 1;
    }
    while pos < s.len() && b[pos] == b' ' {
        pos += 1;
    }
    pos
}

// ── Popup helpers ──────────────────────────────────────────────────────────────

/// Clear `count` rows above the prompt bar.
pub fn clear_rows_above(count: u16) -> std::io::Result<()> {
    use crossterm::{cursor, execute, terminal};
    if count == 0 {
        return Ok(());
    }
    let (_, h) = crossterm::terminal::size()?;
    let top_row = h.saturating_sub(PROMPT_BAR_HEIGHT + 1);
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

/// Clear rows drawn below the splash box for the slash-command popup.
pub fn clear_splash_popup_rows(start_row: u16, count: u16) -> std::io::Result<()> {
    use crossterm::{cursor, execute, terminal};
    for row in start_row..start_row + count {
        execute!(
            std::io::stdout(),
            cursor::MoveTo(0, row),
            terminal::Clear(crossterm::terminal::ClearType::CurrentLine),
        )?;
    }
    Ok(())
}

/// Draw a file-completion popup above the prompt bar. Returns the number of rows drawn.
pub fn draw_file_popup(
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

/// Draw a bordered slash-command picker.
///
/// `splash_pos` — when `Some((col, top_row, box_w))` the popup is drawn below
/// the splash input box (splash mode); when `None` it is drawn above the prompt
/// bar at column 0 (REPL mode).
///
/// Returns the number of rows drawn (including border rows).
pub fn draw_slash_popup(
    matches: &[usize],
    selected: Option<usize>,
    splash_pos: Option<(u16, u16, u16)>,
) -> std::io::Result<u16> {
    use crossterm::{
        cursor, execute,
        style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    };
    if matches.is_empty() {
        if splash_pos.is_none() {
            clear_rows_above(0)?;
        }
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
    let content_w = 1 + 1 + name_w + 2 + desc_w + 1;

    let (col, top_row, inner_w) = match splash_pos {
        Some((c, r, bw)) => {
            clear_splash_popup_rows(r, MAX_POPUP_ROWS + 2)?;
            (c, r, bw.saturating_sub(2) as usize)
        }
        None => {
            let iw = content_w.min(w as usize - 2);
            let total = n as u16 + 2;
            let tr = h.saturating_sub(PROMPT_BAR_HEIGHT + total);
            clear_rows_above(MAX_POPUP_ROWS + 2)?;
            (0u16, tr, iw)
        }
    };

    let box_w = (inner_w + 2) as u16;
    let total_rows = n as u16 + 2;

    execute!(
        std::io::stdout(),
        cursor::MoveTo(col, top_row),
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
        let mut end = content.len().min(inner_w);
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        let padded = format!("{:<width$}", &content[..end], width = inner_w);

        execute!(
            std::io::stdout(),
            cursor::MoveTo(col, row),
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
            cursor::MoveTo(col + box_w - 1, row),
            SetForegroundColor(Color::DarkGrey),
            Print("│"),
            ResetColor,
        )?;
    }

    execute!(
        std::io::stdout(),
        cursor::MoveTo(col, top_row + n as u16 + 1),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("╰{}╯", "─".repeat(inner_w))),
        ResetColor,
    )?;

    std::io::stdout().flush()?;
    Ok(total_rows)
}

// ── Paste block ────────────────────────────────────────────────────────────────

/// A paste block: content is stored separately; `pos` is the byte offset in `buf`
/// where it logically sits, and `label` is shown inline (e.g. `[~45 lines pasted]`).
pub struct PasteBlock {
    pub pos: usize,
    pub content: String,
    pub label: String,
}

impl PasteBlock {
    pub fn new(pos: usize, content: String) -> Self {
        let label = {
            let lines = content.lines().count();
            if lines > 1 {
                format!("[~{} lines pasted]", lines)
            } else {
                format!("[~{} chars pasted]", content.chars().count())
            }
        };
        PasteBlock {
            pos,
            content,
            label,
        }
    }

    /// Adjust anchor after inserting `bytes` at `insert_pos` in buf.
    pub fn on_insert(&mut self, insert_pos: usize, bytes: usize) {
        if insert_pos <= self.pos {
            self.pos += bytes;
        }
    }

    /// Adjust anchor after removing `buf[remove_start..remove_end]`.
    /// Returns false if the removal covers the paste anchor (caller should clear it).
    pub fn on_remove(&mut self, remove_start: usize, remove_end: usize) -> bool {
        if remove_start <= self.pos && self.pos <= remove_end {
            return false;
        }
        if remove_end <= self.pos {
            self.pos -= remove_end - remove_start;
        }
        true
    }
}

/// Build the display string and display-cursor for the prompt bar,
/// inserting the paste label at its anchor position.
pub fn paste_display(buf: &str, cursor: usize, pb: Option<&PasteBlock>) -> (String, usize) {
    match pb {
        None => (buf.to_string(), cursor),
        Some(p) => {
            let display = format!("{}{}{}", &buf[..p.pos], p.label, &buf[p.pos..]);
            let dcursor = if cursor >= p.pos {
                p.pos + p.label.len() + (cursor - p.pos)
            } else {
                cursor
            };
            (display, dcursor)
        }
    }
}

// ── REPL readline ──────────────────────────────────────────────────────────────

/// Read a line of input in TUI mode. Supports /slash completions, @file mentions,
/// paste blocks, cursor movement, and Tab agent cycling.
#[allow(clippy::too_many_arguments)]
pub fn tui_read_line(
    full_agents: &[io_agents::AgentConfig],
    tab_current: &mut usize,
    agent: &io_runtime::Agent,
    last_input_tokens: u32,
    context_window: u64,
    current_agent_id: &str,
    theme: &io_tui::render::Theme,
    line_buf: &LineBuf,
) -> anyhow::Result<Option<String>> {
    use crossterm::event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers, MouseEventKind,
    };

    let mut buf = String::new();
    let mut cursor: usize = 0;
    let mut file_all: Option<Vec<String>> = None;
    let mut file_filtered: Vec<String> = vec![];
    let mut file_selected: Option<usize> = None;
    let mut file_scroll: usize = 0;

    let mut slash_matches: Vec<usize> = Vec::new();
    let mut slash_selected: Option<usize> = None;
    let mut popup_rows: u16 = 0;
    let mut scroll_offset: usize = 0;

    let bar = |input: &str,
               cursor_byte: usize,
               tc: usize,
               ps: Option<&PasteBlock>|
     -> std::io::Result<u16> {
        let name = full_agents
            .get(tc)
            .map(|a| a.name)
            .unwrap_or(current_agent_id);
        let (display, dcursor) = paste_display(input, cursor_byte, ps);
        let h = draw_prompt_bar(
            &display,
            name,
            agent.provider_id,
            &agent.model_id,
            last_input_tokens,
            context_window,
            theme,
        )?;
        handle_resize_with_height(h)?;
        io_tui::render::move_prompt_cursor(&display, dcursor)?;
        Ok(h)
    };

    let mut paste_block: Option<PasteBlock> = None;

    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    let mut current_prompt_height = bar("", 0, *tab_current, None)?;
    crossterm::execute!(std::io::stdout(), crossterm::cursor::Show)?;

    loop {
        let ev = event::read()?;
        match ev {
            Event::Resize(_, _) => {
                handle_resize_with_height(current_prompt_height)?;
                if file_all.is_some() {
                    popup_rows = draw_file_popup(&file_filtered, file_selected, file_scroll)?;
                } else if !slash_matches.is_empty() {
                    popup_rows = draw_slash_popup(&slash_matches, slash_selected, None)?;
                } else {
                    popup_rows = 0;
                    render_scroll_view(
                        &line_buf.lock().unwrap(),
                        scroll_offset,
                        theme,
                        current_prompt_height,
                    )?;
                }
                current_prompt_height = bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
            }
            Event::Paste(text) => {
                let content: String = text.chars().filter(|&c| c != '\r').collect();
                if !content.trim().is_empty() {
                    paste_block = Some(PasteBlock::new(cursor, content));
                    slash_matches.clear();
                    slash_selected = None;
                    file_all = None;
                    file_filtered.clear();
                    file_selected = None;
                    file_scroll = 0;
                    clear_rows_above(popup_rows)?;
                    popup_rows = 0;
                    current_prompt_height = bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                }
            }
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        let n = line_buf.lock().unwrap().len();
                        scroll_offset = scroll_offset.saturating_add(SCROLL_STEP).min(n);
                        render_scroll_view(
                            &line_buf.lock().unwrap(),
                            scroll_offset,
                            theme,
                            current_prompt_height,
                        )?;
                        crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide)?;
                    }
                    MouseEventKind::ScrollDown if scroll_offset > 0 => {
                        scroll_offset = scroll_offset.saturating_sub(SCROLL_STEP);
                        render_scroll_view(
                            &line_buf.lock().unwrap(),
                            scroll_offset,
                            theme,
                            current_prompt_height,
                        )?;
                        if scroll_offset == 0 {
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
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
                if scroll_offset > 0 {
                    scroll_offset = 0;
                    render_scroll_view(
                        &line_buf.lock().unwrap(),
                        scroll_offset,
                        theme,
                        current_prompt_height,
                    )?;
                    current_prompt_height = bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    crossterm::execute!(std::io::stdout(), crossterm::cursor::Show)?;
                }
                match (key.code, key.modifiers) {
                    (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        clear_rows_above(popup_rows)?;
                        crossterm::execute!(
                            std::io::stdout(),
                            DisableBracketedPaste,
                            crossterm::cursor::Hide
                        )?;
                        return Ok(None);
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        // Raw mode suppresses the terminal's own SIGINT, so
                        // this is the only interrupt signal the app gets:
                        // first press clears the line (mirrors a shell),
                        // second press on an already-empty line exits —
                        // giving Ctrl+C a way back to the terminal like
                        // Ctrl+D, without eating a single accidental press.
                        let was_empty = buf.is_empty() && paste_block.is_none();
                        clear_rows_above(popup_rows)?;
                        crossterm::execute!(std::io::stdout(), DisableBracketedPaste)?;
                        if was_empty {
                            crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide)?;
                            return Ok(None);
                        }
                        let _ = bar("", 0, *tab_current, None)?;
                        return Ok(Some(String::new()));
                    }

                    (KeyCode::Enter, _) => {
                        if file_all.is_some() {
                            if let Some(s) = file_selected {
                                if s < file_filtered.len() {
                                    if let Some((at_pos, _)) = io_tui::readline::at_prefix(&buf) {
                                        buf.truncate(at_pos);
                                        buf.push('@');
                                        buf.push_str(&file_filtered[s]);
                                        cursor = buf.len();
                                        file_all = None;
                                        file_filtered.clear();
                                        file_selected = None;
                                        file_scroll = 0;
                                        popup_rows = 0;
                                        current_prompt_height =
                                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                                        continue;
                                    }
                                }
                            }
                        }
                        if let Some(s) = slash_selected {
                            if s < slash_matches.len() {
                                buf = SLASH_COMMANDS[slash_matches[s]].0.to_string();
                                cursor = buf.len();
                                paste_block = None;
                            }
                        }
                        let final_msg = if let Some(ref pb) = paste_block {
                            format!("{}{}{}", &buf[..pb.pos], pb.content, &buf[pb.pos..])
                        } else {
                            buf.clone()
                        };
                        clear_rows_above(popup_rows)?;
                        crossterm::execute!(std::io::stdout(), DisableBracketedPaste)?;
                        let _ = bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                        crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide)?;
                        return Ok(Some(final_msg));
                    }

                    (KeyCode::Backspace, KeyModifiers::ALT) if cursor > 0 => {
                        let start = word_back(&buf, cursor);
                        buf.drain(start..cursor);
                        if let Some(ref mut pb) = paste_block {
                            if !pb.on_remove(start, cursor) {
                                paste_block = None;
                            }
                        }
                        cursor = start;
                        let mut new_rows: u16 = 0;
                        slash_matches = filter_slash_commands(&buf);
                        slash_selected = None;
                        if !slash_matches.is_empty() {
                            new_rows = draw_slash_popup(&slash_matches, slash_selected, None)?;
                        } else {
                            clear_rows_above(popup_rows)?;
                        }
                        popup_rows = new_rows;
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }

                    (KeyCode::Backspace, _) => {
                        if cursor > 0 {
                            let prev = char_floor(&buf, cursor - 1);
                            buf.remove(prev);
                            if let Some(ref mut pb) = paste_block {
                                if !pb.on_remove(prev, cursor) {
                                    paste_block = None;
                                }
                            }
                            cursor = prev;
                        }
                        let mut new_rows: u16 = 0;
                        if let Some(ref all) = file_all {
                            let prefix = io_tui::readline::at_prefix(&buf)
                                .map(|(_, p)| p)
                                .unwrap_or("");
                            file_filtered = io_tui::readline::filter_files(all, prefix);
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
                                new_rows = draw_slash_popup(&slash_matches, slash_selected, None)?;
                            } else {
                                clear_rows_above(popup_rows)?;
                            }
                        }
                        popup_rows = new_rows;
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
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
                                paste_block = None;
                                clear_rows_above(popup_rows)?;
                                popup_rows = 0;
                                bar("", 0, *tab_current, None)?;
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
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                        } else if !slash_matches.is_empty() {
                            slash_selected = Some(match slash_selected {
                                None => 0,
                                Some(s) => (s + 1) % slash_matches.len(),
                            });
                            popup_rows = draw_slash_popup(&slash_matches, slash_selected, None)?;
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
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
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
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
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                        } else if !slash_matches.is_empty() {
                            slash_selected = Some(match slash_selected {
                                None => 0,
                                Some(s) => (s + 1) % slash_matches.len(),
                            });
                            popup_rows = draw_slash_popup(&slash_matches, slash_selected, None)?;
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
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
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                        } else if !slash_matches.is_empty() {
                            let prev = match slash_selected {
                                None | Some(0) => slash_matches.len() - 1,
                                Some(s) => s - 1,
                            };
                            slash_selected = Some(prev);
                            popup_rows = draw_slash_popup(&slash_matches, slash_selected, None)?;
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                        }
                    }

                    // ── Cursor movement ──────────────────────────────────────
                    (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Left, KeyModifiers::ALT) => {
                        cursor = word_back(&buf, cursor);
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::Right, KeyModifiers::CONTROL)
                    | (KeyCode::Right, KeyModifiers::ALT) => {
                        cursor = word_forward(&buf, cursor);
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::Left, _) if cursor > 0 => {
                        cursor = char_floor(&buf, cursor - 1);
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::Right, _) if cursor < buf.len() => {
                        cursor += buf[cursor..].chars().next().map_or(0, |c| c.len_utf8());
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::Home, _) => {
                        cursor = 0;
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::End, _) => {
                        cursor = buf.len();
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::Delete, _) => {
                        if paste_block
                            .as_ref()
                            .map(|p| p.pos == cursor)
                            .unwrap_or(false)
                        {
                            paste_block = None;
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                        } else if cursor < buf.len() {
                            let next =
                                cursor + buf[cursor..].chars().next().map_or(0, |c| c.len_utf8());
                            buf.drain(cursor..next);
                            if let Some(ref mut pb) = paste_block {
                                if !pb.on_remove(cursor, next) {
                                    paste_block = None;
                                }
                            }
                            let mut new_rows: u16 = 0;
                            slash_matches = filter_slash_commands(&buf);
                            slash_selected = None;
                            if !slash_matches.is_empty() {
                                new_rows = draw_slash_popup(&slash_matches, slash_selected, None)?;
                            } else {
                                clear_rows_above(popup_rows)?;
                            }
                            popup_rows = new_rows;
                            current_prompt_height =
                                bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                        }
                    }
                    (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                        cursor = 0;
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                        cursor = buf.len();
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                        let old_len = buf.len();
                        buf.truncate(cursor);
                        if paste_block
                            .as_ref()
                            .map(|p| p.pos >= cursor && p.pos <= old_len)
                            .unwrap_or(false)
                        {
                            paste_block = None;
                        }
                        file_all = None;
                        file_filtered.clear();
                        file_selected = None;
                        file_scroll = 0;
                        slash_matches.clear();
                        slash_selected = None;
                        clear_rows_above(popup_rows)?;
                        popup_rows = 0;
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }
                    (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                        if paste_block
                            .as_ref()
                            .map(|p| p.pos <= cursor)
                            .unwrap_or(false)
                        {
                            paste_block = None;
                        } else if let Some(ref mut pb) = paste_block {
                            pb.pos = pb.pos.saturating_sub(cursor);
                        }
                        buf.drain(..cursor);
                        cursor = 0;
                        file_all = None;
                        file_filtered.clear();
                        file_selected = None;
                        file_scroll = 0;
                        slash_matches.clear();
                        slash_selected = None;
                        clear_rows_above(popup_rows)?;
                        popup_rows = 0;
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }

                    // ── Character insertion ──────────────────────────────────
                    (KeyCode::Char('@'), _) => {
                        buf.insert(cursor, '@');
                        if let Some(ref mut pb) = paste_block {
                            pb.on_insert(cursor, '@'.len_utf8());
                        }
                        cursor += '@'.len_utf8();
                        let all = io_tui::readline::list_files();
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
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }

                    (KeyCode::Char(' '), _) if file_all.is_some() => {
                        buf.insert(cursor, ' ');
                        if let Some(ref mut pb) = paste_block {
                            pb.on_insert(cursor, ' '.len_utf8());
                        }
                        cursor += ' '.len_utf8();
                        file_all = None;
                        file_filtered.clear();
                        file_selected = None;
                        file_scroll = 0;
                        slash_matches.clear();
                        slash_selected = None;
                        clear_rows_above(popup_rows)?;
                        popup_rows = 0;
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }

                    (KeyCode::Char(c), _) => {
                        buf.insert(cursor, c);
                        if let Some(ref mut pb) = paste_block {
                            pb.on_insert(cursor, c.len_utf8());
                        }
                        cursor += c.len_utf8();
                        if file_all.is_some() {
                            if let Some((_, prefix)) = io_tui::readline::at_prefix(&buf) {
                                if let Some(all) = &file_all {
                                    file_filtered = io_tui::readline::filter_files(all, prefix);
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
                                popup_rows =
                                    draw_slash_popup(&slash_matches, slash_selected, None)?;
                            }
                        }
                        current_prompt_height =
                            bar(&buf, cursor, *tab_current, paste_block.as_ref())?;
                    }

                    _ => {}
                }
            }
            _ => {}
        }
    }
}

// ── Splash readline ────────────────────────────────────────────────────────────

/// Read the first message from the centered splash screen.
/// Returns `None` on Ctrl+D (exit), `Some(text)` on Enter with non-empty input.
pub fn splash_read_line(
    full_agents: &[io_agents::AgentConfig],
    tab_current: &mut usize,
    agent: &io_runtime::Agent,
    theme: &io_tui::render::Theme,
) -> anyhow::Result<Option<String>> {
    use crossterm::{
        cursor,
        event::{
            self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
            KeyModifiers,
        },
        execute,
    };
    crossterm::terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), EnableBracketedPaste)?;

    let mut buf = String::new();
    let mut cursor: usize = 0;
    let mut slash_matches: Vec<usize> = Vec::new();
    let mut slash_selected: Option<usize> = None;
    let mut popup_rows: u16 = 0;

    let splash_name =
        |tc: usize| -> &str { full_agents.get(tc).map(|a| a.name).unwrap_or("build") };

    let mut layout = io_tui::render::draw_splash(
        &buf,
        splash_name(*tab_current),
        agent.provider_id,
        &agent.model_id,
        theme,
    )?;
    let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
    execute!(std::io::stdout(), cursor::MoveTo(cx, cy), cursor::Show)?;

    let popup_pos = |l: &io_tui::render::SplashLayout| Some((l.box_x, l.status_row + 2, l.box_w));
    let clear_popup = |l: &io_tui::render::SplashLayout, rows: u16| {
        clear_splash_popup_rows(l.status_row + 2, rows)
    };

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
                    popup_rows =
                        draw_slash_popup(&slash_matches, slash_selected, popup_pos(&layout))?;
                }
                let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                execute!(std::io::stdout(), cursor::MoveTo(cx, cy), cursor::Show)?;
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                match (key.code, key.modifiers) {
                    (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        clear_popup(&layout, popup_rows)?;
                        execute!(std::io::stdout(), DisableBracketedPaste, cursor::Hide)?;
                        return Ok(None);
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        // Same first-clears/second-exits pattern as the REPL
                        // prompt — raw mode suppresses the terminal's own
                        // SIGINT, so this is the app's only Ctrl+C handling.
                        if buf.is_empty() {
                            clear_popup(&layout, popup_rows)?;
                            execute!(std::io::stdout(), DisableBracketedPaste, cursor::Hide)?;
                            return Ok(None);
                        }
                        buf.clear();
                        cursor = 0;
                        slash_matches.clear();
                        slash_selected = None;
                        clear_popup(&layout, popup_rows)?;
                        popup_rows = 0;
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Enter, _) => {
                        if let Some(s) = slash_selected {
                            if s < slash_matches.len() {
                                buf = SLASH_COMMANDS[slash_matches[s]].0.to_string();
                                cursor = buf.len();
                            }
                        }
                        clear_popup(&layout, popup_rows)?;
                        if !buf.trim().is_empty() {
                            execute!(std::io::stdout(), DisableBracketedPaste, cursor::Hide)?;
                            return Ok(Some(buf));
                        }
                    }
                    (KeyCode::Esc, _) if !slash_matches.is_empty() => {
                        slash_matches.clear();
                        slash_selected = None;
                        clear_popup(&layout, popup_rows)?;
                        popup_rows = 0;
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Backspace, KeyModifiers::ALT) if cursor > 0 => {
                        let start = word_back(&buf, cursor);
                        buf.drain(start..cursor);
                        cursor = start;
                        slash_matches = filter_slash_commands(&buf);
                        slash_selected = None;
                        if !slash_matches.is_empty() {
                            popup_rows = draw_slash_popup(
                                &slash_matches,
                                slash_selected,
                                popup_pos(&layout),
                            )?;
                        } else {
                            clear_popup(&layout, popup_rows)?;
                            popup_rows = 0;
                        }
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Backspace, _) => {
                        if cursor > 0 {
                            let prev = char_floor(&buf, cursor - 1);
                            buf.remove(prev);
                            cursor = prev;
                        }
                        slash_matches = filter_slash_commands(&buf);
                        slash_selected = None;
                        if !slash_matches.is_empty() {
                            popup_rows = draw_slash_popup(
                                &slash_matches,
                                slash_selected,
                                popup_pos(&layout),
                            )?;
                        } else {
                            clear_popup(&layout, popup_rows)?;
                            popup_rows = 0;
                        }
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Tab, _) | (KeyCode::Down, _) if !slash_matches.is_empty() => {
                        slash_selected = Some(match slash_selected {
                            None => 0,
                            Some(s) => (s + 1) % slash_matches.len(),
                        });
                        popup_rows =
                            draw_slash_popup(&slash_matches, slash_selected, popup_pos(&layout))?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Up, _) if !slash_matches.is_empty() => {
                        slash_selected = Some(match slash_selected {
                            None | Some(0) => slash_matches.len() - 1,
                            Some(s) => s - 1,
                        });
                        popup_rows =
                            draw_slash_popup(&slash_matches, slash_selected, popup_pos(&layout))?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
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
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    // ── Cursor movement ──────────────────────────────────────
                    (KeyCode::Left, KeyModifiers::CONTROL) | (KeyCode::Left, KeyModifiers::ALT) => {
                        cursor = word_back(&buf, cursor);
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Right, KeyModifiers::CONTROL)
                    | (KeyCode::Right, KeyModifiers::ALT) => {
                        cursor = word_forward(&buf, cursor);
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Left, _) if cursor > 0 => {
                        cursor = char_floor(&buf, cursor - 1);
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Right, _) if cursor < buf.len() => {
                        cursor += buf[cursor..].chars().next().map_or(0, |c| c.len_utf8());
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Home, _) => {
                        cursor = 0;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::End, _) => {
                        cursor = buf.len();
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Delete, _) if cursor < buf.len() => {
                        let next =
                            cursor + buf[cursor..].chars().next().map_or(0, |c| c.len_utf8());
                        buf.drain(cursor..next);
                        slash_matches = filter_slash_commands(&buf);
                        slash_selected = None;
                        if !slash_matches.is_empty() {
                            popup_rows = draw_slash_popup(
                                &slash_matches,
                                slash_selected,
                                popup_pos(&layout),
                            )?;
                        } else {
                            clear_popup(&layout, popup_rows)?;
                            popup_rows = 0;
                        }
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                        cursor = 0;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                        cursor = buf.len();
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    // ── Character insertion ──────────────────────────────────
                    (KeyCode::Char(c), _) => {
                        buf.insert(cursor, c);
                        cursor += c.len_utf8();
                        slash_matches = filter_slash_commands(&buf);
                        slash_selected = None;
                        if !slash_matches.is_empty() {
                            popup_rows = draw_slash_popup(
                                &slash_matches,
                                slash_selected,
                                popup_pos(&layout),
                            )?;
                        } else {
                            clear_popup(&layout, popup_rows)?;
                            popup_rows = 0;
                        }
                        io_tui::render::splash_update_input(&layout, &buf, theme)?;
                        let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                        execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                let clean: String = text.chars().filter(|&c| c != '\r' && c != '\n').collect();
                buf.insert_str(cursor, &clean);
                cursor += clean.len();
                slash_matches = filter_slash_commands(&buf);
                slash_selected = None;
                if !slash_matches.is_empty() {
                    popup_rows =
                        draw_slash_popup(&slash_matches, slash_selected, popup_pos(&layout))?;
                } else {
                    clear_popup(&layout, popup_rows)?;
                    popup_rows = 0;
                }
                io_tui::render::splash_update_input(&layout, &buf, theme)?;
                let (cx, cy) = io_tui::render::splash_cursor(&layout, &buf, cursor);
                execute!(std::io::stdout(), cursor::MoveTo(cx, cy))?;
            }
            _ => {}
        }
    }
}
