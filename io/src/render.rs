//! Terminal rendering: full-screen TUI with fixed prompt bar,
//! markdown, thoughts, tool calls, and syntax-colored diffs.

use std::io::Write;

pub const PROMPT_BAR_HEIGHT: u16 = 3;

// ── Theme ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct Theme {
    pub name: &'static str,
    /// true = optimised for dark terminal backgrounds, false = light backgrounds.
    pub dark: bool,
    /// Primary accent color: logo, ▌ bar, agent name highlight.
    pub accent: crossterm::style::Color,
    /// Secondary muted color: separators, borders, dots, hint text.
    pub muted: crossterm::style::Color,
    /// Diff: addition line foreground.
    pub diff_add_fg: crossterm::style::Color,
    /// Diff: addition line background.
    pub diff_add_bg: crossterm::style::Color,
    /// Diff: deletion line foreground.
    pub diff_del_fg: crossterm::style::Color,
    /// Diff: deletion line background.
    pub diff_del_bg: crossterm::style::Color,
    /// Single display-width char used as the addition prefix symbol.
    pub diff_add_prefix: &'static str,
    /// Single display-width char used as the deletion prefix symbol.
    pub diff_del_prefix: &'static str,
}

pub fn get_theme(name: &str) -> Theme {
    use crossterm::style::Color;
    match name {
        "ocean" => Theme {
            name: "ocean",
            dark: true,
            accent: Color::Blue,
            muted: Color::DarkGrey,
            diff_add_fg: Color::White,
            diff_add_bg: Color::DarkBlue,
            diff_del_fg: Color::DarkGrey,
            diff_del_bg: Color::Black,
            diff_add_prefix: "▶",
            diff_del_prefix: "◀",
        },
        "rose" => Theme {
            name: "rose",
            dark: true,
            accent: Color::Magenta,
            muted: Color::DarkGrey,
            diff_add_fg: Color::White,
            diff_add_bg: Color::DarkMagenta,
            diff_del_fg: Color::DarkGrey,
            diff_del_bg: Color::Black,
            diff_add_prefix: "◆",
            diff_del_prefix: "◇",
        },
        "forest" => Theme {
            name: "forest",
            dark: true,
            accent: Color::Green,
            muted: Color::DarkGrey,
            diff_add_fg: Color::White,
            diff_add_bg: Color::DarkGreen,
            diff_del_fg: Color::DarkGrey,
            diff_del_bg: Color::Black,
            diff_add_prefix: "+",
            diff_del_prefix: "-",
        },
        "sunset" => Theme {
            name: "sunset",
            dark: true,
            accent: Color::Yellow,
            muted: Color::DarkGrey,
            diff_add_fg: Color::Black,
            diff_add_bg: Color::DarkYellow,
            diff_del_fg: Color::DarkGrey,
            diff_del_bg: Color::Black,
            diff_add_prefix: "›",
            diff_del_prefix: "‹",
        },
        "mono" => Theme {
            name: "mono",
            dark: true,
            accent: Color::White,
            muted: Color::Grey,
            diff_add_fg: Color::White,
            diff_add_bg: Color::DarkGrey,
            diff_del_fg: Color::Grey,
            diff_del_bg: Color::Black,
            diff_add_prefix: "+",
            diff_del_prefix: "-",
        },
        // ── Light-terminal themes (Reset bg = terminal default, dark fg for contrast) ──
        "breeze" => Theme {
            name: "breeze",
            dark: false,
            accent: Color::DarkCyan,
            muted: Color::DarkGrey,
            diff_add_fg: Color::DarkGreen,
            diff_add_bg: Color::Reset,
            diff_del_fg: Color::DarkRed,
            diff_del_bg: Color::Reset,
            diff_add_prefix: "+",
            diff_del_prefix: "-",
        },
        "ink" => Theme {
            name: "ink",
            dark: false,
            accent: Color::DarkBlue,
            muted: Color::DarkGrey,
            diff_add_fg: Color::DarkGreen,
            diff_add_bg: Color::Reset,
            diff_del_fg: Color::DarkMagenta,
            diff_del_bg: Color::Reset,
            diff_add_prefix: "▶",
            diff_del_prefix: "◀",
        },
        _ => Theme {
            name: "default",
            dark: true,
            accent: Color::Cyan,
            muted: Color::DarkGrey,
            diff_add_fg: Color::White,
            diff_add_bg: Color::DarkGreen,
            diff_del_fg: Color::White,
            diff_del_bg: Color::DarkRed,
            diff_add_prefix: "+",
            diff_del_prefix: "-",
        },
    }
}

