use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute, queue,
    style::{Color, Stylize},
    terminal::{self, ClearType},
};
use std::io::{self, Write};

const VIEWPORT: usize = 20;

/// The user dismissed the picker without choosing. Callers detect this with
/// `err.is::<Dismissed>()` instead of matching on message strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Dismissed {
    /// Esc or `q` — back out of the picker.
    #[error("cancelled")]
    Cancelled,
    /// Ctrl+C — interrupt.
    #[error("interrupted")]
    Interrupted,
}

/// Show an interactive arrow-key list. Returns the selected index.
/// `current` highlights the already-active item in green even when not focused.
pub fn pick(items: &[&str], current: Option<usize>) -> anyhow::Result<usize> {
    if items.is_empty() {
        anyhow::bail!("no items to pick from");
    }
    // Preserve the caller's raw-mode state: inside the TUI raw mode is
    // already on, and disabling it here would leave the terminal cooked
    // mid-session while mouse capture stays enabled (raw SGR garbage on
    // mouse move). The guard only disables when it did the enabling.
    let _raw = crate::raw::RawModeGuard::acquire()?;
    let mut stdout = io::stdout();
    let result = pick_loop(&mut stdout, items, current);
    let _ = execute!(io::stdout(), cursor::Show);
    result
}

/// Show an interactive arrow-key list with a secondary hint shown in dark grey on the right.
/// `items[i]` is `(primary_label, hint)`. Returns the selected index.
pub fn pick_with_hint(items: &[(&str, &str)], current: Option<usize>) -> anyhow::Result<usize> {
    if items.is_empty() {
        anyhow::bail!("no items to pick from");
    }
    let _raw = crate::raw::RawModeGuard::acquire()?;
    let mut stdout = io::stdout();
    let result = pick_hint_loop(&mut stdout, items, current, None, current.unwrap_or(0));
    let _ = execute!(io::stdout(), cursor::Show);
    result
}

/// Show an interactive arrow-key list with a title line above the items.
/// `initial` is the index preselected when the picker opens (used by the
/// permission modal to default to the safe choice, e.g. deny). Returns the
/// selected index, or [`Dismissed`] when the user cancels.
pub fn pick_permission(
    title: &str,
    items: &[(&str, &str)],
    initial: usize,
) -> anyhow::Result<usize> {
    if items.is_empty() {
        anyhow::bail!("no items to pick from");
    }
    let _raw = crate::raw::RawModeGuard::acquire()?;
    let mut stdout = io::stdout();
    let result = pick_hint_loop(&mut stdout, items, None, Some(title), initial);
    let _ = execute!(io::stdout(), cursor::Show);
    result
}

// ── shared cleanup ────────────────────────────────────────────────────────────

/// Move back to the top of the picker area and wipe everything drawn.
/// `lines` is the number of item rows (NOT including the status bar — the
/// status bar sits on the same row the cursor lands on after `lines` items,
/// so `MoveUp(lines)` returns exactly to the top and `FromCursorDown` erases
/// items + status in one shot).
fn clear_picker(stdout: &mut impl Write, lines: usize) -> io::Result<()> {
    if lines > 0 {
        queue!(stdout, cursor::MoveUp(lines as u16))?;
    }
    queue!(stdout, terminal::Clear(ClearType::FromCursorDown))?;
    Ok(())
}

// ── pick_with_hint ────────────────────────────────────────────────────────────

