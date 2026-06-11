//! Terminal rendering: prompt/status line, markdown, thoughts, tool calls,
//! and syntax-colored diffs.

use std::io::Write;

/// Render a compact context bar: `ctx [████░░░░░░] 13% of 200K`
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

pub fn print_prompt(agent: &io_runtime::Agent, input_tokens: u32, agent_id: &str) {
    use crossterm::style::Stylize;
    use std::io::Write;

    let provider_model = format!(
        "  {} · {} · {}",
        agent_id, agent.provider_id, agent.model_id
    );
    let status = if input_tokens > 0 {
        let bar = render_context_bar(input_tokens, agent.context_window());
        format!("{}  |  {}", provider_model, bar)
    } else {
        provider_model
    };
    print!("{}\n>>> ", status.dark_grey());
    let _ = std::io::stdout().flush();
}

pub fn render_markdown(text: &str) {
    use std::io::Write;

    let mut skin = termimad::MadSkin::default();
    skin.paragraph.align = termimad::Alignment::Left;
    skin.paragraph.left_margin = 0;
    skin.code_block.align = termimad::Alignment::Left;
    skin.code_block.left_margin = 0;
    for h in &mut skin.headers {
        h.align = termimad::Alignment::Left;
        h.left_margin = 0;
    }
    skin.headers[0].set_fg(termimad::crossterm::style::Color::Green);
    skin.headers[0].add_attr(termimad::crossterm::style::Attribute::Bold);
    skin.bold.set_fg(termimad::crossterm::style::Color::Green);
    skin.inline_code
        .set_fg(termimad::crossterm::style::Color::Yellow);
    skin.code_block
        .set_fg(termimad::crossterm::style::Color::Yellow);
    skin.italic.set_fg(termimad::crossterm::style::Color::White);
    skin.table.align = termimad::Alignment::Left;
    skin.table.left_margin = 0;

    // Write each line with \r\n so rendering works in raw mode
    // (raw mode \n doesn't carriage-return, causing horizontal drift)
    let rendered = format!("{}", skin.text(text, None));
    let mut out = std::io::stdout();
    for line in rendered.lines() {
        use crossterm::QueueableCommand;
        let _ = out.queue(crossterm::style::Print(line));
        let _ = out.queue(crossterm::style::Print("\r\n"));
    }
    let _ = out.flush();
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

pub fn render_tool_done(name: &str, output: &str, success: bool) {
    if !success {
        use crossterm::style::Stylize;
        print!("  {}\r\n", format!("error: {output}").red());
        let _ = std::io::stdout().flush();
        return;
    }
    match name {
        "write" | "edit" => render_diff(output),
        _ => {}
    }
}

fn render_diff(diff: &str) {
    use crossterm::{
        execute,
        style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    };
    use std::io::stdout;

    let mut old_line: u32 = 0;
    let mut new_line: u32 = 0;

    for raw in diff.lines() {
        if raw.starts_with("--- ") || raw.starts_with("+++ ") {
            // file header — dim grey
            let _ = execute!(
                stdout(),
                SetForegroundColor(Color::DarkGrey),
                Print(format!("  {raw}\r\n")),
                ResetColor
            );
        } else if let Some(rest) = raw.strip_prefix("@@ ") {
            // parse @@ -a,b +c,d @@ to reset counters
            if let Some((a, b)) = parse_hunk(rest) {
                old_line = a;
                new_line = b;
            }
            let _ = execute!(
                stdout(),
                SetForegroundColor(Color::DarkCyan),
                Print(format!("  @@ {rest}\r\n")),
                ResetColor
            );
        } else if let Some(content) = raw.strip_prefix('-') {
            let _ = execute!(
                stdout(),
                SetForegroundColor(Color::Red),
                Print(format!("{:>5} ", old_line)),
                SetForegroundColor(Color::Black),
                SetBackgroundColor(Color::Red),
                Print("-"),
                ResetColor,
                SetBackgroundColor(Color::DarkRed),
                SetForegroundColor(Color::White),
                Print(format!("  {content}")),
                ResetColor,
                Print("\r\n")
            );
            old_line += 1;
        } else if let Some(content) = raw.strip_prefix('+') {
            let _ = execute!(
                stdout(),
                SetForegroundColor(Color::Green),
                Print(format!("{:>5} ", new_line)),
                SetForegroundColor(Color::Black),
                SetBackgroundColor(Color::Green),
                Print("+"),
                ResetColor,
                SetBackgroundColor(Color::DarkGreen),
                SetForegroundColor(Color::White),
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
    // expects "-A,B +C,D @@…" (the "@@ " prefix already stripped)
    let s = s.trim_start_matches('-');
    let mut parts = s.splitn(2, ' ');
    let old_part = parts.next()?;
    let rest = parts.next()?.trim_start_matches('+');
    let a = old_part.split(',').next()?.parse().ok()?;
    let b = rest.split(',').next()?.parse().ok()?;
    Some((a, b))
}
