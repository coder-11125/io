use crate::config::PermissionConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionLevel {
    Allow,
    Prompt,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool_name: Option<String>,
    pub command_pattern: Option<String>,
    pub level: PermissionLevel,
}

/// Tools that never modify state — safe to run without prompting.
const READ_ONLY_TOOLS: &[&str] = &["read", "glob", "grep"];

pub struct PermissionChecker {
    mode: PermissionLevel,
    allowlist: HashSet<String>,
    denylist: HashSet<String>,
    /// Tools the user approved with "always allow" for the current session.
    session_allow: std::sync::Mutex<HashSet<String>>,
}

impl PermissionChecker {
    pub fn new(mode_str: &str) -> Self {
        let mode = match mode_str {
            "allow" => PermissionLevel::Allow,
            "deny" => PermissionLevel::Deny,
            _ => PermissionLevel::Prompt,
        };

        Self {
            mode,
            allowlist: HashSet::new(),
            denylist: HashSet::new(),
            session_allow: std::sync::Mutex::new(HashSet::new()),
        }
    }

    pub fn add_allow(&mut self, pattern: String) {
        self.allowlist.insert(pattern);
    }

    pub fn add_deny(&mut self, pattern: String) {
        self.denylist.insert(pattern);
    }

    /// Legacy boolean check: treats `Prompt` as allowed. Callers that can ask
    /// the user (the agent loop) should use `decide_tool` instead.
    pub fn check_tool(&self, tool_name: &str, input: &serde_json::Value) -> bool {
        self.decide_tool(tool_name, input) != PermissionLevel::Deny
    }

    /// Full three-way decision for a tool call.
    ///
    /// In `Prompt` mode: deny/allow lists win, read-only tools (read, glob,
    /// grep) and session-approved tools run without asking, bash commands are
    /// matched against the command lists, and everything else returns `Prompt`
    /// so the caller can ask the user.
    pub fn decide_tool(&self, tool_name: &str, input: &serde_json::Value) -> PermissionLevel {
        match self.mode {
            PermissionLevel::Allow => PermissionLevel::Allow,
            PermissionLevel::Deny => PermissionLevel::Deny,
            PermissionLevel::Prompt => {
                if self.denylist.contains(tool_name) {
                    return PermissionLevel::Deny;
                }
                if self.allowlist.contains(tool_name) {
                    return PermissionLevel::Allow;
                }
                if self.session_allow.lock().unwrap().contains(tool_name) {
                    return PermissionLevel::Allow;
                }
                if READ_ONLY_TOOLS.contains(&tool_name) {
                    return PermissionLevel::Allow;
                }
                if tool_name == "bash" {
                    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                        return self.check_command(cmd);
                    }
                }
                PermissionLevel::Prompt
            }
        }
    }

    /// Record a user's "always allow" answer for the rest of this session.
    pub fn allow_for_session(&self, tool_name: &str) {
        self.session_allow
            .lock()
            .unwrap()
            .insert(tool_name.to_string());
    }

    pub fn check_command(&self, command: &str) -> PermissionLevel {
        let tokens = shell_tokens(command);
        for denied in &self.denylist {
            if tokens.iter().any(|t| t == denied) {
                return PermissionLevel::Deny;
            }
        }
        for allowed in &self.allowlist {
            if tokens.iter().any(|t| t == allowed) {
                return PermissionLevel::Allow;
            }
        }
        self.mode
    }

    pub fn mode(&self) -> PermissionLevel {
        self.mode
    }
}

/// Extracts command basenames from a shell command string for deny/allow matching.
/// Splits on shell metacharacters, strips backslash escapes, and resolves basenames
/// so that `r\m`, `/bin/rm`, and `rm` all produce the token `rm`.
fn shell_tokens(command: &str) -> Vec<String> {
    command
        .split([' ', '|', ';', '&', '(', ')', '`', '\n', '\t', '{', '}'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            // Strip backslash escapes (r\m → rm)
            let unescaped: String = t.chars().filter(|&c| c != '\\').collect();
            // Strip leading $( for command substitution
            let cleaned = unescaped.trim_start_matches("$(").trim_end_matches(')');
            // Return basename so /bin/rm → rm
            std::path::Path::new(cleaned)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(cleaned)
                .to_string()
        })
        .collect()
}