fn pick_hint_loop(
    stdout: &mut impl Write,
    items: &[(&str, &str)],
    current: Option<usize>,
    title: Option<&str>,
    initial: usize,
) -> anyhow::Result<usize> {
    execute!(stdout, cursor::Hide)?;

    // Wipe any leftover content from readline or a previous picker invocation.
    queue!(stdout, terminal::Clear(ClearType::FromCursorDown))?;

    let col = items.iter().map(|(l, _)| l.len()).max().unwrap_or(0) + 4;

    let mut selected = initial.min(items.len().saturating_sub(1));
    let mut viewport_start = selected.saturating_sub(VIEWPORT / 2);

    let title_rows = usize::from(title.is_some());

    // Reserve rows: title + items + status bar.
    let reserve = (VIEWPORT + 2).min(items.len() + 2) + title_rows;
    for _ in 0..reserve {
        queue!(stdout, crossterm::style::Print("\n"))?;
    }
    queue!(stdout, cursor::MoveUp(reserve as u16))?;
    stdout.flush()?;

    // Rows drawn in the last frame (title + items). The status bar sits on the
    // row after the items, so MoveUp(drawn_rows) returns to the very top of the
    // picker area on redraw.
    let mut drawn_rows = 0usize;

    loop {
        if selected < viewport_start {
            viewport_start = selected;
        } else if selected >= viewport_start + VIEWPORT {
            viewport_start = selected + 1 - VIEWPORT;
        }

        // Return cursor to the top of the picker area.
        if drawn_rows > 0 {
            queue!(stdout, cursor::MoveUp(drawn_rows as u16))?;
        }

        // Title line, drawn above the items.
        if let Some(title) = title {
            queue!(
                stdout,
                cursor::MoveToColumn(0),
                terminal::Clear(ClearType::CurrentLine),
                crossterm::style::PrintStyledContent(title.with(Color::Yellow).bold()),
                cursor::MoveToNextLine(1),
            )?;
        }

        let visible = VIEWPORT.min(items.len().saturating_sub(viewport_start));

        for i in 0..visible {
            let idx = viewport_start + i;
            let (label, hint) = items[idx];
            let pad = " ".repeat(col.saturating_sub(label.len()));
            queue!(
                stdout,
                cursor::MoveToColumn(0),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            if idx == selected {
                queue!(
                    stdout,
                    crossterm::style::PrintStyledContent("  ● ".with(Color::Cyan)),
                    crossterm::style::PrintStyledContent(label.with(Color::Cyan).bold()),
                    crossterm::style::Print(&pad),
                    crossterm::style::PrintStyledContent(hint.dark_grey()),
                )?;
            } else if Some(idx) == current {
                queue!(
                    stdout,
                    crossterm::style::PrintStyledContent("  ○  ".with(Color::Green)),
                    crossterm::style::PrintStyledContent(label.with(Color::Green)),
                    crossterm::style::Print(&pad),
                    crossterm::style::PrintStyledContent(hint.dark_grey()),
                )?;
            } else {
                queue!(
                    stdout,
                    crossterm::style::PrintStyledContent("  ○  ".dark_grey()),
                    crossterm::style::Print(label),
                    crossterm::style::Print(&pad),
                    crossterm::style::PrintStyledContent(hint.dark_grey()),
                )?;
            }
            // Advance to the next row.
            queue!(stdout, cursor::MoveToNextLine(1))?;
        }

        // Status bar sits at cursor position after the last item row.
        queue!(
            stdout,
            cursor::MoveToColumn(0),
            terminal::Clear(ClearType::CurrentLine),
            crossterm::style::PrintStyledContent(
                format!(
                    "  {}/{}  ↑↓ or j/k  Enter select  Esc cancel",
                    selected + 1,
                    items.len()
                )
                .dark_grey()
            )
        )?;
        stdout.flush()?;

        // Cursor is now at the status bar row; MoveUp(drawn_rows) on the next
        // frame returns exactly to the top of the picker area.
        drawn_rows = visible + title_rows;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    selected = if selected > 0 {
                        selected - 1
                    } else {
                        items.len() - 1
                    };
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    selected = (selected + 1) % items.len();
                }
                KeyCode::Home => selected = 0,
                KeyCode::End => selected = items.len() - 1,
                KeyCode::Enter => {
                    clear_picker(stdout, drawn_rows)?;
                    execute!(stdout, cursor::Show)?;
                    stdout.flush()?;
                    return Ok(selected);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    clear_picker(stdout, drawn_rows)?;
                    execute!(stdout, cursor::Show)?;
                    stdout.flush()?;
                    return Err(Dismissed::Cancelled.into());
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    execute!(stdout, cursor::Show)?;
                    stdout.flush()?;
                    return Err(Dismissed::Interrupted.into());
                }
                _ => {}
            }
        }
    }
}

// ── pick (no hint) ────────────────────────────────────────────────────────────

fn pick_loop(
    stdout: &mut impl Write,
    items: &[&str],
    current: Option<usize>,
) -> anyhow::Result<usize> {
    execute!(stdout, cursor::Hide)?;

    queue!(stdout, terminal::Clear(ClearType::FromCursorDown))?;

    let mut selected = current.unwrap_or(0).min(items.len().saturating_sub(1));
    let mut viewport_start = selected.saturating_sub(VIEWPORT / 2);

    let reserve = (VIEWPORT + 2).min(items.len() + 2);
    for _ in 0..reserve {
        queue!(stdout, crossterm::style::Print("\n"))?;
    }
    queue!(stdout, cursor::MoveUp(reserve as u16))?;
    stdout.flush()?;

    let mut drawn_items = 0usize;

    loop {
        if selected < viewport_start {
            viewport_start = selected;
        } else if selected >= viewport_start + VIEWPORT {
            viewport_start = selected + 1 - VIEWPORT;
        }

        if drawn_items > 0 {
            queue!(stdout, cursor::MoveUp(drawn_items as u16))?;
        }

        let visible = VIEWPORT.min(items.len().saturating_sub(viewport_start));

        for i in 0..visible {
            let idx = viewport_start + i;
            let label = items[idx];
            queue!(
                stdout,
                cursor::MoveToColumn(0),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            if idx == selected {
                queue!(
                    stdout,
                    crossterm::style::PrintStyledContent("  ● ".with(Color::Cyan)),
                    crossterm::style::PrintStyledContent(label.with(Color::Cyan).bold())
                )?;
            } else if Some(idx) == current {
                queue!(
                    stdout,
                    crossterm::style::PrintStyledContent("  ○  ".with(Color::Green)),
                    crossterm::style::PrintStyledContent(label.with(Color::Green))
                )?;
            } else {
                queue!(
                    stdout,
                    crossterm::style::PrintStyledContent("  ○  ".dark_grey()),
                    crossterm::style::Print(label)
                )?;
            }
            queue!(stdout, cursor::MoveToNextLine(1))?;
        }

        queue!(
            stdout,
            cursor::MoveToColumn(0),
            terminal::Clear(ClearType::CurrentLine),
            crossterm::style::PrintStyledContent(
                format!(
                    "  {}/{}  ↑↓ or j/k  Enter select  Esc cancel",
                    selected + 1,
                    items.len()
                )
                .dark_grey()
            )
        )?;
        stdout.flush()?;

        drawn_items = visible;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    selected = if selected > 0 {
                        selected - 1
                    } else {
                        items.len() - 1
                    };
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    selected = (selected + 1) % items.len();
                }
                KeyCode::Home => selected = 0,
                KeyCode::End => selected = items.len() - 1,
                KeyCode::Enter => {
                    clear_picker(stdout, drawn_items)?;
                    execute!(stdout, cursor::Show)?;
                    stdout.flush()?;
                    return Ok(selected);
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    clear_picker(stdout, drawn_items)?;
                    execute!(stdout, cursor::Show)?;
                    stdout.flush()?;
                    return Err(Dismissed::Cancelled.into());
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    execute!(stdout, cursor::Show)?;
                    stdout.flush()?;
                    return Err(Dismissed::Interrupted.into());
                }
                _ => {}
            }
        }
    }
}