pub const THEME_NAMES: &[&str] = &[
    "default", "ocean", "rose", "forest", "sunset", "mono", "breeze", "ink",
];

// ── TUI lifecycle ──────────────────────────────────────────────────────────────

/// Enter TUI mode: switch to the alternate screen (hides shell history
/// entirely), enable raw mode, enable mouse capture for scroll events,
/// hide cursor, and set a scroll region that protects the bottom
/// `PROMPT_BAR_HEIGHT` rows from scrolling.
pub fn enter_tui() -> std::io::Result<()> {
    let (_, h) = crossterm::terminal::size()?;
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::cursor::Hide,
    )?;
    set_scroll_region(1, h.saturating_sub(PROMPT_BAR_HEIGHT));
    Ok(())
}

/// Restore terminal to normal state: leave alternate screen, show cursor,
/// disable mouse capture, reset scroll region, disable raw mode.
pub fn exit_tui() -> std::io::Result<()> {
    reset_scroll_region();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Show,
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen,
    )?;
    crossterm::terminal::disable_raw_mode()
}

fn set_scroll_region(top: u16, bottom: u16) {
    if bottom > top {
        let _ = write!(std::io::stdout(), "\x1b[{};{}r", top, bottom);
        let _ = std::io::stdout().flush();
    }
}

fn reset_scroll_region() {
    let _ = write!(std::io::stdout(), "\x1b[r");
    let _ = std::io::stdout().flush();
}

/// Re-read terminal size and re-apply scroll region after a resize.
pub fn handle_resize() -> std::io::Result<()> {
    let (_, h) = crossterm::terminal::size()?;
    set_scroll_region(1, h.saturating_sub(PROMPT_BAR_HEIGHT));
    Ok(())
}

/// Render a page of session history from the line buffer.
/// `scroll_offset` = how many lines from the bottom are currently hidden.
pub fn render_scroll_view(
    lines: &std::collections::VecDeque<String>,
    scroll_offset: usize,
    theme: &Theme,
) -> std::io::Result<()> {
    use crossterm::{
        cursor, queue,
        style::{Print, ResetColor, SetForegroundColor},
        terminal::{self, ClearType},
    };
    let (w, h) = terminal::size()?;
    let content_h = h.saturating_sub(PROMPT_BAR_HEIGHT) as usize;
    let mut out = std::io::stdout();

    for row in 0..content_h as u16 {
        queue!(
            out,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::CurrentLine)
        )?;
    }

    let n = lines.len();
    let display_rows = content_h.saturating_sub(1); // row 0 = indicator bar
    let end = n.saturating_sub(scroll_offset);
    let start = end.saturating_sub(display_rows);

    for (i, line) in lines.range(start..end).enumerate() {
        queue!(out, cursor::MoveTo(0, 1 + i as u16))?;
        if let Some(ansi) = line.strip_prefix('\x01') {
            // Pre-rendered ANSI line — print as-is (no color override, no truncation).
            queue!(out, Print(ansi), ResetColor)?;
        } else {
            let s: String = line.chars().take(w as usize).collect();
            queue!(out, SetForegroundColor(theme.muted), Print(s), ResetColor)?;
        }
    }

    // Indicator row at the top of the content area
    let indicator = if start == 0 {
        "── top of session ──  ↓ scroll down  ·  any key → live".to_string()
    } else {
        format!(
            "↑ {} more lines  ·  ↓ scroll down  ·  any key → live",
            start
        )
    };
    let s: String = indicator
        .chars()
        .take(w.saturating_sub(4) as usize)
        .collect();
    queue!(
        out,
        cursor::MoveTo(2, 0),
        SetForegroundColor(theme.accent),
        Print(s),
        ResetColor,
    )?;

    out.flush()
}

// ── Prompt bar ─────────────────────────────────────────────────────────────────

