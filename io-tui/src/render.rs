//! Terminal rendering: full-screen TUI with fixed prompt bar,
//! markdown, thoughts, tool calls, and syntax-colored diffs.

use std::io::Write;

pub const PROMPT_BAR_HEIGHT: u16 = 3;
/// Maximum input lines before the prompt stops growing (capped at this height).
pub const MAX_INPUT_LINES: u16 = 5;

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
    /// Inline code / code block foreground color.
    pub code_fg: crossterm::style::Color,
    /// Bold text and heading foreground color.
    pub bold_fg: crossterm::style::Color,
    /// Italic text foreground color.
    pub italic_fg: crossterm::style::Color,
    /// Tool name badge foreground color.
    pub tool_tag_fg: crossterm::style::Color,
    /// Tool name badge background color.
    pub tool_tag_bg: crossterm::style::Color,
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
            code_fg: Color::Cyan,
            bold_fg: Color::Blue,
            italic_fg: Color::Grey,
            tool_tag_fg: Color::Black,
            tool_tag_bg: Color::DarkBlue,
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
            code_fg: Color::Yellow,
            bold_fg: Color::Magenta,
            italic_fg: Color::Grey,
            tool_tag_fg: Color::Black,
            tool_tag_bg: Color::DarkMagenta,
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
            code_fg: Color::Cyan,
            bold_fg: Color::Green,
            italic_fg: Color::Grey,
            tool_tag_fg: Color::Black,
            tool_tag_bg: Color::DarkGreen,
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
            code_fg: Color::Yellow,
            bold_fg: Color::Yellow,
            italic_fg: Color::DarkGrey,
            tool_tag_fg: Color::Black,
            tool_tag_bg: Color::DarkYellow,
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
            code_fg: Color::Grey,
            bold_fg: Color::White,
            italic_fg: Color::DarkGrey,
            tool_tag_fg: Color::White,
            tool_tag_bg: Color::DarkGrey,
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
            code_fg: Color::DarkYellow,
            bold_fg: Color::DarkGreen,
            italic_fg: Color::DarkGrey,
            tool_tag_fg: Color::White,
            tool_tag_bg: Color::DarkCyan,
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
            code_fg: Color::DarkBlue,
            bold_fg: Color::DarkBlue,
            italic_fg: Color::DarkGrey,
            tool_tag_fg: Color::White,
            tool_tag_bg: Color::DarkBlue,
        },
        "dawn" => Theme {
            name: "dawn",
            dark: false,
            accent: Color::DarkRed,
            muted: Color::DarkGrey,
            diff_add_fg: Color::DarkGreen,
            diff_add_bg: Color::Reset,
            diff_del_fg: Color::DarkRed,
            diff_del_bg: Color::Reset,
            diff_add_prefix: "+",
            diff_del_prefix: "-",
            code_fg: Color::DarkYellow,
            bold_fg: Color::DarkRed,
            italic_fg: Color::DarkGrey,
            tool_tag_fg: Color::White,
            tool_tag_bg: Color::DarkRed,
        },
        "sand" => Theme {
            name: "sand",
            dark: false,
            accent: Color::DarkYellow,
            muted: Color::DarkGrey,
            diff_add_fg: Color::DarkGreen,
            diff_add_bg: Color::Reset,
            diff_del_fg: Color::DarkRed,
            diff_del_bg: Color::Reset,
            diff_add_prefix: "›",
            diff_del_prefix: "‹",
            code_fg: Color::DarkCyan,
            bold_fg: Color::DarkYellow,
            italic_fg: Color::DarkGrey,
            tool_tag_fg: Color::White,
            tool_tag_bg: Color::DarkYellow,
        },
        "mint" => Theme {
            name: "mint",
            dark: false,
            accent: Color::DarkGreen,
            muted: Color::DarkGrey,
            diff_add_fg: Color::DarkGreen,
            diff_add_bg: Color::Reset,
            diff_del_fg: Color::DarkRed,
            diff_del_bg: Color::Reset,
            diff_add_prefix: "◆",
            diff_del_prefix: "◇",
            code_fg: Color::DarkCyan,
            bold_fg: Color::DarkGreen,
            italic_fg: Color::DarkGrey,
            tool_tag_fg: Color::White,
            tool_tag_bg: Color::DarkGreen,
        },
        "dusk" => Theme {
            name: "dusk",
            dark: false,
            accent: Color::DarkMagenta,
            muted: Color::DarkGrey,
            diff_add_fg: Color::DarkGreen,
            diff_add_bg: Color::Reset,
            diff_del_fg: Color::DarkMagenta,
            diff_del_bg: Color::Reset,
            diff_add_prefix: "◆",
            diff_del_prefix: "◇",
            code_fg: Color::DarkBlue,
            bold_fg: Color::DarkMagenta,
            italic_fg: Color::DarkGrey,
            tool_tag_fg: Color::White,
            tool_tag_bg: Color::DarkMagenta,
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
            code_fg: Color::Yellow,
            bold_fg: Color::Green,
            italic_fg: Color::White,
            tool_tag_fg: Color::Black,
            tool_tag_bg: Color::DarkGrey,
        },
    }
}

