#![allow(dead_code)]

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    queue,
    style::{Color, Print, ResetColor, SetForegroundColor, Stylize},
    terminal::{self, ClearType},
};
use std::io::{self, Write};

struct Cmd {
    name: &'static str,
    desc: &'static str,
}

const COMMANDS: &[Cmd] = &[
    Cmd {
        name: "/help",
        desc: "Show available commands",
    },
    Cmd {
        name: "/agent",
        desc: "Switch agent mode",
    },
    Cmd {
        name: "/connect",
        desc: "Set up a provider",
    },
    Cmd {
        name: "/login",
        desc: "Sign in with OAuth (ChatGPT / Claude)",
    },
    Cmd {
        name: "/model",
        desc: "Switch model",
    },
    Cmd {
        name: "/cost",
        desc: "Show API cost for current session",
    },
    Cmd {
        name: "/compact",
        desc: "Summarize and compress conversation history",
    },
    Cmd {
        name: "/exit",
        desc: "Exit",
    },
    Cmd {
        name: "/quit",
        desc: "Exit",
    },
    Cmd {
        name: "/q",
        desc: "Exit",
    },
];

// Column index where user input starts — must match the width of "> "
pub(crate) const PROMPT_COL: u16 = 2;

/// Context passed into read_line for in-place Tab cycling.
pub struct ReadLineCtx {
    pub tab_statuses: Vec<String>,
    pub tab_current: usize,
}

/// Result returned by read_line.
pub struct ReadLineOutput {
    pub text: String,
    pub agent_idx: usize,
}

/// Read a line from stdin with inline slash-command completion, `@`-file mention,
/// and Tab agent cycling.
pub fn read_line(ctx: ReadLineCtx) -> anyhow::Result<Option<ReadLineOutput>> {
    let mut stdout = io::stdout();

    // Compute popup height from the terminal — leave 6 rows for status/prompt/breathing room.
    let popup_capacity = {
        let (_, rows) = terminal::size().unwrap_or((80, 24));
        (rows as usize).saturating_sub(6).max(4)
    };
    let reserve = popup_capacity.max(COMMANDS.len());

    for _ in 0..reserve {
        queue!(stdout, Print("\n"))?;
    }
    queue!(stdout, cursor::MoveUp(reserve as u16))?;
    stdout.flush()?;

    terminal::enable_raw_mode()?;
    let result = input_loop(&mut stdout, ctx, popup_capacity);
    let _ = terminal::disable_raw_mode();
    result
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn filter_matches(buf: &str) -> Vec<usize> {
    if buf.is_empty() || !buf.starts_with('/') {
        return vec![];
    }
    let lower = buf.to_ascii_lowercase();
    COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, c)| c.name.starts_with(lower.as_str()))
        .map(|(i, _)| i)
        .collect()
}

/// Returns `(at_byte_pos, typed_prefix)` for the last active `@`-mention.
pub fn at_prefix(buf: &str) -> Option<(usize, &str)> {
    let at_pos = buf.rfind('@')?;
    if at_pos > 0 && !buf[..at_pos].ends_with(char::is_whitespace) {
        return None;
    }
    Some((at_pos, &buf[at_pos + 1..]))
}

/// Tries `fd`, then `rg --files`, then `git ls-files`, then stdlib single-level walk.
pub fn list_files() -> Vec<String> {
    list_files_fd()
        .or_else(list_files_rg)
        .or_else(list_files_git)
        .unwrap_or_else(list_files_stdlib)
}

fn sort_entries(files: &mut [String]) {
    files.sort_by(|a, b| b.ends_with('/').cmp(&a.ends_with('/')).then(a.cmp(b)));
}

