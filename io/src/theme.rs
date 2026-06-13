use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute, queue,
    style::{Attribute, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use io_runtime::config::Config;
use std::io::{self, Write};

use crate::render::{get_theme, THEME_NAMES};

// Mock diff: (content without prefix, kind)  0=context  1=add  -1=del
const MOCK_DIFF: &[(&str, i8)] = &[
    ("fn greet(name: &str) -> String {", 0),
    ("    format!(\"Hi, {}!\", name)", 1),
    ("    String::from(\"Hello!\")", -1),
    ("    .trim().to_string()", 0),
    ("}", 0),
    ("", 0),
    ("fn farewell(name: &str) {", 0),
    ("    println!(\"Bye, {}!\", name)", 1),
    ("    println!(\"Farewell.\")", -1),
];

// Fixed width for the name column: border + "  ● " + 8-char name + "  " + 5-char badge + " "
const LEFT_W: usize = 20;

pub fn run(current_theme: &str) -> anyhow::Result<&'static str> {
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    let result = theme_picker_loop(&mut stdout, current_theme);
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), cursor::Show);
    result
}

fn theme_picker_loop(stdout: &mut impl Write, current_theme: &str) -> anyhow::Result<&'static str> {
    execute!(stdout, cursor::Hide)?;
    queue!(stdout, terminal::Clear(ClearType::FromCursorDown))?;

    let n = THEME_NAMES.len();
    let mut selected = THEME_NAMES
        .iter()
        .position(|&t| t == current_theme)
        .unwrap_or(0);

    // Reserve space
    let body_rows = n.max(MOCK_DIFF.len());
    let total_rows = body_rows + 3; // top border + body + bottom border
    for _ in 0..total_rows {
        queue!(stdout, Print("\n"))?;
    }
    queue!(stdout, cursor::MoveUp(total_rows as u16))?;
    stdout.flush()?;

    let mut drawn = 0usize;

    loop {
        let (w, _) = terminal::size().unwrap_or((80, 24));
        let box_w = (w.saturating_sub(4) as usize).min(74);

        if drawn > 0 {
            queue!(stdout, cursor::MoveUp(drawn as u16))?;
        }
        drawn = draw_panel(stdout, selected, box_w)?;
        stdout.flush()?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    selected = if selected > 0 { selected - 1 } else { n - 1 };
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    selected = (selected + 1) % n;
                }
                KeyCode::Enter => {
                    queue!(
                        stdout,
                        cursor::MoveUp(drawn as u16),
                        terminal::Clear(ClearType::FromCursorDown)
                    )?;
                    execute!(stdout, cursor::Show)?;
                    let name = THEME_NAMES[selected];
                    let mut config = Config::load()?;
                    config.theme = name.to_string();
                    config.save()?;
                    return Ok(name);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    queue!(
                        stdout,
                        cursor::MoveUp(drawn as u16),
                        terminal::Clear(ClearType::FromCursorDown)
                    )?;
                    execute!(stdout, cursor::Show)?;
                    return Err(crate::picker::Dismissed::Cancelled.into());
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let _ = terminal::disable_raw_mode();
                    execute!(stdout, cursor::Show)?;
                    return Err(crate::picker::Dismissed::Interrupted.into());
                }
                _ => {}
            }
        }
    }
}

