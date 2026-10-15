//! A centered, scrollable popup for read-only command output (`/help`,
//! `/status`, …), drawn over a dimmed copy of the screen behind it. Esc (or
//! `q`, Enter, Ctrl+C) closes it and hands focus back to the caller, which
//! redraws its own screen.

use crate::render::{draw_splash, Theme};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    execute, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal,
};
use std::io::Write;

const MIN_WIDTH: usize = 40;
const SCROLL_STEP: usize = 3;

/// Where the popup box sits and how much of the content it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub x: u16,
    pub y: u16,
    /// Outer width, borders included.
    pub width: u16,
    /// Content rows (the box is two taller: top and bottom border).
    pub body: u16,
}

impl Geometry {
    /// Text columns inside the box: the width minus borders and one space of
    /// padding on each side.
    pub fn text_width(&self) -> usize {
        (self.width as usize).saturating_sub(4)
    }
}

/// Size and center the box for `lines` and `title` on a `term_w` × `term_h`
/// screen: as wide as the longest line needs (at least [`MIN_WIDTH`]), as tall
/// as the content, never past the screen edges.
pub fn geometry(term_w: u16, term_h: u16, lines: &[String], title: &str) -> Geometry {
    let longest = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .max(title.chars().count() + 2);
    let width = (longest + 4)
        .max(MIN_WIDTH)
        .min((term_w as usize).saturating_sub(4).max(8));
    let body = lines
        .len()
        .max(1)
        .min((term_h as usize).saturating_sub(4).max(1));
    let width = width as u16;
    let body = body as u16;
    Geometry {
        x: term_w.saturating_sub(width) / 2,
        y: term_h.saturating_sub(body + 2) / 2,
        width,
        body,
    }
}

/// The splash screen drawn in grey, for use as a popup's background. (The
/// terminal's "dim" attribute would not survive: every `ResetColor` the
/// splash code emits is `ESC[0m`, which cancels it.)
pub fn draw_dimmed_splash(
    agent_name: &str,
    provider_id: &str,
    model_id: &str,
    theme: &Theme,
) -> std::io::Result<()> {
    let grey = Theme {
        accent: Color::DarkGrey,
        muted: Color::DarkGrey,
        ..*theme
    };
    draw_splash("", agent_name, provider_id, model_id, &grey)?;
    Ok(())
}

fn max_top(total: usize, body: usize) -> usize {
    total.saturating_sub(body)
}

fn draw(
    out: &mut impl Write,
    title: &str,
    lines: &[String],
    top: usize,
    g: Geometry,
) -> std::io::Result<()> {
    let inner = g.width as usize - 2;
    let text_w = g.text_width();
    let border = SetForegroundColor(Color::DarkGrey);

    // Plain intensity for the box itself, whatever was drawn before it.
    queue!(out, SetAttribute(Attribute::NormalIntensity), cursor::Hide)?;

    let title_text: String = format!(" {title} ").chars().take(inner).collect();
    let rule = inner.saturating_sub(title_text.chars().count() + 1);
    queue!(
        out,
        cursor::MoveTo(g.x, g.y),
        border,
        Print("╭─"),
        ResetColor,
        SetAttribute(Attribute::Bold),
        Print(&title_text),
        SetAttribute(Attribute::NormalIntensity),
        border,
        Print(format!("{}╮", "─".repeat(rule))),
        ResetColor,
    )?;

    let body = g.body as usize;
    let top = top.min(max_top(lines.len(), body));
    for row in 0..body {
        let text: String = lines
            .get(top + row)
            .map(|l| l.chars().take(text_w).collect())
            .unwrap_or_default();
        let pad = text_w.saturating_sub(text.chars().count());
        queue!(
            out,
            cursor::MoveTo(g.x, g.y + 1 + row as u16),
            border,
            Print("│ "),
            ResetColor,
            SetBackgroundColor(Color::Reset),
            Print(&text),
            Print(" ".repeat(pad)),
            border,
            Print(" │"),
            ResetColor,
        )?;
    }

    let hint = if lines.len() > body {
        format!(
            " {}-{}/{} · ↑↓ scroll · Esc close ",
            top + 1,
            (top + body).min(lines.len()),
            lines.len()
        )
    } else {
        " Esc close ".to_string()
    };
    let hint: String = hint.chars().take(inner).collect();
    let rule = inner.saturating_sub(hint.chars().count() + 1);
    queue!(
        out,
        cursor::MoveTo(g.x, g.y + 1 + g.body),
        border,
        Print(format!("╰{}", "─".repeat(rule))),
        Print(&hint),
        Print("─╯"),
        ResetColor,
    )?;
    out.flush()
}