fn list_files_fd() -> Option<Vec<String>> {
    let dirs = std::process::Command::new("fd")
        .args(["--type", "d", "--strip-cwd-prefix", "--color", "never"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let files = std::process::Command::new("fd")
        .args(["--type", "f", "--strip-cwd-prefix", "--color", "never"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;

    let mut results = Vec::new();
    for line in String::from_utf8_lossy(&dirs.stdout).lines() {
        if !line.is_empty() {
            results.push(format!("{}/", line.trim_end_matches('/')));
        }
    }
    for line in String::from_utf8_lossy(&files.stdout).lines() {
        if !line.is_empty() {
            results.push(line.to_string());
        }
    }
    sort_entries(&mut results);
    Some(results)
}

fn list_files_rg() -> Option<Vec<String>> {
    let out = std::process::Command::new("rg")
        .args(["--files", "--color", "never"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;

    let mut results: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    if let Ok(entries) = std::fs::read_dir(".") {
        for e in entries.filter_map(|e| e.ok()) {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Ok(name) = e.file_name().into_string() {
                    results.push(format!("{name}/"));
                }
            }
        }
    }
    sort_entries(&mut results);
    Some(results)
}

/// `git ls-files`: tracked + untracked non-ignored files, gitignore-aware.
/// Directories are reconstructed from the file paths since git only lists files.
fn list_files_git() -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;

    let text = String::from_utf8_lossy(&out.stdout);
    let mut results: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    // Derive all ancestor directories from the file paths.
    let mut dirs = std::collections::HashSet::new();
    for path in &results {
        let mut p = std::path::Path::new(path);
        while let Some(parent) = p.parent() {
            if parent == std::path::Path::new("") {
                break;
            }
            dirs.insert(format!("{}/", parent.display()));
            p = parent;
        }
    }
    results.extend(dirs);
    sort_entries(&mut results);
    Some(results)
}

fn list_files_stdlib() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(".") else {
        return vec![];
    };
    let mut files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some(if is_dir { format!("{name}/") } else { name })
        })
        .collect();
    sort_entries(&mut files);
    files
}

pub fn filter_files(all: &[String], prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return all.to_vec();
    }
    let lower = prefix.to_ascii_lowercase();
    all.iter()
        .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
        .cloned()
        .collect()
}

/// Print `buf` with any `@token` spans rendered in cyan.
fn render_buf(stdout: &mut impl Write, buf: &str) -> io::Result<()> {
    let mut rest = buf;
    while !rest.is_empty() {
        // Find the next @ that is at the start or preceded by whitespace.
        let at_pos = rest
            .char_indices()
            .find(|&(i, c)| c == '@' && (i == 0 || rest[..i].ends_with(char::is_whitespace)))
            .map(|(i, _)| i);

        match at_pos {
            None => {
                queue!(stdout, Print(rest))?;
                break;
            }
            Some(i) => {
                if i > 0 {
                    queue!(stdout, Print(&rest[..i]))?;
                }
                let after = &rest[i + 1..];
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                queue!(
                    stdout,
                    SetForegroundColor(Color::Cyan),
                    Print(format!("@{}", &after[..end])),
                    ResetColor,
                )?;
                rest = &after[end..];
            }
        }
    }
    Ok(())
}

// ── rendering ────────────────────────────────────────────────────────────────