pub const THEME_NAMES: &[&str] = &[
    "default", "breeze", "ocean", "ink", "rose", "dawn", "forest", "mint", "sunset", "sand",
    "mono", "dusk",
];

/// Fixed per-agent accent colors — independent of the active theme.
pub fn agent_color(name: &str) -> crossterm::style::Color {
    use crossterm::style::Color;
    match name {
        "Builder" => Color::Cyan,
        "Planner" => Color::Green,
        "Debugger" => Color::Yellow,
        "Refactor" => Color::Red,
        _ => Color::Reset, // caller falls back to theme.accent
    }
}

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
/// `prompt_height` = current height of the prompt bar (default: `PROMPT_BAR_HEIGHT`).
pub fn render_scroll_view(
    lines: &std::collections::VecDeque<String>,
    scroll_offset: usize,
    theme: &Theme,
    prompt_height: u16,
) -> std::io::Result<()> {
    use crossterm::{
        cursor, queue,
        style::{Print, ResetColor, SetForegroundColor},
        terminal::{self, ClearType},
    };
    let (w, h) = terminal::size()?;
    let content_h = h.saturating_sub(prompt_height) as usize;
    let mut out = std::io::stdout();

    for row in 0..content_h as u16 {
        queue!(
            out,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::CurrentLine)
        )?;
    }

    let n = lines.len();
    // Never scroll past the point where the first line is at the top of the
    // content area — beyond that the screen would go blank.
    let max_scroll = n.saturating_sub(content_h);
    let scroll_offset = scroll_offset.min(max_scroll);
    let end = n.saturating_sub(scroll_offset);
    let start = end.saturating_sub(content_h);

    for (i, line) in lines.range(start..end).enumerate() {
        queue!(out, cursor::MoveTo(0, i as u16))?;
        if let Some(ansi) = line.strip_prefix('\x01') {
            queue!(out, Print(ansi), ResetColor)?;
        } else {
            let s: String = line.chars().take(w as usize).collect();
            queue!(out, SetForegroundColor(theme.muted), Print(s), ResetColor)?;
        }
    }

    out.flush()
}

// ── Prompt bar ─────────────────────────────────────────────────────────────────