fn draw_panel(stdout: &mut impl Write, selected: usize, box_w: usize) -> io::Result<usize> {
    let theme = get_theme(THEME_NAMES[selected]);
    let inner_w = box_w.saturating_sub(2);
    // right_w: inner minus the left name column
    let right_w = inner_w.saturating_sub(LEFT_W);
    let n = THEME_NAMES.len();
    let body_rows = n.max(MOCK_DIFF.len());
    let mut rows = 0usize;

    // ── Top border ────────────────────────────────────────────────
    let title = " Theme ";
    let dashes = inner_w.saturating_sub(title.len() + 1);
    queue!(
        stdout,
        cursor::MoveToColumn(0),
        terminal::Clear(ClearType::CurrentLine),
        SetForegroundColor(theme.accent),
        Print(format!("╭─{title}{:─<width$}╮", "", width = dashes)),
        ResetColor,
        Print("\r\n"),
    )?;
    rows += 1;

    // ── Body rows ─────────────────────────────────────────────────
    for row in 0..body_rows {
        queue!(
            stdout,
            cursor::MoveToColumn(0),
            terminal::Clear(ClearType::CurrentLine)
        )?;

        // Left border
        queue!(
            stdout,
            SetForegroundColor(theme.accent),
            Print("│"),
            ResetColor
        )?;

        // Theme name column — each name shown in its own accent color + dark/light badge
        if row < n {
            let name = THEME_NAMES[row];
            let t = get_theme(name);
            let indicator = if row == selected { "●" } else { "○" };
            let badge = if t.dark { "dark " } else { "light" };
            // "  ● default   dark  " fits in LEFT_W=20
            let raw_label = format!("  {} {:<8}  {}  ", indicator, name, badge);
            let label: String = if raw_label.chars().count() > LEFT_W {
                raw_label.chars().take(LEFT_W).collect()
            } else {
                format!("{:<width$}", raw_label, width = LEFT_W)
            };
            // Split label: name portion vs badge (last 7 chars "badge  ")
            let name_part: String = label.chars().take(LEFT_W - 7).collect();
            let badge_part: String = label.chars().skip(LEFT_W - 7).collect();
            let badge_color = if t.dark {
                crossterm::style::Color::DarkGrey
            } else {
                crossterm::style::Color::DarkYellow
            };
            if row == selected {
                queue!(
                    stdout,
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(t.accent),
                    Print(&name_part),
                    SetAttribute(Attribute::Reset),
                    SetForegroundColor(badge_color),
                    Print(&badge_part),
                    ResetColor,
                )?;
            } else {
                queue!(
                    stdout,
                    SetForegroundColor(t.accent),
                    Print(&name_part),
                    SetForegroundColor(badge_color),
                    Print(&badge_part),
                    ResetColor,
                )?;
            }
        } else {
            queue!(stdout, Print(format!("{:<LEFT_W$}", "")))?;
        }

        // Diff preview
        if row < MOCK_DIFF.len() {
            let (content, kind) = MOCK_DIFF[row];
            let prefix = match kind {
                1 => theme.diff_add_prefix,
                -1 => theme.diff_del_prefix,
                _ => " ",
            };
            let line = if kind == 0 {
                format!("  {content}")
            } else {
                format!(" {prefix} {content}")
            };
            // Truncate to right_w, then pad
            let visible: String = line.chars().take(right_w.saturating_sub(1)).collect();
            let pad = right_w
                .saturating_sub(1)
                .saturating_sub(visible.chars().count());

            match kind {
                1 => queue!(
                    stdout,
                    SetForegroundColor(theme.diff_add_fg),
                    SetBackgroundColor(theme.diff_add_bg),
                    Print(format!("{}{:>pad$}", visible, "")),
                    ResetColor,
                )?,
                -1 => queue!(
                    stdout,
                    SetForegroundColor(theme.diff_del_fg),
                    SetBackgroundColor(theme.diff_del_bg),
                    Print(format!("{}{:>pad$}", visible, "")),
                    ResetColor,
                )?,
                _ => queue!(
                    stdout,
                    SetForegroundColor(theme.muted),
                    Print(format!("{}{:>pad$}", visible, "")),
                    ResetColor,
                )?,
            }
            // fill remaining to border
            let fill = right_w.saturating_sub(visible.chars().count() + pad + 1);
            queue!(stdout, Print(format!("{:fill$}", "")))?;
        } else {
            queue!(stdout, Print(format!("{:<width$}", "", width = right_w)))?;
        }

        // Right border
        queue!(
            stdout,
            SetForegroundColor(theme.accent),
            Print("│"),
            ResetColor
        )?;
        queue!(stdout, Print("\r\n"))?;
        rows += 1;
    }

    // ── Bottom border with nav hint ───────────────────────────────
    let nav = " ↑↓ navigate  Enter select  Esc cancel ";
    let remaining = inner_w.saturating_sub(nav.len());
    let left_dashes = remaining / 2;
    let right_dashes = remaining - left_dashes;
    queue!(
        stdout,
        cursor::MoveToColumn(0),
        terminal::Clear(ClearType::CurrentLine),
        SetForegroundColor(theme.accent),
        Print(format!("╰{:─<ld$}", "", ld = left_dashes)),
        ResetColor,
        SetForegroundColor(theme.muted),
        Print(nav),
        SetForegroundColor(theme.accent),
        Print(format!("{:─<rd$}╯", "", rd = right_dashes)),
        ResetColor,
        Print("\r\n"),
    )?;
    rows += 1;

    Ok(rows)
}