/// Redraw the input line and popup.
///
/// `file_state = Some((items, selected_abs, scroll))` for @ mode;
/// `None` for / command mode.
fn redraw(
    stdout: &mut impl Write,
    buf: &str,
    slash_selected: Option<usize>,
    old_popup: usize,
    file_state: Option<(&[String], Option<usize>, usize)>,
    popup_capacity: usize,
) -> io::Result<usize> {
    queue!(
        stdout,
        cursor::MoveToColumn(PROMPT_COL),
        terminal::Clear(ClearType::UntilNewLine)
    )?;
    render_buf(stdout, buf)?;

    let input_end_col = PROMPT_COL + buf.len() as u16;
    let new_popup;

    match file_state {
        Some((items, selected_abs, scroll)) => {
            // Determine how many item rows fit, reserving space for scroll indicators.
            let has_above = scroll > 0;
            let mut item_rows = popup_capacity.saturating_sub(usize::from(has_above));
            let has_below = scroll + item_rows < items.len();
            if has_below {
                item_rows = item_rows.saturating_sub(1);
            }

            let window = &items[scroll..(scroll + item_rows).min(items.len())];
            new_popup = usize::from(has_above) + window.len() + usize::from(has_below);
            let max_lines = old_popup.max(new_popup);

            if max_lines > 0 {
                queue!(stdout, cursor::MoveDown(1), cursor::MoveToColumn(0))?;
                let mut render_idx = 0usize; // which logical popup row we're on

                for i in 0..max_lines {
                    queue!(stdout, terminal::Clear(ClearType::CurrentLine))?;

                    if render_idx == 0 && has_above {
                        queue!(
                            stdout,
                            SetForegroundColor(Color::DarkGrey),
                            Print(format!("  \u{2191} {} above", scroll)),
                            ResetColor,
                        )?;
                        render_idx += 1;
                    } else {
                        let item_i = render_idx - usize::from(has_above);
                        if item_i < window.len() {
                            let item = &window[item_i];
                            let abs_idx = scroll + item_i;
                            let is_dir = item.ends_with('/');
                            if selected_abs == Some(abs_idx) {
                                queue!(
                                    stdout,
                                    SetForegroundColor(Color::Cyan),
                                    Print("\u{25b6} "), // ▶
                                    ResetColor,
                                    Print(item),
                                )?;
                            } else if is_dir {
                                queue!(
                                    stdout,
                                    SetForegroundColor(Color::Cyan),
                                    Print(format!("  {item}")),
                                    ResetColor,
                                )?;
                            } else {
                                queue!(
                                    stdout,
                                    SetForegroundColor(Color::DarkGrey),
                                    Print(format!("  {item}")),
                                    ResetColor,
                                )?;
                            }
                            render_idx += 1;
                        } else if has_below && item_i == window.len() {
                            let remaining = items.len().saturating_sub(scroll + window.len());
                            queue!(
                                stdout,
                                SetForegroundColor(Color::DarkGrey),
                                Print(format!("  \u{2193} {} below", remaining)),
                                ResetColor,
                            )?;
                            render_idx += 1;
                        }
                    }

                    if i + 1 < max_lines {
                        queue!(stdout, cursor::MoveToNextLine(1))?;
                    }
                }
                queue!(
                    stdout,
                    cursor::MoveUp(max_lines as u16),
                    cursor::MoveToColumn(input_end_col),
                )?;
            }
        }
        None => {
            let matches = filter_matches(buf);
            new_popup = matches.len();
            let max_lines = old_popup.max(new_popup);

            if max_lines > 0 {
                let name_w = matches
                    .iter()
                    .map(|&i| COMMANDS[i].name.len())
                    .max()
                    .unwrap_or(0);
                queue!(stdout, cursor::MoveDown(1), cursor::MoveToColumn(0))?;
                for i in 0..max_lines {
                    queue!(stdout, terminal::Clear(ClearType::CurrentLine))?;
                    if i < new_popup {
                        let cmd = &COMMANDS[matches[i]];
                        let pad = " ".repeat(name_w - cmd.name.len() + 2);
                        if slash_selected == Some(i) {
                            queue!(
                                stdout,
                                crossterm::style::SetBackgroundColor(Color::DarkGrey),
                                SetForegroundColor(Color::White),
                                Print(format!("  {}{}{}  ", cmd.name, pad, cmd.desc)),
                                ResetColor,
                            )?;
                        } else {
                            queue!(
                                stdout,
                                SetForegroundColor(Color::DarkGrey),
                                Print(format!("  {}{}{}  ", cmd.name, pad, cmd.desc)),
                                ResetColor,
                            )?;
                        }
                    }
                    if i + 1 < max_lines {
                        queue!(stdout, cursor::MoveToNextLine(1))?;
                    }
                }
                queue!(
                    stdout,
                    cursor::MoveUp(max_lines as u16),
                    cursor::MoveToColumn(input_end_col),
                )?;
            }
        }
    }

    stdout.flush()?;
    Ok(new_popup)
}

