//! In-TUI modal popups: a single-line text field and a background-wait
//! spinner. Both render as a bordered box drawn in place (same technique as
//! `picker.rs`) and never touch the alternate screen or raw-mode state
//! beyond what [`crate::raw::RawModeGuard`] already manages — so `/connect`
//! and `/login` can stay inside the TUI instead of shelling out to a cooked
//! terminal.

use crate::picker::Dismissed;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute, queue,
    style::{
        Color, Print, PrintStyledContent, ResetColor, SetBackgroundColor, SetForegroundColor,
        Stylize,
    },
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Show a bordered text-input popup. `header` lines are informational context
/// (e.g. an OAuth URL) shown above the field; long lines wrap rather than
/// truncate so a URL stays fully readable/copyable. The field starts
/// pre-filled with `default` (editable). `secret` masks typed characters.
/// Returns the entered text, or [`Dismissed`] on Ctrl+C.
///
/// Ctrl+C, not Esc, is the reject key here: these popups run for as long as
/// an OAuth/API-key flow takes, during which mouse capture stays on, and on
/// terminals that report mouse motion continuously a bare Esc keypress can
/// race an in-flight mouse-motion escape sequence and get silently absorbed.
/// Ctrl+C (byte 0x03) has no such ambiguity.
pub fn text_prompt(header: &[&str], default: &str, secret: bool) -> anyhow::Result<String> {
    let _raw = crate::raw::RawModeGuard::acquire()?;
    let mut stdout = io::stdout();
    let result = text_prompt_loop(&mut stdout, header, default, secret);
    let _ = execute!(io::stdout(), cursor::Show);
    result
}

/// Block until `rx` yields a value (produced by a background task, typically
/// `tokio::spawn`'d by the caller), showing a spinner box in the meantime.
/// Ctrl+C returns [`Dismissed`] without waiting for the background task —
/// see [`text_prompt`] for why Ctrl+C rather than Esc is the reject key here.
pub fn wait_for<T>(header: &[&str], rx: Receiver<T>) -> anyhow::Result<T> {
    let _raw = crate::raw::RawModeGuard::acquire()?;
    let mut stdout = io::stdout();
    let result = wait_loop(&mut stdout, header, rx);
    let _ = execute!(io::stdout(), cursor::Show);
    result
}

// ── layout helpers ────────────────────────────────────────────────────────────

fn field_width(header: &[&str], extra: &str) -> (usize, usize) {
    let (term_w, _) = terminal::size().unwrap_or((80, 24));
    let max_field = (term_w as usize).saturating_sub(6).clamp(24, 90);
    let natural = header
        .iter()
        .map(|l| l.chars().count() + 1)
        .max()
        .unwrap_or(0)
        .max(extra.chars().count() + 1)
        .max(30);
    let field_w = natural.min(max_field);
    let text_w = field_w.saturating_sub(1).max(1);
    (field_w, text_w)
}

fn wrap_lines(lines: &[&str], width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in lines {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        for chunk in chars.chunks(width) {
            out.push(chunk.iter().collect());
        }
    }
    out
}

fn draw_top_border(stdout: &mut impl Write, field_w: usize) -> io::Result<()> {
    queue!(
        stdout,
        cursor::MoveToColumn(0),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("╭{}╮", "─".repeat(field_w))),
        ResetColor,
        cursor::MoveToNextLine(1),
    )
}

fn draw_bottom_border(stdout: &mut impl Write, field_w: usize) -> io::Result<()> {
    queue!(
        stdout,
        cursor::MoveToColumn(0),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("╰{}╯", "─".repeat(field_w))),
        ResetColor,
        cursor::MoveToNextLine(1),
    )
}

fn draw_text_row(stdout: &mut impl Write, text: &str, text_w: usize) -> io::Result<()> {
    let count = text.chars().count().min(text_w);
    let shown: String = text.chars().take(count).collect();
    let pad = text_w.saturating_sub(count);
    queue!(
        stdout,
        cursor::MoveToColumn(0),
        SetForegroundColor(Color::DarkGrey),
        Print("│ "),
        ResetColor,
        Print(&shown),
        Print(" ".repeat(pad)),
        SetForegroundColor(Color::DarkGrey),
        Print("│"),
        ResetColor,
        cursor::MoveToNextLine(1),
    )
}

fn draw_hint_row(stdout: &mut impl Write, hint: &str) -> io::Result<()> {
    queue!(
        stdout,
        cursor::MoveToColumn(0),
        terminal::Clear(ClearType::CurrentLine),
        PrintStyledContent(format!("  {hint}").dark_grey()),
        cursor::MoveToNextLine(1),
    )
}

fn clear_box(stdout: &mut impl Write, rows: u16) -> io::Result<()> {
    if rows > 0 {
        queue!(stdout, cursor::MoveUp(rows))?;
    }
    queue!(stdout, terminal::Clear(ClearType::FromCursorDown))
}

// ── text_prompt ────────────────────────────────────────────────────────────────

