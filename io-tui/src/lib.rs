pub mod modal;
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
    ("/agent", "Switch agent mode"),
    ("/connect", "Set up a provider"),
    ("/login", "Sign in with OAuth (ChatGPT / Claude)"),
    ("/model", "Switch model"),
    ("/theme", "Switch UI theme"),
    ("/config", "Toggle session & permission settings"),
    ("/cost", "Show API cost for current session"),
    (
        "/context",
        "Fetch real context window, pricing, and tool support from the provider catalog",
    ),
    ("/compact", "Summarize and compress conversation history"),
    (
        "/plan",
        "Enter plan mode: read-only research, then a plan to approve (or Shift+Tab)",
    ),
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