/// Draw the fixed prompt bar at the bottom of the terminal.
///
/// Layout (3 rows):
/// ```text
/// ──────────────────────────────────────  ← thin separator
/// ▌ user input text                        ← cyan accent + input
/// ▌ Build · model · provider   8.7K /cmds  ← status + right info
/// ```
///
/// Cursor is placed at the end of the input on the middle row.
pub fn draw_prompt_bar(
    input: &str,
    agent_name: &str,
    provider_id: &str,
    model_id: &str,
    input_tokens: u32,
    context_window: u64,
    theme: &Theme,
) -> std::io::Result<()> {
    use crossterm::{
        cursor, execute,
        style::{Print, ResetColor, SetForegroundColor},
        terminal,
    };

    let (w, h) = crossterm::terminal::size()?;
    let sep_row = h.saturating_sub(PROMPT_BAR_HEIGHT);
    let input_row = sep_row + 1;
    let status_row = sep_row + 2;

    // Separator
    execute!(
        std::io::stdout(),
        cursor::MoveTo(0, sep_row),
        terminal::Clear(terminal::ClearType::CurrentLine),
        SetForegroundColor(theme.muted),
        Print("─".repeat(w as usize)),
        ResetColor,
    )?;

    // Input row: accent bar + text
    execute!(
        std::io::stdout(),
        cursor::MoveTo(0, input_row),
        terminal::Clear(terminal::ClearType::CurrentLine),
        SetForegroundColor(theme.accent),
        Print("▌ "),
        ResetColor,
        Print(input),
    )?;

    // Status row: accent bar + agent info (left) + context usage + hint (right)
    let dot = " · ";
    let hint = "/commands";
    let ctx = format_context_info(input_tokens, context_window);
    let right = if ctx.is_empty() {
        format!("{}  ", hint)
    } else {
        format!("{}  {}  ", ctx, hint)
    };

    // Compute how much width is available for model_id
    let left_fixed = 2 + agent_name.len() + dot.len() * 2 + provider_id.len();
    let model_budget = (w as usize).saturating_sub(left_fixed + right.len());
    let mut m_end = model_id.len().min(model_budget);
    while m_end > 0 && !model_id.is_char_boundary(m_end) {
        m_end -= 1;
    }
    let model_display = &model_id[..m_end];

    execute!(
        std::io::stdout(),
        cursor::MoveTo(0, status_row),
        terminal::Clear(terminal::ClearType::CurrentLine),
        SetForegroundColor(theme.accent),
        Print("▌ "),
        Print(agent_name),
        SetForegroundColor(theme.muted),
        Print(dot),
        ResetColor,
        Print(model_display),
        SetForegroundColor(theme.muted),
        Print(dot),
        Print(provider_id),
        ResetColor,
    )?;

    // Right-aligned info
    let right_x = w.saturating_sub(right.len() as u16);
    execute!(
        std::io::stdout(),
        cursor::MoveTo(right_x, status_row),
        SetForegroundColor(theme.muted),
        Print(&right),
        ResetColor,
    )?;

    // Park cursor at end of input
    execute!(
        std::io::stdout(),
        cursor::MoveTo(2 + input.len() as u16, input_row),
    )?;
    std::io::stdout().flush()
}

fn format_context_info(used: u32, window: u64) -> String {
    if window == 0 {
        return String::new();
    }
    let window_label = if window >= 1_000_000 {
        format!("{}M", window / 1_000_000)
    } else {
        format!("{}K", window / 1_000)
    };
    if used == 0 {
        return format!("{} ctx", window_label);
    }
    let pct = ((used as f64 / window as f64) * 100.0).min(100.0) as u64;
    let remaining = window.saturating_sub(used as u64);
    let rem_label = if remaining >= 1_000_000 {
        format!("{}M", remaining / 1_000_000)
    } else {
        format!("{}K", remaining / 1_000)
    };
    format!("{}% used · {}  rem · {} ctx", pct, rem_label, window_label)
}

/// Clear the input portion of the prompt bar (line after `> `) without
/// redrawing the whole bar. Used to blank the line before streaming.
pub fn clear_prompt_input() -> std::io::Result<()> {
    use crossterm::{cursor, execute, terminal};

    let (_, h) = crossterm::terminal::size()?;
    let prompt_y = h - PROMPT_BAR_HEIGHT;
    execute!(
        std::io::stdout(),
        cursor::MoveTo(0, prompt_y + 1),
        terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
    )?;
    Ok(())
}