fn clear_popup(stdout: &mut impl Write, popup_lines: usize) -> io::Result<()> {
    if popup_lines == 0 {
        return Ok(());
    }
    queue!(stdout, cursor::MoveDown(1), cursor::MoveToColumn(0))?;
    for i in 0..popup_lines {
        queue!(stdout, terminal::Clear(ClearType::CurrentLine))?;
        if i + 1 < popup_lines {
            queue!(stdout, cursor::MoveToNextLine(1))?;
        }
    }
    queue!(stdout, cursor::MoveUp(popup_lines as u16))?;
    Ok(())
}

fn redraw_status(stdout: &mut impl Write, status: &str) -> io::Result<()> {
    queue!(
        stdout,
        cursor::MoveToColumn(0),
        cursor::MoveUp(2),
        terminal::Clear(ClearType::CurrentLine),
        crossterm::style::PrintStyledContent(status.dark_grey()),
        cursor::MoveDown(2),
        cursor::MoveToColumn(PROMPT_COL),
    )?;
    stdout.flush()?;
    Ok(())
}

// ── scroll helpers ────────────────────────────────────────────────────────────

/// After moving `selected`, adjust `scroll` to keep the selection in view.
fn adjust_scroll(selected: usize, scroll: &mut usize, popup_capacity: usize) {
    // Account for indicator rows: each can steal 1 slot from the item window.
    let effective = popup_capacity.saturating_sub(2);
    if selected < *scroll {
        *scroll = selected;
    } else if selected >= *scroll + effective {
        *scroll = selected + 1 - effective;
    }
}

// ── main loop ─────────────────────────────────────────────────────────────────

