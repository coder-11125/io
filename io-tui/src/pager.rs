//! A full-screen scrollable viewer, used for the Ctrl+O transcript.

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    execute,
    style::{Print, ResetColor, SetForegroundColor},
    terminal,
};
use std::io::Write;

/// The largest scroll offset that still fills the screen.
pub fn max_top(total: usize, height: usize) -> usize {
    total.saturating_sub(height)
}

/// The lines visible at `top`, each cut to `width` characters.
pub fn visible(lines: &[String], top: usize, height: usize, width: usize) -> Vec<String> {
    lines
        .iter()
        .skip(top.min(max_top(lines.len(), height)))
        .take(height)
        .map(|l| l.chars().take(width).collect())
        .collect()
}

/// Show `lines` until the user closes the viewer. Expects raw mode and the
/// alternate screen (the REPL's normal state); the caller redraws afterwards.
pub fn show(lines: &[String], title: &str) -> std::io::Result<()> {
    let mut top = usize::MAX; // start at the end: the latest activity
    loop {
        let (w, h) = terminal::size()?;
        let (w, h) = (w as usize, h as usize);
        let body = h.saturating_sub(1).max(1);
        top = top.min(max_top(lines.len(), body));
        let mut out = std::io::stdout();
        execute!(out, terminal::Clear(terminal::ClearType::All), cursor::Hide)?;
        for (i, line) in visible(lines, top, body, w).iter().enumerate() {
            execute!(out, cursor::MoveTo(0, i as u16), Print(line))?;
        }
        let shown = (top + body).min(lines.len());
        let footer = format!(
            " {title} · {shown}/{} · ↑↓ j k PgUp PgDn g G scroll · q or Esc closes ",
            lines.len()
        );
        execute!(
            out,
            cursor::MoveTo(0, (h - 1) as u16),
            SetForegroundColor(crossterm::style::Color::DarkGrey),
            Print(footer.chars().take(w).collect::<String>()),
            ResetColor
        )?;
        out.flush()?;

        let max = max_top(lines.len(), body);
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => match (k.code, k.modifiers) {
                (KeyCode::Char('q') | KeyCode::Esc, _)
                | (KeyCode::Char('o' | 'c'), KeyModifiers::CONTROL) => return Ok(()),
                (KeyCode::Up | KeyCode::Char('k'), _) => top = top.saturating_sub(1),
                (KeyCode::Down | KeyCode::Char('j'), _) => top = (top + 1).min(max),
                (KeyCode::PageUp | KeyCode::Char('b'), _) => top = top.saturating_sub(body),
                (KeyCode::PageDown | KeyCode::Char(' '), _) => top = (top + body).min(max),
                (KeyCode::Home | KeyCode::Char('g'), _) => top = 0,
                (KeyCode::End | KeyCode::Char('G'), _) => top = max,
                _ => {}
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => top = top.saturating_sub(3),
                MouseEventKind::ScrollDown => top = (top + 3).min(max),
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    #[test]
    fn the_last_screenful_is_the_furthest_you_can_scroll() {
        assert_eq!(max_top(100, 20), 80);
        assert_eq!(max_top(10, 20), 0);
        assert_eq!(max_top(0, 20), 0);
    }

    #[test]
    fn visible_cuts_to_the_window_and_the_width() {
        let l = lines(50);
        let v = visible(&l, 10, 3, 6);
        assert_eq!(v, ["line 1", "line 1", "line 1"].map(String::from));
        assert_eq!(
            visible(&l, 0, 2, 100),
            ["line 0", "line 1"].map(String::from)
        );
        // Past the end clamps to the last full screen.
        assert_eq!(
            visible(&l, usize::MAX, 2, 100),
            ["line 48", "line 49"].map(String::from)
        );
        assert_eq!(visible(&l[..1], 5, 10, 100).len(), 1);
    }
}