impl From<&PermissionConfig> for PermissionChecker {
    fn from(config: &PermissionConfig) -> Self {
        let mut checker = PermissionChecker::new(&config.default);
        for cmd in &config.allowed_commands {
            checker.add_allow(cmd.clone());
        }
        for cmd in &config.denied_commands {
            checker.add_deny(cmd.clone());
        }
        checker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_mode_permits_all_tools() {
        let checker = PermissionChecker::new("allow");
        assert!(checker.check_tool("bash", &serde_json::json!({})));
        assert!(checker.check_tool("read", &serde_json::json!({})));
    }

    #[test]
    fn deny_mode_blocks_all_tools() {
        let checker = PermissionChecker::new("deny");
        assert!(!checker.check_tool("bash", &serde_json::json!({})));
        assert!(!checker.check_tool("read", &serde_json::json!({})));
    }

    #[test]
    fn prompt_mode_allows_by_default() {
        let checker = PermissionChecker::new("prompt");
        assert!(checker.check_tool("read", &serde_json::json!({})));
    }

    #[test]
    fn denylist_blocks_specific_tool_in_prompt_mode() {
        let mut checker = PermissionChecker::new("prompt");
        checker.add_deny("bash".to_string());
        assert!(!checker.check_tool("bash", &serde_json::json!({})));
        assert!(checker.check_tool("read", &serde_json::json!({})));
    }

    #[test]
    fn check_command_deny_matches_basename() {
        let mut checker = PermissionChecker::new("prompt");
        checker.add_deny("rm".to_string());
        assert_eq!(
            checker.check_command("rm -rf /tmp/x"),
            PermissionLevel::Deny
        );
        assert_eq!(
            checker.check_command("/bin/rm -rf /tmp/x"),
            PermissionLevel::Deny
        );
    }

    #[test]
    fn check_command_allow_matches_token() {
        let mut checker = PermissionChecker::new("prompt");
        checker.add_allow("echo".to_string());
        assert_eq!(checker.check_command("echo hello"), PermissionLevel::Allow);
    }

    #[test]
    fn check_command_falls_back_to_mode() {
        let checker = PermissionChecker::new("prompt");
        assert_eq!(checker.check_command("ls -la"), PermissionLevel::Prompt);
    }

    #[test]
    fn decide_tool_read_only_tools_skip_prompt() {
        let checker = PermissionChecker::new("prompt");
        for tool in ["read", "glob", "grep"] {
            assert_eq!(
                checker.decide_tool(tool, &serde_json::json!({})),
                PermissionLevel::Allow
            );
        }
    }

    #[test]
    fn decide_tool_prompts_for_mutating_tools() {
        let checker = PermissionChecker::new("prompt");
        for tool in ["write", "edit", "spawn_agent"] {
            assert_eq!(
                checker.decide_tool(tool, &serde_json::json!({})),
                PermissionLevel::Prompt
            );
        }
    }

    #[test]
    fn decide_tool_bash_uses_command_lists() {
        let mut checker = PermissionChecker::new("prompt");
        checker.add_deny("rm".to_string());
        checker.add_allow("ls".to_string());
        let deny_input = serde_json::json!({"command": "rm -rf /tmp/x"});
        let allow_input = serde_json::json!({"command": "ls -la"});
        let other_input = serde_json::json!({"command": "git push"});
        assert_eq!(
            checker.decide_tool("bash", &deny_input),
            PermissionLevel::Deny
        );
        assert_eq!(
            checker.decide_tool("bash", &allow_input),
            PermissionLevel::Allow
        );
        assert_eq!(
            checker.decide_tool("bash", &other_input),
            PermissionLevel::Prompt
        );
    }

    #[test]
    fn allow_for_session_persists_approval() {
        let checker = PermissionChecker::new("prompt");
        assert_eq!(
            checker.decide_tool("write", &serde_json::json!({})),
            PermissionLevel::Prompt
        );
        checker.allow_for_session("write");
        assert_eq!(
            checker.decide_tool("write", &serde_json::json!({})),
            PermissionLevel::Allow
        );
    }

    #[test]
    fn decide_tool_allow_mode_bypasses_everything() {
        let checker = PermissionChecker::new("allow");
        assert_eq!(
            checker.decide_tool("bash", &serde_json::json!({"command": "rm x"})),
            PermissionLevel::Allow
        );
    }
}
