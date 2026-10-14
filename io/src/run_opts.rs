//! Per-run overrides from command-line flags (`--allowedTools`,
//! `--append-system-prompt`, `--permission-mode`, `--max-turns`). Agents are
//! rebuilt many times in a session, so the flags live here rather than on any
//! one agent.

use std::sync::Mutex;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOpts {
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub append_system_prompt: Option<String>,
    /// `--permission-mode bypassPermissions`: allow everything.
    pub bypass_permissions: bool,
    pub max_turns: Option<usize>,
    /// A working mode was chosen on the command line, so settings.json's
    /// `defaultMode` must not override it.
    pub explicit_mode: bool,
}

static OPTS: Mutex<Option<RunOpts>> = Mutex::new(None);

pub fn set(opts: RunOpts) {
    *OPTS.lock().unwrap() = Some(opts);
}

pub fn get() -> RunOpts {
    OPTS.lock().unwrap().clone().unwrap_or_default()
}

/// Split a `--allowedTools` value into rules. Entries are separated by commas
/// or whitespace, except inside parentheses: `Bash(git log:*) Read,Edit`.
pub fn split_tool_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' | ' ' | '\t' | '\n' if depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// What a `--permission-mode` word means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeFlag {
    Default,
    AcceptEdits,
    Plan,
    Bypass,
}

pub fn parse_permission_mode(word: &str) -> Result<ModeFlag, String> {
    match word {
        "default" => Ok(ModeFlag::Default),
        "acceptEdits" | "accept-edits" => Ok(ModeFlag::AcceptEdits),
        "plan" => Ok(ModeFlag::Plan),
        "bypassPermissions" | "bypass" => Ok(ModeFlag::Bypass),
        other => Err(format!(
            "unknown permission mode '{other}' (use default, acceptEdits, plan or bypassPermissions)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_lists_split_on_commas_and_spaces_outside_parentheses() {
        assert_eq!(
            split_tool_list("Bash(git log:*) Read,Edit"),
            ["Bash(git log:*)", "Read", "Edit"]
        );
        assert_eq!(
            split_tool_list("Bash(npm run a, b),mcp__x__*"),
            ["Bash(npm run a, b)", "mcp__x__*"]
        );
        assert!(split_tool_list("  ,, ").is_empty());
        assert_eq!(split_tool_list("Read"), ["Read"]);
    }

    #[test]
    fn permission_modes_parse() {
        assert_eq!(parse_permission_mode("plan"), Ok(ModeFlag::Plan));
        assert_eq!(
            parse_permission_mode("acceptEdits"),
            Ok(ModeFlag::AcceptEdits)
        );
        assert_eq!(
            parse_permission_mode("bypassPermissions"),
            Ok(ModeFlag::Bypass)
        );
        assert!(parse_permission_mode("yolo").is_err());
    }
}