fn input_loop(
    stdout: &mut impl Write,
    ctx: ReadLineCtx,
    popup_capacity: usize,
) -> anyhow::Result<Option<ReadLineOutput>> {
    let ReadLineCtx {
        tab_statuses,
        mut tab_current,
    } = ctx;

    let mut buf = String::new();
    let mut slash_selected: Option<usize> = None;
    let mut file_all: Option<Vec<String>> = None;
    let mut file_filtered: Vec<String> = vec![];
    let mut file_selected: Option<usize> = None; // absolute index
    let mut file_scroll: usize = 0;

    let mut popup_lines = redraw(stdout, &buf, slash_selected, 0, None, popup_capacity)?;

    loop {
        let ev = event::read()?;
        if let Event::Mouse(mouse) = &ev {
            if file_all.is_some() && !file_filtered.is_empty() {
                let new_sel = match mouse.kind {
                    MouseEventKind::ScrollDown => file_selected
                        .map(|s| (s + 1) % file_filtered.len())
                        .unwrap_or(0),
                    MouseEventKind::ScrollUp => match file_selected {
                        None | Some(0) => file_filtered.len() - 1,
                        Some(s) => s - 1,
                    },
                    _ => continue,
                };
                file_selected = Some(new_sel);
                adjust_scroll(new_sel, &mut file_scroll, popup_capacity);
                let fs = Some((file_filtered.as_slice(), file_selected, file_scroll));
                popup_lines = redraw(
                    stdout,
                    &buf,
                    slash_selected,
                    popup_lines,
                    fs,
                    popup_capacity,
                )?;
            }
            continue;
        }
        let Event::Key(key) = ev else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        match (key.code, key.modifiers) {
            // ── exit signals ─────────────────────────────────────────────
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                clear_popup(stdout, popup_lines)?;
                queue!(stdout, Print("\n"))?;
                stdout.flush()?;
                return Ok(None);
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                clear_popup(stdout, popup_lines)?;
                queue!(stdout, Print("^C\n"))?;
                stdout.flush()?;
                return Ok(Some(ReadLineOutput {
                    text: String::new(),
                    agent_idx: tab_current,
                }));
            }

            // ── confirm ──────────────────────────────────────────────────
            (KeyCode::Enter, _) => {
                if file_all.is_some() {
                    if let Some(s) = file_selected {
                        if s < file_filtered.len() {
                            if let Some((at_pos, _)) = at_prefix(&buf) {
                                buf.truncate(at_pos);
                                buf.push('@');
                                buf.push_str(&file_filtered[s]);
                                file_all.take();
                                file_filtered.clear();
                                file_selected = None;
                                file_scroll = 0;
                                popup_lines = redraw(
                                    stdout,
                                    &buf,
                                    slash_selected,
                                    popup_lines,
                                    None,
                                    popup_capacity,
                                )?;
                                continue;
                            }
                        }
                    }
                    // No selection — fall through to submit.
                }

                let matches = filter_matches(&buf);
                if let Some(s) = slash_selected {
                    if s < matches.len() {
                        buf = COMMANDS[matches[s]].name.to_string();
                    }
                }
                clear_popup(stdout, popup_lines)?;
                queue!(stdout, Print("\n"))?;
                stdout.flush()?;
                return Ok(Some(ReadLineOutput {
                    text: buf,
                    agent_idx: tab_current,
                }));
            }

            // ── editing ──────────────────────────────────────────────────
            (KeyCode::Backspace, _) => {
                buf.pop();
                slash_selected = None;
                match at_prefix(&buf) {
                    Some((_, prefix)) => {
                        if file_all.is_none() {
                            let all = list_files();
                            file_filtered = filter_files(&all, prefix);
                            file_all = Some(all);
                        } else if let Some(all) = &file_all {
                            file_filtered = filter_files(all, prefix);
                        }
                        file_selected = if file_filtered.is_empty() {
                            None
                        } else {
                            Some(0)
                        };
                        file_scroll = 0;
                    }
                    None => {
                        file_all = None;
                        file_filtered.clear();
                        file_selected = None;
                        file_scroll = 0;
                    }
                }
                let fs = file_all
                    .as_ref()
                    .map(|_| (file_filtered.as_slice(), file_selected, file_scroll));
                popup_lines = redraw(
                    stdout,
                    &buf,
                    slash_selected,
                    popup_lines,
                    fs,
                    popup_capacity,
                )?;
            }
            (KeyCode::Esc, _) => {
                file_all = None;
                file_filtered.clear();
                file_selected = None;
                file_scroll = 0;
                slash_selected = None;
                popup_lines = redraw(
                    stdout,
                    &buf,
                    slash_selected,
                    popup_lines,
                    None,
                    popup_capacity,
                )?;
            }

            // ── Tab ───────────────────────────────────────────────────────
            (KeyCode::Tab, _) => {
                if buf.is_empty() {
                    if !tab_statuses.is_empty() {
                        tab_current = (tab_current + 1) % tab_statuses.len();
                        redraw_status(stdout, &tab_statuses[tab_current])?;
                    }
                } else if file_all.is_some() && !file_filtered.is_empty() {
                    let next = file_selected
                        .map(|s| (s + 1) % file_filtered.len())
                        .unwrap_or(0);
                    file_selected = Some(next);
                    adjust_scroll(next, &mut file_scroll, popup_capacity);
                    let fs = Some((file_filtered.as_slice(), file_selected, file_scroll));
                    popup_lines = redraw(
                        stdout,
                        &buf,
                        slash_selected,
                        popup_lines,
                        fs,
                        popup_capacity,
                    )?;
                } else {
                    let matches = filter_matches(&buf);
                    if !matches.is_empty() {
                        slash_selected = Some(match slash_selected {
                            None => 0,
                            Some(s) => (s + 1) % matches.len(),
                        });
                        popup_lines = redraw(
                            stdout,
                            &buf,
                            slash_selected,
                            popup_lines,
                            None,
                            popup_capacity,
                        )?;
                    }
                }
            }

            // ── arrow navigation ─────────────────────────────────────────
            (KeyCode::Down, _) => {
                if file_all.is_some() && !file_filtered.is_empty() {
                    let next = file_selected
                        .map(|s| (s + 1) % file_filtered.len())
                        .unwrap_or(0);
                    file_selected = Some(next);
                    adjust_scroll(next, &mut file_scroll, popup_capacity);
                    let fs = Some((file_filtered.as_slice(), file_selected, file_scroll));
                    popup_lines = redraw(
                        stdout,
                        &buf,
                        slash_selected,
                        popup_lines,
                        fs,
                        popup_capacity,
                    )?;
                } else {
                    let matches = filter_matches(&buf);
                    if !matches.is_empty() {
                        slash_selected = Some(match slash_selected {
                            None => 0,
                            Some(s) => (s + 1) % matches.len(),
                        });
                        popup_lines = redraw(
                            stdout,
                            &buf,
                            slash_selected,
                            popup_lines,
                            None,
                            popup_capacity,
                        )?;
                    }
                }
            }
            (KeyCode::Up, _) => {
                if file_all.is_some() && !file_filtered.is_empty() {
                    let prev = match file_selected {
                        None | Some(0) => file_filtered.len() - 1,
                        Some(s) => s - 1,
                    };
                    file_selected = Some(prev);
                    adjust_scroll(prev, &mut file_scroll, popup_capacity);
                    let fs = Some((file_filtered.as_slice(), file_selected, file_scroll));
                    popup_lines = redraw(
                        stdout,
                        &buf,
                        slash_selected,
                        popup_lines,
                        fs,
                        popup_capacity,
                    )?;
                } else {
                    let matches = filter_matches(&buf);
                    if !matches.is_empty() {
                        slash_selected = Some(match slash_selected {
                            None | Some(0) => matches.len() - 1,
                            Some(s) => s - 1,
                        });
                        popup_lines = redraw(
                            stdout,
                            &buf,
                            slash_selected,
                            popup_lines,
                            None,
                            popup_capacity,
                        )?;
                    }
                }
            }

            // ── @ triggers file-mention mode ──────────────────────────────
            (KeyCode::Char('@'), _) => {
                buf.push('@');
                let all = list_files();
                file_filtered = all.clone();
                file_all = Some(all);
                file_selected = if file_filtered.is_empty() {
                    None
                } else {
                    Some(0)
                };
                file_scroll = 0;
                let fs = Some((file_filtered.as_slice(), file_selected, file_scroll));
                popup_lines = redraw(
                    stdout,
                    &buf,
                    slash_selected,
                    popup_lines,
                    fs,
                    popup_capacity,
                )?;
            }

            // ── space ends a file path ────────────────────────────────────
            (KeyCode::Char(' '), _) if file_all.is_some() => {
                buf.push(' ');
                file_all = None;
                file_filtered.clear();
                file_selected = None;
                file_scroll = 0;
                slash_selected = None;
                popup_lines = redraw(
                    stdout,
                    &buf,
                    slash_selected,
                    popup_lines,
                    None,
                    popup_capacity,
                )?;
            }

            // ── regular character ────────────────────────────────────────
            (KeyCode::Char(c), _) => {
                buf.push(c);
                if file_all.is_some() {
                    if let Some((_, prefix)) = at_prefix(&buf) {
                        if let Some(all) = &file_all {
                            file_filtered = filter_files(all, prefix);
                        }
                    }
                    file_selected = if file_filtered.is_empty() {
                        None
                    } else {
                        Some(0)
                    };
                    file_scroll = 0;
                    let fs = Some((file_filtered.as_slice(), file_selected, file_scroll));
                    popup_lines = redraw(
                        stdout,
                        &buf,
                        slash_selected,
                        popup_lines,
                        fs,
                        popup_capacity,
                    )?;
                } else {
                    slash_selected = None;
                    popup_lines = redraw(
                        stdout,
                        &buf,
                        slash_selected,
                        popup_lines,
                        None,
                        popup_capacity,
                    )?;
                }
            }

            _ => {}
        }
    }
}
