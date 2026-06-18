//! Streaming event processing: spinner, token rendering, tool output, thought extraction.

use io_runtime::AgentEvent;
use io_tui::render::{render_markdown_lines, render_tool_done, render_tool_start, tool_detail, Theme};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Shared plain-text line buffer used for in-session scrollback.
pub type LineBuf = Arc<Mutex<VecDeque<String>>>;

pub type PendingPermission =
    Arc<Mutex<Option<tokio::sync::oneshot::Sender<io_runtime::PermissionReply>>>>;

pub const MAX_SCROLL_LINES: usize = 5000;
pub const SCROLL_STEP: usize = 3;

pub fn push_line(buf: &LineBuf, line: String) {
    let mut g = buf.lock().unwrap();
    g.push_back(line);
    while g.len() > MAX_SCROLL_LINES {
        g.pop_front();
    }
}

// ── Think-block parser ─────────────────────────────────────────────────────────

pub struct ThinkParser {
    pending: String,
    in_think: bool,
}

impl ThinkParser {
    pub fn new() -> Self {
        Self {
            pending: String::new(),
            in_think: false,
        }
    }

    pub fn feed(&mut self, delta: &str) -> (String, String) {
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
                    let safe = crate::input::char_floor(&self.pending, raw);
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
                let safe = crate::input::char_floor(&self.pending, raw);
                display.push_str(&self.pending[..safe]);
                self.pending = self.pending[safe..].to_string();
                break;
            }
        }

        (display, thoughts)
    }

    pub fn flush(&mut self) -> (String, String) {
        let text = std::mem::take(&mut self.pending);
        if self.in_think {
            (String::new(), text)
        } else {
            (text, String::new())
        }
    }
}

// ── Event processing ───────────────────────────────────────────────────────────

fn process_ev(
    ev: AgentEvent,
    text: &mut String,
    think: &mut String,
    parser: &mut ThinkParser,
    pending_perm: &PendingPermission,
    theme: Theme,
    line_buf: &LineBuf,
) {
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
            render_tool_start(&name, &input, &theme);
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

// ── Streaming print loop ───────────────────────────────────────────────────────

pub async fn blink_and_print(
    mut rx: tokio::sync::mpsc::Receiver<AgentEvent>,
    pending_perm: PendingPermission,
    theme: Theme,
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
        if let AgentEvent::Usage {
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
            if let AgentEvent::Usage {
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
        let ansi_lines = render_markdown_lines(&text_buf, &theme);
        {
            use crossterm::QueueableCommand;
            let mut out = std::io::stdout();
            for line in &ansi_lines {
                let _ = out.queue(crossterm::style::Print(line));
                let _ = out.queue(crossterm::style::Print("\r\n"));
            }
            let _ = out.flush();
        }
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

// ── Tests ──────────────────────────────────────────────────────────────────────

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
