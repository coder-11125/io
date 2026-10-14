//! Read-only reports behind `/status`, `/context`, `/hooks`, `/agents`,
//! `/todos`, `/bug` and `/terminal-setup`. Each builds plain lines so it can
//! be tested and shown in the REPL or printed.

use io_runtime::hooks::{HookEntry, HooksConfig};
use io_runtime::types::Turn;
use io_runtime::ContextBreakdown;

pub const ISSUES_URL: &str = "https://github.com/coder-11125/io/issues/new";

pub struct Status<'a> {
    pub version: &'a str,
    pub cwd: &'a str,
    pub project_root: &'a str,
    pub session_id: &'a str,
    pub session_name: Option<&'a str>,
    pub turns: usize,
    pub provider: &'a str,
    pub model: &'a str,
    pub agent: &'a str,
    pub mode: &'a str,
    pub permissions: &'a str,
    pub thinking: &'a str,
    pub output_style: &'a str,
    pub settings_files: Vec<String>,
    pub rule_counts: (usize, usize, usize),
    pub mcp: (usize, usize),
    pub sandbox: &'a str,
    pub memory_files: Vec<String>,
}

pub fn status_lines(s: &Status) -> Vec<String> {
    let session = match s.session_name {
        Some(name) => format!("{} \"{name}\" ({} turns)", s.session_id, s.turns),
        None => format!("{} ({} turns)", s.session_id, s.turns),
    };
    let list = |items: &[String]| {
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join(", ")
        }
    };
    vec![
        format!("io {}", s.version),
        format!("  Session:      {session}"),
        format!(
            "  Model:        {} · {} · agent {}",
            s.model, s.provider, s.agent
        ),
        format!("  Working mode: {} · permissions {}", s.mode, s.permissions),
        format!(
            "  Thinking:     {} · output style {}",
            s.thinking, s.output_style
        ),
        format!("  Directory:    {}", s.cwd),
        format!("  Project root: {}", s.project_root),
        format!("  Settings:     {}", list(&s.settings_files)),
        format!(
            "  Rules:        {} allow · {} ask · {} deny",
            s.rule_counts.0, s.rule_counts.1, s.rule_counts.2
        ),
        format!("  Memory:       {}", list(&s.memory_files)),
        format!("  MCP:          {} server(s), {} tool(s)", s.mcp.0, s.mcp.1),
        format!("  Sandbox:      {}", s.sandbox),
    ]
}