/// Show `lines` in a popup until the user closes it. `redraw_background` paints
/// the screen behind the box (and clears it); it runs again after a resize.
/// Expects raw mode (the TUI's normal state); the caller redraws afterwards.
pub fn show(
    title: &str,
    lines: &[String],
    redraw_background: &mut dyn FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut top = 0usize;
    let result = (|| {
        loop {
            let (w, h) = terminal::size()?;
            let g = geometry(w, h, lines, title);
            let body = g.body as usize;
            top = top.min(max_top(lines.len(), body));
            redraw_background()?;
            draw(&mut std::io::stdout(), title, lines, top, g)?;

            // Scroll or resize without repainting the background each key.
            loop {
                let max = max_top(lines.len(), body);
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => {
                        match (k.code, k.modifiers) {
                            (KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter, _)
                            | (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) => return Ok(()),
                            (KeyCode::Up | KeyCode::Char('k'), _) => top = top.saturating_sub(1),
                            (KeyCode::Down | KeyCode::Char('j'), _) => top = (top + 1).min(max),
                            (KeyCode::PageUp | KeyCode::Char('b'), _) => {
                                top = top.saturating_sub(body)
                            }
                            (KeyCode::PageDown | KeyCode::Char(' '), _) => {
                                top = (top + body).min(max)
                            }
                            (KeyCode::Home | KeyCode::Char('g'), _) => top = 0,
                            (KeyCode::End | KeyCode::Char('G'), _) => top = max,
                            _ => continue,
                        }
                    }
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => top = top.saturating_sub(SCROLL_STEP),
                        MouseEventKind::ScrollDown => top = (top + SCROLL_STEP).min(max),
                        _ => continue,
                    },
                    Event::Resize(_, _) => break,
                    _ => continue,
                }
                draw(&mut std::io::stdout(), title, lines, top, g)?;
            }
        }
    })();
    // Leave no attribute behind for whatever draws next.
    let _ = execute!(
        std::io::stdout(),
        SetAttribute(Attribute::Reset),
        ResetColor
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize, len: usize) -> Vec<String> {
        (0..n).map(|_| "x".repeat(len)).collect()
    }

    #[test]
    fn the_box_is_centered_and_sized_to_its_content() {
        let g = geometry(100, 40, &lines(5, 50), "/status");
        assert_eq!(g.width, 54);
        assert_eq!(g.body, 5);
        assert_eq!(g.x, (100 - 54) / 2);
        assert_eq!(g.y, (40 - 7) / 2);
        assert_eq!(g.text_width(), 50);
    }

    #[test]
    fn short_content_gets_the_minimum_width() {
        let g = geometry(100, 40, &lines(1, 3), "/bug");
        assert_eq!(g.width as usize, MIN_WIDTH);
    }

    #[test]
    fn the_title_widens_the_box() {
        let title = "t".repeat(60);
        let g = geometry(200, 40, &lines(1, 3), &title);
        assert_eq!(g.width, 66);
    }

    #[test]
    fn the_box_never_exceeds_the_screen() {
        let g = geometry(50, 20, &lines(100, 200), "/help");
        assert_eq!(g.width, 46);
        assert_eq!(g.body, 16);
        assert!(g.x + g.width <= 50);
        assert!(g.y + g.body + 2 <= 20);
        // A tiny terminal still gets a sane box.
        let g = geometry(10, 5, &lines(10, 10), "x");
        assert!(g.width >= 6 && g.body >= 1);
    }

    #[test]
    fn empty_content_still_has_a_row() {
        assert_eq!(geometry(80, 24, &[], "/todos").body, 1);
    }

    #[test]
    fn scrolling_stops_at_the_last_full_screen() {
        assert_eq!(max_top(100, 20), 80);
        assert_eq!(max_top(5, 20), 0);
    }
}
