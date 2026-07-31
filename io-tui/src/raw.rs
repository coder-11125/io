//! Terminal raw-mode helpers.
//!
//! TUI components (pickers, theme picker, readline popups) temporarily enter
//! raw mode. They must preserve the caller's raw-mode state: the app enables
//! raw mode once in `render::enter_tui` and keeps it on for the whole
//! session, so a popup that unconditionally calls `disable_raw_mode()` on exit
//! leaves the terminal cooked mid-session while mouse capture stays enabled —
//! every mouse move then echoes raw SGR escape sequences as visible garbage.
//!
//! [`RawModeGuard`] records whether *it* enabled raw mode and only restores
//! (disables) it in that case, on every exit path including errors and Ctrl+C.

use crossterm::terminal;

/// RAII guard that enables raw mode only if it was not already enabled, and
/// disables it on drop only when it did the enabling.
pub struct RawModeGuard {
    enabled_here: bool,
}

impl RawModeGuard {
    /// Enable raw mode if needed, remembering whether this guard owns the
    /// change. Returns the guard; raw mode is restored when it is dropped.
    pub fn acquire() -> std::io::Result<Self> {
        let already = terminal::is_raw_mode_enabled().unwrap_or(false);
        if !already {
            terminal::enable_raw_mode()?;
        }
        Ok(Self {
            enabled_here: !already,
        })
    }

    /// Whether this guard turned raw mode on (and therefore owns restoring it).
    pub fn enabled_here(&self) -> bool {
        self.enabled_here
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.enabled_here {
            let _ = terminal::disable_raw_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs under a real PTY (`script -q /dev/null cargo test -p io-tui --lib
    /// -- --ignored pty_raw_mode_guard` on macOS/Linux). Proves the guard
    /// preserves a caller's pre-existing raw mode — the regression that left
    /// the terminal cooked mid-session while mouse capture stayed on, which
    /// echoed raw SGR escape sequences on every mouse move.
    #[test]
    #[ignore]
    fn pty_raw_mode_guard() {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            eprintln!("skipping: no tty (run under `script -q /dev/null`)");
            return;
        }

        // Caller already in raw mode (TUI session): guard must NOT disable.
        terminal::enable_raw_mode().unwrap();
        {
            let guard = RawModeGuard::acquire().unwrap();
            assert!(!guard.enabled_here());
            assert!(terminal::is_raw_mode_enabled().unwrap());
        }
        assert!(
            terminal::is_raw_mode_enabled().unwrap(),
            "guard disabled raw mode the caller had enabled — mouse SGR garbage bug"
        );
        terminal::disable_raw_mode().unwrap();

        // Caller not in raw mode: guard enables, then restores on drop.
        assert!(!terminal::is_raw_mode_enabled().unwrap());
        {
            let guard = RawModeGuard::acquire().unwrap();
            assert!(guard.enabled_here());
            assert!(terminal::is_raw_mode_enabled().unwrap());
        }
        assert!(
            !terminal::is_raw_mode_enabled().unwrap(),
            "guard must restore raw mode it enabled"
        );
    }
}