// ── Streaming preparation ──────────────────────────────────────────────────────

/// Move cursor to the last scrollable row so streaming output appears at
/// the bottom of the content area (above the prompt bar).
pub fn prepare_streaming() -> std::io::Result<()> {
    let (_, h) = crossterm::terminal::size()?;
    let row = h.saturating_sub(PROMPT_BAR_HEIGHT).saturating_sub(1);
    crossterm::execute!(std::io::stdout(), crossterm::cursor::MoveTo(0, row))?;
    Ok(())
}

// ── Existing render functions (unchanged, used during streaming) ────────────────

/// Render a compact context bar: `ctx [████░░░░░░] 13% of 200K`
#[allow(dead_code)]
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
    format!(
        "ctx [{}{}] {}% of {}",
        "█".repeat(filled),
        "░".repeat(empty),
        pct,
        window_label
    )
}

fn make_skin(theme: &Theme) -> termimad::MadSkin {
    use termimad::crossterm::style::Color;
    let mut skin = termimad::MadSkin::default();
    skin.paragraph.align = termimad::Alignment::Left;
    skin.paragraph.left_margin = 0;
    skin.code_block.align = termimad::Alignment::Left;
    skin.code_block.left_margin = 0;
    for h in &mut skin.headers {
        h.align = termimad::Alignment::Left;
        h.left_margin = 0;
    }
    skin.table.align = termimad::Alignment::Left;
    skin.table.left_margin = 0;

    let (code_fg, bold_fg, italic_fg) = if theme.dark {
        (Color::Yellow, Color::Green, Color::White)
    } else {
        (Color::DarkYellow, Color::DarkGreen, Color::DarkGrey)
    };
    skin.headers[0].set_fg(bold_fg);
    skin.headers[0].add_attr(termimad::crossterm::style::Attribute::Bold);
    skin.bold.set_fg(bold_fg);
    skin.italic.set_fg(italic_fg);
    skin.inline_code.set_fg(code_fg);
    skin.inline_code.set_bg(Color::Reset);
    skin.code_block.set_fg(code_fg);
    skin.code_block.set_bg(Color::Reset);
    skin
}

/// Render markdown and return the ANSI-colored lines (without printing).
pub fn render_markdown_lines(text: &str, theme: &Theme) -> Vec<String> {
    let skin = make_skin(theme);
    let rendered = format!("{}", skin.text(text, None));
    rendered.lines().map(|l| l.to_string()).collect()
}