fn draw_input_row(
    stdout: &mut impl Write,
    chars: &[char],
    cursor_i: usize,
    view_start: &mut usize,
    secret: bool,
    text_w: usize,
) -> io::Result<()> {
    if cursor_i < *view_start {
        *view_start = cursor_i;
    } else if cursor_i >= *view_start + text_w {
        *view_start = cursor_i + 1 - text_w;
    }
    let end = (*view_start + text_w).min(chars.len());
    let window = &chars[*view_start..end];

    queue!(
        stdout,
        cursor::MoveToColumn(0),
        SetForegroundColor(Color::DarkGrey),
        Print("│ "),
        ResetColor,
    )?;

    let mut printed = 0usize;
    for (i, &ch) in window.iter().enumerate() {
        let idx = *view_start + i;
        let shown = if secret { '•' } else { ch };
        if idx == cursor_i {
            queue!(
                stdout,
                SetForegroundColor(Color::Black),
                SetBackgroundColor(Color::Cyan),
                Print(shown),
                ResetColor,
            )?;
        } else {
            queue!(stdout, Print(shown))?;
        }
        printed += 1;
    }
    if cursor_i == chars.len() && cursor_i >= *view_start && cursor_i < *view_start + text_w {
        queue!(
            stdout,
            SetForegroundColor(Color::Black),
            SetBackgroundColor(Color::Cyan),
            Print(' '),
            ResetColor,
        )?;
        printed += 1;
    }
    if printed < text_w {
        queue!(stdout, Print(" ".repeat(text_w - printed)))?;
    }
    queue!(
        stdout,
        SetForegroundColor(Color::DarkGrey),
        Print("│"),
        ResetColor,
        cursor::MoveToNextLine(1),
    )
}

fn text_prompt_loop(
    stdout: &mut impl Write,
    header: &[&str],
    default: &str,
    secret: bool,
) -> anyhow::Result<String> {
    execute!(stdout, cursor::Hide)?;
    queue!(stdout, terminal::Clear(ClearType::FromCursorDown))?;

    let (field_w, text_w) = field_width(header, default);
    let header_wrapped = wrap_lines(header, text_w);

    let mut chars: Vec<char> = default.chars().collect();
    let mut cursor_i = chars.len();
    let mut view_start = 0usize;

    let rows = header_wrapped.len() as u16 + 4;
    for _ in 0..rows {
        queue!(stdout, Print("\n"))?;
    }
    queue!(stdout, cursor::MoveUp(rows))?;
    stdout.flush()?;

    loop {
        draw_top_border(stdout, field_w)?;
        for line in &header_wrapped {
            draw_text_row(stdout, line, text_w)?;
        }
        draw_input_row(stdout, &chars, cursor_i, &mut view_start, secret, text_w)?;
        draw_bottom_border(stdout, field_w)?;
        draw_hint_row(stdout, "Enter confirm   Ctrl+C cancel")?;
        stdout.flush()?;

        if let Event::Key(key) = event::read()? {
            if key.kind == crossterm::event::KeyEventKind::Release {
                queue!(stdout, cursor::MoveUp(rows))?;
                continue;
            }
            match (key.code, key.modifiers) {
                (KeyCode::Enter, _) => {
                    clear_box(stdout, rows)?;
                    execute!(stdout, cursor::Show)?;
                    stdout.flush()?;
                    return Ok(chars.into_iter().collect());
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    clear_box(stdout, rows)?;
                    execute!(stdout, cursor::Show)?;
                    stdout.flush()?;
                    return Err(Dismissed::Interrupted.into());
                }
                (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    chars.clear();
                    cursor_i = 0;
                }
                (KeyCode::Char('a'), KeyModifiers::CONTROL) | (KeyCode::Home, _) => {
                    cursor_i = 0;
                }
                (KeyCode::Char('e'), KeyModifiers::CONTROL) | (KeyCode::End, _) => {
                    cursor_i = chars.len();
                }
                (KeyCode::Backspace, _) if cursor_i > 0 => {
                    chars.remove(cursor_i - 1);
                    cursor_i -= 1;
                }
                (KeyCode::Delete, _) if cursor_i < chars.len() => {
                    chars.remove(cursor_i);
                }
                (KeyCode::Left, _) if cursor_i > 0 => cursor_i -= 1,
                (KeyCode::Right, _) if cursor_i < chars.len() => cursor_i += 1,
                (KeyCode::Char(c), m)
                    if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
                {
                    chars.insert(cursor_i, c);
                    cursor_i += 1;
                }
                _ => {}
            }
        }
        queue!(stdout, cursor::MoveUp(rows))?;
    }
}

// ── wait_for ──────────────────────────────────────────────────────────────────

fn wait_loop<T>(stdout: &mut impl Write, header: &[&str], rx: Receiver<T>) -> anyhow::Result<T> {
    execute!(stdout, cursor::Hide)?;
    queue!(stdout, terminal::Clear(ClearType::FromCursorDown))?;

    let (field_w, text_w) = field_width(header, "");
    let header_wrapped = wrap_lines(header, text_w);

    let rows = header_wrapped.len() as u16 + 4;
    for _ in 0..rows {
        queue!(stdout, Print("\n"))?;
    }
    queue!(stdout, cursor::MoveUp(rows))?;
    stdout.flush()?;

    let mut frame = 0usize;
    loop {
        draw_top_border(stdout, field_w)?;
        for line in &header_wrapped {
            draw_text_row(stdout, line, text_w)?;
        }
        let spin = format!("{} working…", SPINNER[frame % SPINNER.len()]);
        draw_text_row(stdout, &spin, text_w)?;
        draw_bottom_border(stdout, field_w)?;
        draw_hint_row(stdout, "Ctrl+C cancel")?;
        stdout.flush()?;
        frame += 1;

        match rx.try_recv() {
            Ok(v) => {
                clear_box(stdout, rows)?;
                stdout.flush()?;
                return Ok(v);
            }
            Err(TryRecvError::Disconnected) => {
                clear_box(stdout, rows)?;
                stdout.flush()?;
                anyhow::bail!("background task ended unexpectedly");
            }
            Err(TryRecvError::Empty) => {}
        }

        if event::poll(Duration::from_millis(90))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    clear_box(stdout, rows)?;
                    stdout.flush()?;
                    return Err(Dismissed::Interrupted.into());
                }
            }
        }

        queue!(stdout, cursor::MoveUp(rows))?;
    }
}