fn bar(chars: usize, total: usize) -> String {
    let width = 24usize;
    let filled = if total == 0 {
        0
    } else {
        (chars * width).div_ceil(total).min(width)
    };
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// The `/context` breakdown: where the conversation's tokens go.
pub fn context_lines(b: &ContextBreakdown, window: u64, reported_used: u32) -> Vec<String> {
    let total = b.total_chars();
    let est = ContextBreakdown::tokens(total);
    let parts = [
        ("System prompt", b.system_prompt),
        ("Memory files", b.memory_files),
        ("Tool definitions", b.tool_definitions),
        ("Compaction summary", b.summary),
        ("Conversation", b.history),
    ];
    let mut lines = vec![format!(
        "Context: ~{}K of {}K tokens (estimate from text size)",
        est.div_ceil(1000),
        window / 1000
    )];
    for (name, chars) in parts {
        let tokens = ContextBreakdown::tokens(chars);
        let pct = (chars * 100).checked_div(total).unwrap_or(0);
        lines.push(format!(
            "  {name:<20} {} {tokens:>7} tokens  {pct:>3}%",
            bar(chars, total)
        ));
    }
    if reported_used > 0 {
        lines.push(format!(
            "  Last request used {reported_used} tokens as counted by the provider ({}% of the window)",
            (reported_used as u64 * 100).checked_div(window).unwrap_or(0)
        ));
    }
    lines.push(
        "  (tool results from earlier turns are not carried over, so they don't count)".into(),
    );
    lines
}

pub fn hooks_lines(config: &HooksConfig) -> Vec<String> {
    let events: [(&str, &Vec<HookEntry>); 8] = [
        ("pre_tool_use", &config.pre_tool_use),
        ("post_tool_use", &config.post_tool_use),
        ("user_prompt_submit", &config.user_prompt_submit),
        ("stop", &config.stop),
        ("session_start", &config.session_start),
        ("session_end", &config.session_end),
        ("notification", &config.notification),
        ("pre_compact", &config.pre_compact),
    ];
    let mut lines = Vec::new();
    for (name, entries) in events {
        for e in entries {
            let matcher = e.matcher.as_deref().filter(|m| *m != "*");
            lines.push(format!(
                "  {name:<19} {}{}",
                matcher.map(|m| format!("[{m}] ")).unwrap_or_default(),
                e.command
            ));
        }
    }
    if lines.is_empty() {
        return vec![
            "No hooks configured. Add [[hooks.pre_tool_use]] and friends to ~/.io/config.toml."
                .into(),
        ];
    }
    lines.insert(0, "Hooks (from ~/.io/config.toml):".to_string());
    lines
}

pub fn agents_lines(current: &str) -> Vec<String> {
    let mut lines = vec!["Agents (Tab or /agent switches between the full ones):".to_string()];
    for kind in [true, false] {
        for a in io_agents::all_agents()
            .into_iter()
            .filter(|a| (a.tool_access == io_agents::ToolAccess::All) == kind)
        {
            lines.push(format!(
                "  {}{:<10} {:<9} {}",
                if a.id == current { "* " } else { "  " },
                a.id,
                if kind { "full" } else { "sub-agent" },
                a.description
            ));
        }
    }
    lines
}

/// The latest task list the model wrote with `todo_write`.
pub fn todos_lines(turns: &[Turn]) -> Vec<String> {
    let latest = turns
        .iter()
        .rev()
        .flat_map(|t| t.tool_calls.iter().rev())
        .find(|c| c.tool_name == "todo_write" && c.success);
    let Some(call) = latest else {
        return vec![
            "No task list yet — the model writes one when a job has several steps.".into(),
        ];
    };
    let Ok(todos) = io_runtime::tools::todo::parse_todos(&call.input["todos"]) else {
        return vec!["The last task list could not be read.".into()];
    };
    let mut lines = vec![format!(
        "Tasks — {}",
        io_runtime::tools::todo::summarize(&todos)
    )];
    for t in &todos {
        let mark = match t.status {
            io_runtime::tools::todo::TodoStatus::Completed => "[x]",
            io_runtime::tools::todo::TodoStatus::InProgress => "[~]",
            io_runtime::tools::todo::TodoStatus::Pending => "[ ]",
        };
        lines.push(format!("  {mark} {}", t.content));
    }
    lines
}

pub fn bug_lines(version: &str, provider: &str, model: &str) -> Vec<String> {
    vec![
        format!("Found a bug? Open an issue: {ISSUES_URL}"),
        "Include this:".to_string(),
        format!(
            "  io {version} · {} {} · {provider}/{model}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        "  what you ran, what you expected, and what happened".to_string(),
        "  `io doctor` output, if it looks setup-related".to_string(),
    ]
}

pub fn terminal_setup_lines() -> Vec<String> {
    vec![
        "New line in a message: Alt+Enter or Ctrl+J, or end the line with a backslash.".into(),
        "Shift+Enter also works in terminals that speak the kitty keyboard protocol".into(),
        "(kitty, WezTerm, Ghostty, foot, iTerm2); in others it sends a plain Enter.".into(),
        "If Alt+Enter does nothing, make your terminal send Option/Alt as Escape".into(),
        "(macOS Terminal: Settings → Profiles → Keyboard → \"Use Option as Meta key\";".into(),
        "iTerm2: Profiles → Keys → \"Left Option key: Esc+\").".into(),
        "Images: Ctrl+V needs pngpaste (macOS), wl-paste (Wayland) or xclip (X11).".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use io_runtime::types::ToolCallRecord;

    fn call(name: &str, input: serde_json::Value, success: bool) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: name.into(),
            input,
            output: String::new(),
            success,
            duration_ms: 0,
            pre_edit_snapshot: None,
        }
    }

    fn turn(calls: Vec<ToolCallRecord>) -> Turn {
        Turn {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user_message: String::new(),
            assistant_message: None,
            tool_calls: calls,
            usage: None,
        }
    }

    #[test]
    fn the_context_report_splits_the_total_and_names_the_parts() {
        let b = ContextBreakdown {
            system_prompt: 4000,
            memory_files: 2000,
            tool_definitions: 8000,
            summary: 0,
            history: 6000,
        };
        let lines = context_lines(&b, 200_000, 12_000);
        assert!(lines[0].contains("~5K of 200K"), "{}", lines[0]);
        let tools = lines
            .iter()
            .find(|l| l.contains("Tool definitions"))
            .unwrap();
        assert!(
            tools.contains("2000 tokens") && tools.contains(" 40%"),
            "{tools}"
        );
        assert!(lines
            .iter()
            .any(|l| l.contains("12000 tokens as counted by the provider (6%")));
        // An empty conversation doesn't divide by zero.
        let empty = context_lines(&ContextBreakdown::default(), 0, 0);
        assert!(empty[0].contains("~0K of 0K"));
    }

    #[test]
    fn todos_come_from_the_latest_successful_todo_write() {
        let first = call(
            "todo_write",
            serde_json::json!({"todos": [{"content": "old", "status": "pending"}]}),
            true,
        );
        let second = call(
            "todo_write",
            serde_json::json!({"todos": [
                {"content": "Write code", "status": "completed"},
                {"content": "Test it", "status": "in_progress", "activeForm": "Testing"},
                {"content": "Ship", "status": "pending"}
            ]}),
            true,
        );
        let failed = call("todo_write", serde_json::json!({"todos": "bad"}), false);
        let lines = todos_lines(&[turn(vec![first]), turn(vec![second, failed])]);
        assert_eq!(
            lines[1..],
            ["  [x] Write code", "  [~] Test it", "  [ ] Ship"]
        );
        assert!(todos_lines(&[])
            .first()
            .unwrap()
            .starts_with("No task list yet"));
    }

    #[test]
    fn hooks_are_listed_with_their_matchers() {
        let mut cfg = HooksConfig::default();
        assert!(hooks_lines(&cfg)[0].starts_with("No hooks configured"));
        cfg.pre_tool_use.push(HookEntry {
            matcher: Some("bash".into()),
            command: "./guard.sh".into(),
            timeout_ms: None,
        });
        cfg.stop.push(HookEntry {
            matcher: Some("*".into()),
            command: "notify".into(),
            timeout_ms: None,
        });
        let lines = hooks_lines(&cfg);
        assert!(lines
            .iter()
            .any(|l| l.contains("pre_tool_use") && l.contains("[bash] ./guard.sh")));
        assert!(lines
            .iter()
            .any(|l| l.contains("stop") && l.ends_with("notify")));
    }

    #[test]
    fn agents_show_full_agents_first_and_mark_the_current_one() {
        let lines = agents_lines("build");
        assert!(lines[1].contains("full"));
        assert!(lines.iter().any(|l| l.starts_with("  * build")));
        assert!(lines.iter().any(|l| l.contains("sub-agent")));
    }

    #[test]
    fn status_lists_every_section() {
        let lines = status_lines(&Status {
            version: "1.2.3",
            cwd: "/p/sub",
            project_root: "/p",
            session_id: "abc",
            session_name: Some("work"),
            turns: 3,
            provider: "anthropic",
            model: "m",
            agent: "build",
            mode: "plan",
            permissions: "agent",
            thinking: "off",
            output_style: "concise",
            settings_files: vec![],
            rule_counts: (1, 2, 3),
            mcp: (2, 7),
            sandbox: "off",
            memory_files: vec!["CLAUDE.md".into()],
        });
        let text = lines.join("\n");
        for needle in [
            "io 1.2.3",
            "abc \"work\" (3 turns)",
            "plan · permissions agent",
            "1 allow · 2 ask · 3 deny",
            "Settings:     none",
            "Memory:       CLAUDE.md",
            "2 server(s), 7 tool(s)",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
    }
}
