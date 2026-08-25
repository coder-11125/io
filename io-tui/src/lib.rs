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
    ("/exit", "Exit"),
    ("/quit", "Exit"),
    ("/q", "Exit"),
];
