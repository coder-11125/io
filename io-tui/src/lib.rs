pub mod picker;
pub mod readline;
pub mod render;
pub mod theme;

/// Canonical slash-command list shared by the splash screen and the active-session popup.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show available commands"),
    ("/new", "Start a new session"),
    ("/agent", "Switch agent mode"),
    ("/connect", "Set up a provider"),
    ("/model", "Switch model"),
    ("/theme", "Switch UI theme"),
    ("/cost", "Show API cost for current session"),
    ("/compact", "Summarize and compress conversation history"),
    ("/exit", "Exit"),
    ("/quit", "Exit"),
    ("/q", "Exit"),
];