/// Draw the prompt bar at the bottom of the terminal.
///
/// Supports multiline input (newlines in `input`). Returns the actual height
/// used: `input_lines + 2` (separator + input lines + status row).
///
/// Layout for N input lines:
/// ```text
/// ──────────────────────────────  ← separator  (row h - N - 2)
/// ▌ first line of input           ← accent bar  (row h - N - 1)
///   second line …                 ← continuation (row h - N)
/// ▌ Build · model   8.7K /cmds    ← status       (row h - 1)
/// ```
pub fn draw_prompt_bar(
    input: &str,
    agent_name: &str,
    provider_id: &str,
    model_id: &str,
    input_tokens: u32,
    context_window: u64,
    theme: &Theme,
) -> std::io::Result<u16> {
    use crossterm::{
        cursor, execute,
        style::{Print, ResetColor, SetForegroundColor},
        terminal,
    };

    let (w, h) = crossterm::terminal::size()?;

    // Cap input lines so the prompt never consumes more than half the screen.
    let raw_lines: Vec<&str> = input.split('\n').collect();
    let n = (raw_lines.len() as u16).min(MAX_INPUT_LINES);
    let input_lines = &raw_lines[..n as usize];
    let prompt_height = n + 2; // separator + n input lines + status

    // Always clear the maximum possible prompt area (MAX_INPUT_LINES + 2) so
    // that shrinking the prompt (e.g. deleting a newline) doesn't leave stale rows.
    let clear_from = h.saturating_sub(MAX_INPUT_LINES + 2);
    execute!(
        std::io::stdout(),
        cursor::MoveTo(0, clear_from),
        terminal::Clear(terminal::ClearType::FromCursorDown),
    )?;

    let sep_row = h.saturating_sub(prompt_height);
    let status_row = h - 1;

    // Separator
    execute!(
        std::io::stdout(),
        cursor::MoveTo(0, sep_row),
        SetForegroundColor(theme.muted),
        Print("─".repeat(w as usize)),
        ResetColor,
    )?;

    // Resolve agent color once — used for both ▌ bars and the agent name.
    let name_color = {
        let c = agent_color(agent_name);
        if c == crossterm::style::Color::Reset {
            theme.accent
        } else {
            c
        }
    };

    // Input lines
    for (i, line) in input_lines.iter().enumerate() {
        let row = sep_row + 1 + i as u16;
        execute!(std::io::stdout(), cursor::MoveTo(0, row))?;
        if i == 0 {
            execute!(
                std::io::stdout(),
                SetForegroundColor(name_color),
                Print("▌ "),
                ResetColor,
                Print(line),
            )?;
        } else {
            execute!(
                std::io::stdout(),
                SetForegroundColor(theme.muted),
                Print("╎ "),
                ResetColor,
                Print(line),
            )?;
        }
    }

    // Status row: agent-colored bar + agent info (left) + context usage + hint (right)
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
        SetForegroundColor(name_color),
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

    // Park cursor at end of last input line (caller will reposition via move_prompt_cursor)
    let last_row = sep_row + n;
    let last_line_cols = input_lines.last().unwrap_or(&"").chars().count() as u16;
    execute!(
        std::io::stdout(),
        cursor::MoveTo(2 + last_line_cols, last_row)
    )?;
    std::io::stdout().flush()?;
    Ok(prompt_height)
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
    format!("{}% used · {} rem · {} ctx", pct, rem_label, window_label)
}

