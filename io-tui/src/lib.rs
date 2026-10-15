pub mod modal;
pub mod pager;
pub mod picker;
pub mod raw;
pub mod readline;
pub mod render;
pub mod settings;
pub mod theme;

/// Canonical slash-command list shared by the splash screen and the active-session popup.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show available commands"),
    ("/new", "Start a new session"),
    ("/clear", "Start a new session (clears the conversation)"),
    ("/resume", "Go back to an earlier session"),
    ("/rename", "Name this session: /rename <name>"),
    ("/fork", "Branch this session into a copy"),
    (
        "/export",
        "Write this session to a Markdown file: /export [file]",
    ),
    ("/agent", "Switch agent mode"),
    ("/connect", "Set up a provider"),
    ("/login", "Sign in with OAuth (ChatGPT / Claude)"),
    ("/model", "Switch model"),
    ("/theme", "Switch UI theme"),
    ("/config", "Toggle session & permission settings"),
    ("/cost", "Show API cost for current session"),
    (
        "/context",
        "Where the context window is going (/context refresh re-fetches model limits)",
    ),
    ("/compact", "Summarize and compress conversation history"),
    (
        "/status",
        "Version, model, mode, settings, rules, memory, MCP",
    ),
    ("/todos", "The model's current task list"),
    ("/hooks", "Configured hooks"),
    ("/agents", "Available agents and sub-agents"),
    ("/doctor", "Check the installation and environment"),
    (
        "/add-dir",
        "Let file tools reach another directory: /add-dir <dir>",
    ),
    ("/logout", "Sign out of an OAuth login"),
    ("/usage", "API usage and cost (same as /cost)"),
    ("/terminal-setup", "Multi-line input and image paste setup"),
    ("/bug", "How to report a problem"),
    (
        "/plan",
        "Enter plan mode: read-only research, then a plan to approve (or Shift+Tab)",
    ),
    (
        "/output-style",
        "Change how the agent talks: /output-style [name]",
    ),
    (
        "/init",
        "Have the agent analyze the project and write a CLAUDE.md",
    ),
    ("/memory", "Open a memory file (CLAUDE.md) in your editor"),
    (
        "/permissions",
        "Show or edit allow/ask/deny rules: /permissions allow Bash(npm test:*)",
    ),
    (
        "/mcp",
        "Show MCP servers and their status (/mcp reconnect to restart them)",
    ),
    (
        "/think",
        "Extended thinking: /think [on|off|low|medium|high|<tokens>] (or Alt+T)",
    ),
    (
        "/rewind",
        "Restore files to an earlier point in the chat (or press Esc twice)",
    ),
    ("/exit", "Exit"),
    ("/quit", "Exit"),
    ("/q", "Exit"),
];