pub fn render_thoughts(thoughts: &str) {
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

/// One-line human-readable summary of a tool call's input, shared by the
/// tool-start renderer and the permission prompts.
pub fn tool_detail(name: &str, input: &serde_json::Value) -> String {
    match name {
        "bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "read" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "write" => input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "edit" => input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "grep" => {
            let pat = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
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
    }
}

pub fn render_tool_start(name: &str, input: &serde_json::Value) {
    use crossterm::style::{Color, Stylize};
    let label = format!(" {name} ").with(Color::Black).on(Color::DarkGrey);
    let detail = tool_detail(name, input);
    if detail.is_empty() {
        print!("  {label}\r\n");
    } else {
        print!("  {label}  {}\r\n", detail.dark_grey());
    }
    let _ = std::io::stdout().flush();
}

pub fn render_tool_done(name: &str, output: &str, success: bool, theme: &Theme) {
    if !success {
        use crossterm::style::Stylize;
        print!("  {}\r\n", format!("error: {output}").red());
        let _ = std::io::stdout().flush();
        return;
    }
    match name {
        "write" | "edit" => render_diff(output, theme),
        _ => {}
    }
}

fn render_diff(diff: &str, theme: &Theme) {
    use crossterm::{
        execute,
        style::{Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    };
    use std::io::stdout;

    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for raw in diff.lines() {
        if raw.starts_with("--- ") || raw.starts_with("+++ ") {
            let _ = execute!(
                stdout(),
                SetForegroundColor(theme.muted),
                Print(format!("  {raw}\r\n")),
                ResetColor
            );
        } else if let Some(rest) = raw.strip_prefix("@@ ") {
            if let Some((a, b)) = parse_hunk(rest) {
                old_line = a;
                new_line = b;
            }
            let _ = execute!(
                stdout(),
                SetForegroundColor(theme.accent),
                Print(format!("  @@ {rest}\r\n")),
                ResetColor
            );
        } else if let Some(content) = raw.strip_prefix('-') {
            let _ = execute!(
                stdout(),
                SetForegroundColor(theme.diff_del_fg),
                Print(format!("{:>5} ", old_line)),
                SetForegroundColor(theme.diff_del_fg),
                SetBackgroundColor(theme.diff_del_bg),
                Print(theme.diff_del_prefix),
                ResetColor,
                SetBackgroundColor(theme.diff_del_bg),
                SetForegroundColor(theme.diff_del_fg),
                Print(format!("  {content}")),
                ResetColor,
                Print("\r\n")
            );
            old_line += 1;
        } else if let Some(content) = raw.strip_prefix('+') {
            let _ = execute!(
                stdout(),
                SetForegroundColor(theme.diff_add_fg),
                Print(format!("{:>5} ", new_line)),
                SetForegroundColor(theme.diff_add_fg),
                SetBackgroundColor(theme.diff_add_bg),
                Print(theme.diff_add_prefix),
                ResetColor,
                SetBackgroundColor(theme.diff_add_bg),
                SetForegroundColor(theme.diff_add_fg),
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
    let s = s.trim_start_matches('-');
    let mut parts = s.splitn(2, ' ');
    let old_part = parts.next()?;
    let rest = parts.next()?.trim_start_matches('+');
    let a = old_part.split(',').next()?.parse().ok()?;
    let b = rest.split(',').next()?.parse().ok()?;
    Some((a, b))
}

// ── Splash screen ──────────────────────────────────────────────────────────────

const IO_LOGO: &[&str] = &[
    "████     ████  ",
    " ██     ██  ██ ",
    " ██     ██  ██ ",
    " ██     ██  ██ ",
    "████     ████  ",
];

/// Geometry returned by `draw_splash` and consumed by the partial-update helpers.
#[derive(Clone)]
pub struct SplashLayout {
    pub box_x: u16,
    pub box_w: u16,
    pub input_row: u16,
    pub status_row: u16,
}

impl SplashLayout {
    fn inner_w(&self) -> usize {
        self.box_w.saturating_sub(2) as usize
    }
}

/// Terminal column for the input cursor given the current buffer.
pub fn splash_cursor(layout: &SplashLayout, buf: &str) -> (u16, u16) {
    // box_x+1 (border) + 2 (prefix spaces) + buf
    (layout.box_x + 3 + buf.len() as u16, layout.input_row)
}

/// Full redraw: logo + centered input box + cwd/version footer.
pub fn draw_splash(
    buf: &str,
    agent_name: &str,
    provider_id: &str,
    model_id: &str,
    theme: &Theme,
) -> std::io::Result<SplashLayout> {
    use crossterm::{
        cursor, execute,
        style::{Print, ResetColor, SetForegroundColor},
        terminal,
    };

    let (w, h) = terminal::size()?;
    let mut out = std::io::stdout();

    execute!(out, terminal::Clear(terminal::ClearType::All))?;

    let logo_w = IO_LOGO.iter().map(|l| l.len()).max().unwrap_or(0) as u16;
    let logo_h = IO_LOGO.len() as u16;

    let box_w = (w * 2 / 3).max(52).min(90);
    let inner_w = box_w.saturating_sub(2) as usize;

    const DROP_CMDS: &[(&str, &str)] = &[
        ("/help", "show all commands"),
        ("/new", "start new session"),
        ("/agent", "switch agent"),
        ("/model", "switch model"),
        ("/theme", "change color theme"),
        ("/compact", "compact context"),
        ("/cost", "session token cost"),
        ("/exit", "end session"),
    ];
    let n_cmds = DROP_CMDS.len() as u16;
    // unified box rows: ╭╮ + input + status + ├┤ + cmds + ╰╯  = n_cmds + 5
    let box_full = n_cmds + 5;
    let box_small = 4u16; // ╭╮ + input + status + ╰╯
    let show_drop = h >= logo_h + 2 + box_full;

    let block_h = logo_h + 2 + if show_drop { box_full } else { box_small };
    let start_y = h.saturating_sub(block_h) / 2;
    let box_y = start_y + logo_h + 2;

    let logo_x = w.saturating_sub(logo_w) / 2;
    let box_x = w.saturating_sub(box_w) / 2;

    // Logo
    for (i, line) in IO_LOGO.iter().enumerate() {
        execute!(
            out,
            cursor::MoveTo(logo_x, start_y + i as u16),
            SetForegroundColor(theme.accent),
            Print(line),
            ResetColor,
        )?;
    }

    let input_row = box_y + 1;
    let status_row = box_y + 2;
    let bottom_row = box_y + if show_drop { box_full } else { box_small } - 1;

    // ── Unified rounded box ────────────────────────────────────────────────────
    execute!(
        out,
        cursor::MoveTo(box_x, box_y),
        SetForegroundColor(theme.muted),
        Print(format!("╭{}╮", "─".repeat(inner_w))),
        ResetColor,
    )?;
    for row in [input_row, status_row] {
        execute!(
            out,
            cursor::MoveTo(box_x, row),
            SetForegroundColor(theme.muted),
            Print("│"),
            cursor::MoveTo(box_x + box_w - 1, row),
            Print("│"),
            ResetColor,
        )?;
    }
    if show_drop {
        use crossterm::style::{Attribute, SetAttribute};
        let sep_row = box_y + 3;
        // ├─ commands ──────────────────────────────────────────────────────────┤
        let title = " commands ";
        let sep_dashes = inner_w.saturating_sub(title.len() + 1);
        execute!(
            out,
            cursor::MoveTo(box_x, sep_row),
            SetForegroundColor(theme.muted),
            Print(format!("├─{}{}┤", title, "─".repeat(sep_dashes))),
            ResetColor,
        )?;
        let name_w = 10usize; // "/compact" = 8 + 2 spaces padding
        for (i, (cmd, desc)) in DROP_CMDS.iter().enumerate() {
            let row = sep_row + 1 + i as u16;
            let name_padded = format!("{:<width$}", cmd, width = name_w);
            let desc_avail = inner_w.saturating_sub(name_w + 3);
            let desc_str: String = format!("  {}", desc).chars().take(desc_avail).collect();
            let fill = inner_w.saturating_sub(2 + name_w + desc_str.chars().count());
            execute!(
                out,
                cursor::MoveTo(box_x, row),
                SetForegroundColor(theme.muted),
                Print("│"),
                Print("  "),
                SetAttribute(Attribute::Bold),
                SetForegroundColor(theme.accent),
                Print(&name_padded),
                SetAttribute(Attribute::Reset),
                ResetColor,
                SetForegroundColor(theme.muted),
                Print(&desc_str),
                Print(format!("{:fill$}", "")),
                SetForegroundColor(theme.muted),
                Print("│"),
                ResetColor,
            )?;
        }
    }
    execute!(
        out,
        cursor::MoveTo(box_x, bottom_row),
        SetForegroundColor(theme.muted),
        Print(format!("╰{}╯", "─".repeat(inner_w))),
        ResetColor,
    )?;

    // Input line
    execute!(out, cursor::MoveTo(box_x + 1, input_row), Print("  "))?;
    if buf.is_empty() {
        execute!(
            out,
            SetForegroundColor(theme.muted),
            Print(r#"Ask anything... "Fix a TODO in the codebase""#),
            ResetColor,
        )?;
    } else {
        execute!(out, Print(buf))?;
    }

    // Status line
    draw_splash_status_at(
        &mut out,
        box_x,
        inner_w,
        status_row,
        agent_name,
        provider_id,
        model_id,
        theme,
    )?;

    // CWD bottom-left
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| {
            let s = p.display().to_string();
            if let Ok(home) = std::env::var("HOME") {
                if s.starts_with(&home) {
                    return format!("~{}", &s[home.len()..]);
                }
            }
            s
        })
        .unwrap_or_default();
    let cwd_max = (w / 4) as usize;
    let cwd_display = if cwd.len() > cwd_max && cwd_max > 3 {
        format!("…{}", &cwd[cwd.len() - (cwd_max - 1)..])
    } else {
        cwd
    };
    execute!(
        out,
        cursor::MoveTo(1, h.saturating_sub(1)),
        SetForegroundColor(theme.muted),
        Print(&cwd_display),
        ResetColor,
    )?;

    // Version bottom-right  ● v0.1.0
    let ver = concat!("v", env!("CARGO_PKG_VERSION"));
    // "● " prefix (2 chars) + ver
    let ver_total = 2 + ver.len();
    if (ver_total as u16 + 1) < w {
        execute!(
            out,
            cursor::MoveTo(w.saturating_sub(ver_total as u16 + 1), h.saturating_sub(1)),
            SetForegroundColor(crossterm::style::Color::Green),
            Print("● "),
            SetForegroundColor(theme.muted),
            Print(ver),
            ResetColor,
        )?;
    }

    out.flush()?;
    Ok(SplashLayout {
        box_x,
        box_w,
        input_row,
        status_row,
    })
}

/// Redraw only the input line — called on every keystroke to avoid full-screen flicker.
pub fn splash_update_input(layout: &SplashLayout, buf: &str, theme: &Theme) -> std::io::Result<()> {
    use crossterm::{
        cursor, execute,
        style::{Print, ResetColor, SetForegroundColor},
    };
    let inner_w = layout.inner_w();
    let mut out = std::io::stdout();

    // Redraw left border + clear interior + redraw right border
    execute!(
        out,
        cursor::MoveTo(layout.box_x, layout.input_row),
        SetForegroundColor(theme.muted),
        Print("│"),
        ResetColor,
        cursor::MoveTo(layout.box_x + 1, layout.input_row),
        Print(" ".repeat(inner_w)),
        cursor::MoveTo(layout.box_x + layout.box_w - 1, layout.input_row),
        SetForegroundColor(theme.muted),
        Print("│"),
        ResetColor,
    )?;

    // Draw content
    execute!(
        out,
        cursor::MoveTo(layout.box_x + 1, layout.input_row),
        Print("  ")
    )?;
    if buf.is_empty() {
        execute!(
            out,
            SetForegroundColor(theme.muted),
            Print(r#"Ask anything... "Fix a TODO in the codebase""#),
            ResetColor,
        )?;
    } else {
        execute!(out, Print(buf))?;
    }

    out.flush()
}

/// Redraw only the status line — called when the agent is cycled with Tab.
pub fn splash_update_status(
    layout: &SplashLayout,
    agent_name: &str,
    provider_id: &str,
    model_id: &str,
    theme: &Theme,
) -> std::io::Result<()> {
    use crossterm::{
        cursor, execute,
        style::{Print, ResetColor, SetForegroundColor},
    };
    let inner_w = layout.inner_w();
    let mut out = std::io::stdout();

    execute!(
        out,
        cursor::MoveTo(layout.box_x, layout.status_row),
        SetForegroundColor(theme.muted),
        Print("│"),
        ResetColor,
        cursor::MoveTo(layout.box_x + 1, layout.status_row),
        Print(" ".repeat(inner_w)),
        cursor::MoveTo(layout.box_x + layout.box_w - 1, layout.status_row),
        SetForegroundColor(theme.muted),
        Print("│"),
        ResetColor,
    )?;

    draw_splash_status_at(
        &mut out,
        layout.box_x,
        inner_w,
        layout.status_row,
        agent_name,
        provider_id,
        model_id,
        theme,
    )?;
    out.flush()
}

fn draw_splash_status_at(
    out: &mut impl std::io::Write,
    box_x: u16,
    inner_w: usize,
    row: u16,
    agent_name: &str,
    provider_id: &str,
    model_id: &str,
    theme: &Theme,
) -> std::io::Result<()> {
    use crossterm::{
        cursor, execute,
        style::{Print, ResetColor, SetForegroundColor},
    };
    let sep = " · ";
    // visible chars: 2 prefix + agent + sep + model + sep + provider
    let base = 2 + agent_name.len() + sep.len() + sep.len() + provider_id.len();
    let model_max = inner_w.saturating_sub(base);
    let mut end = model_id.len().min(model_max);
    while end > 0 && !model_id.is_char_boundary(end) {
        end -= 1;
    }
    let model_display = &model_id[..end];

    execute!(
        out,
        cursor::MoveTo(box_x + 1, row),
        Print("  "),
        SetForegroundColor(theme.accent),
        Print(agent_name),
        SetForegroundColor(theme.muted),
        Print(sep),
        ResetColor,
        Print(model_display),
        SetForegroundColor(theme.muted),
        Print(sep),
        Print(provider_id),
        ResetColor,
    )
}