/// Clear the input portion of the prompt bar (line after `> `) without
/// redrawing the whole bar. Used to blank the line before streaming.
pub fn clear_prompt_input() -> std::io::Result<()> {
    use crossterm::{cursor, execute, terminal};

    let (_, h) = crossterm::terminal::size()?;
    let prompt_y = h.saturating_sub(PROMPT_BAR_HEIGHT);
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

/// Convert our crossterm Color (0.28) to the version termimad bundles (0.29).
/// Both enums have identical named variants so the match is exhaustive for our palette.
fn to_skin_color(c: crossterm::style::Color) -> termimad::crossterm::style::Color {
    use termimad::crossterm::style::Color as TC;
    match c {
        crossterm::style::Color::Black => TC::Black,
        crossterm::style::Color::DarkGrey => TC::DarkGrey,
        crossterm::style::Color::Red => TC::Red,
        crossterm::style::Color::DarkRed => TC::DarkRed,
        crossterm::style::Color::Green => TC::Green,
        crossterm::style::Color::DarkGreen => TC::DarkGreen,
        crossterm::style::Color::Yellow => TC::Yellow,
        crossterm::style::Color::DarkYellow => TC::DarkYellow,
        crossterm::style::Color::Blue => TC::Blue,
        crossterm::style::Color::DarkBlue => TC::DarkBlue,
        crossterm::style::Color::Magenta => TC::Magenta,
        crossterm::style::Color::DarkMagenta => TC::DarkMagenta,
        crossterm::style::Color::Cyan => TC::Cyan,
        crossterm::style::Color::DarkCyan => TC::DarkCyan,
        crossterm::style::Color::White => TC::White,
        crossterm::style::Color::Grey => TC::Grey,
        crossterm::style::Color::Rgb { r, g, b } => TC::Rgb { r, g, b },
        crossterm::style::Color::AnsiValue(v) => TC::AnsiValue(v),
        _ => TC::Reset,
    }
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

    let (code_fg, bold_fg, italic_fg) = (
        to_skin_color(theme.code_fg),
        to_skin_color(theme.bold_fg),
        to_skin_color(theme.italic_fg),
    );
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

/// Convert a crossterm Color to its ANSI foreground escape sequence.
/// Used to embed theme colors into pre-rendered ANSI strings stored in the scroll buffer.
pub fn ansi_fg(c: crossterm::style::Color) -> String {
    use crossterm::style::Color;
    match c {
        Color::Reset => "\x1b[39m".to_string(),
        Color::Black => "\x1b[30m".to_string(),
        Color::DarkGrey => "\x1b[90m".to_string(),
        Color::Red => "\x1b[91m".to_string(),
        Color::DarkRed => "\x1b[31m".to_string(),
        Color::Green => "\x1b[92m".to_string(),
        Color::DarkGreen => "\x1b[32m".to_string(),
        Color::Yellow => "\x1b[93m".to_string(),
        Color::DarkYellow => "\x1b[33m".to_string(),
        Color::Blue => "\x1b[94m".to_string(),
        Color::DarkBlue => "\x1b[34m".to_string(),
        Color::Magenta => "\x1b[95m".to_string(),
        Color::DarkMagenta => "\x1b[35m".to_string(),
        Color::Cyan => "\x1b[96m".to_string(),
        Color::DarkCyan => "\x1b[36m".to_string(),
        Color::White => "\x1b[97m".to_string(),
        Color::Grey => "\x1b[37m".to_string(),
        Color::Rgb { r, g, b } => format!("\x1b[38;2;{r};{g};{b}m"),
        Color::AnsiValue(v) => format!("\x1b[38;5;{v}m"),
    }
}

pub fn render_thoughts(thoughts: &str, theme: &Theme) {
    use crossterm::{
        execute,
        style::{Print, ResetColor, SetForegroundColor},
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
        SetForegroundColor(theme.accent),
        Print(prefix),
        ResetColor,
    );

    let mut lines = trimmed.lines();
    if let Some(first) = lines.next() {
        let _ = execute!(
            stdout(),
            SetForegroundColor(theme.muted),
            Print(first),
            Print("\n"),
            ResetColor,
        );
        for line in lines {
            let _ = execute!(
                stdout(),
                Print(&indent),
                SetForegroundColor(theme.muted),
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
        "fetch" => input
            .get("url")
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

pub fn render_tool_start(name: &str, input: &serde_json::Value, theme: &Theme) {
    use crossterm::style::Stylize;
    let label = format!(" {name} ")
        .with(theme.tool_tag_fg)
        .on(theme.tool_tag_bg);
    let detail = tool_detail(name, input);
    if detail.is_empty() {
        print!("  {label}\r\n");
    } else {
        print!("  {label}  {}\r\n", detail.dark_grey());
    }
    let _ = std::io::stdout().flush();
}

const MAX_TOOL_OUTPUT_LINES: usize = 20;
const MAX_TOOL_LINE_CHARS: usize = 160;

fn render_text_output(output: &str) {
    use crossterm::{
        execute,
        style::{Color, Print, ResetColor, SetForegroundColor},
    };
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return;
    }
    let lines: Vec<&str> = trimmed.lines().collect();
    let truncated = lines.len() > MAX_TOOL_OUTPUT_LINES;
    for line in lines.iter().take(MAX_TOOL_OUTPUT_LINES) {
        let s: String = line
            .trim_end_matches('\r')
            .chars()
            .take(MAX_TOOL_LINE_CHARS)
            .collect();
        let _ = execute!(
            std::io::stdout(),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("    {s}\r\n")),
            ResetColor,
        );
    }
    if truncated {
        let remaining = lines.len() - MAX_TOOL_OUTPUT_LINES;
        let _ = execute!(
            std::io::stdout(),
            SetForegroundColor(Color::DarkGrey),
            Print(format!(
                "    … {} more line{}\r\n",
                remaining,
                if remaining == 1 { "" } else { "s" }
            )),
            ResetColor,
        );
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
        "bash" | "read" | "grep" | "glob" => render_text_output(output),
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

/// Width the splash box should be for `buf` at the current terminal width:
/// starts at the existing default/minimum, grows to fit longer input, capped
/// so it never crowds the terminal edges.
pub fn splash_box_width(buf: &str, term_w: u16) -> u16 {
    let default_w = (term_w * 2 / 3).clamp(52, 90);
    let max_w = term_w.saturating_sub(6).max(default_w);
    let needed_w = buf.chars().count() as u16 + 4; // 2 border chars + 2-space prefix
    needed_w.clamp(default_w, max_w)
}

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

/// Truncate `buf` to fit inside the input box, prefixing with `…` when clipped.
/// Returns the display string. Max visible chars = inner_w - 2 (for the "  " prefix).
fn splash_buf_display(buf: &str, inner_w: usize) -> String {
    let max_chars = inner_w.saturating_sub(2);
    let total = buf.chars().count();
    if total <= max_chars {
        buf.to_string()
    } else {
        // Show the tail, with an ellipsis taking the first slot.
        let skip = total - max_chars + 1;
        let start = buf.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(0);
        format!("…{}", &buf[start..])
    }
}

/// Terminal column for the input cursor given the current buffer and cursor byte position.
pub fn splash_cursor(layout: &SplashLayout, buf: &str, cursor: usize) -> (u16, u16) {
    let inner_w = layout.inner_w();
    let max_chars = inner_w.saturating_sub(2);
    let total_chars = buf.chars().count();
    let cursor_chars = buf[..cursor.min(buf.len())].chars().count();
    let col = if total_chars <= max_chars {
        layout.box_x + 3 + cursor_chars as u16
    } else {
        let skip = total_chars - max_chars + 1;
        if cursor_chars >= skip {
            // +1 because the "…" occupies one display column
            layout.box_x + 3 + 1 + (cursor_chars - skip) as u16
        } else {
            layout.box_x + 3 // cursor before the visible window — clamp to start
        }
    };
    (col, layout.input_row)
}

/// Reposition the terminal cursor to the correct (col, row) within a
/// possibly-multiline prompt input, given the current byte cursor position.
pub fn move_prompt_cursor(input: &str, cursor_byte: usize) -> std::io::Result<()> {
    use crossterm::{cursor, execute};
    let (_, h) = crossterm::terminal::size()?;
    let before = &input[..cursor_byte.min(input.len())];
    let line_idx = before.chars().filter(|&c| c == '\n').count() as u16;
    let col_text = before.rsplit('\n').next().unwrap_or("").chars().count() as u16;
    let total_input_lines =
        (input.chars().filter(|&c| c == '\n').count() as u16 + 1).min(MAX_INPUT_LINES);
    let prompt_height = total_input_lines + 2;
    let sep_row = h.saturating_sub(prompt_height);
    let row = sep_row + 1 + line_idx.min(total_input_lines - 1);
    execute!(std::io::stdout(), cursor::MoveTo(2 + col_text, row))
}

/// Set the terminal scroll region to protect `prompt_height` rows at the bottom.
pub fn handle_resize_with_height(prompt_height: u16) -> std::io::Result<()> {
    let (_, h) = crossterm::terminal::size()?;
    set_scroll_region(1, h.saturating_sub(prompt_height));
    Ok(())
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

    // Use char count, not byte length — █ is 3 bytes but 1 display column.
    let logo_w = IO_LOGO.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let logo_h = IO_LOGO.len() as u16;

    let box_w = splash_box_width(buf, w);
    let inner_w = box_w.saturating_sub(2) as usize;

    let box_small = 4u16; // ╭╮ + input + status + ╰╯
    let block_h = logo_h + 2 + box_small;
    let start_y = h.saturating_sub(block_h) / 2;
    let box_y = start_y + logo_h + 2;

    let box_x = w.saturating_sub(box_w) / 2;
    // Center logo over the box, not the full terminal width.
    let logo_x = box_x + (box_w.saturating_sub(logo_w)) / 2;

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
    let bottom_row = box_y + box_small - 1;

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
        execute!(out, Print(splash_buf_display(buf, inner_w)))?;
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
        execute!(out, Print(splash_buf_display(buf, layout.inner_w())))?;
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

#[allow(clippy::too_many_arguments)]
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

    let name_color = {
        let c = agent_color(agent_name);
        if c == crossterm::style::Color::Reset {
            theme.accent
        } else {
            c
        }
    };
    execute!(
        out,
        cursor::MoveTo(box_x + 1, row),
        Print("  "),
        SetForegroundColor(name_color),
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

#[cfg(test)]
mod tests {
    use super::splash_box_width;

    #[test]
    fn short_input_stays_at_default_width() {
        assert_eq!(splash_box_width("", 100), 66);
        assert_eq!(splash_box_width("hello", 100), 66);
    }

    #[test]
    fn long_input_grows_the_box() {
        let long = "x".repeat(80);
        let grown = splash_box_width(&long, 100);
        assert!(grown > 66, "box should grow past the default width");
        assert_eq!(grown, 84); // 80 chars + 4 (borders + prefix)
    }

    #[test]
    fn very_long_input_clamps_at_the_terminal_width_cap() {
        let huge = "x".repeat(500);
        assert_eq!(splash_box_width(&huge, 100), 94); // 100 - 6
    }

    #[test]
    fn narrow_terminal_never_shrinks_below_default() {
        // term_w so small that term_w - 6 would undercut the default.
        assert_eq!(splash_box_width(&"x".repeat(500), 40), 52);
    }
}
