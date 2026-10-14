//! Lifecycle hooks: user-configured shell commands that run at fixed points
//! in the agent loop.
//!
//! Configured in the global `~/.io/config.toml` (project config files are
//! deliberately not consulted, so a cloned repository can never plant a
//! command that runs on the user's machine):
//!
//! ```toml
//! [[hooks.pre_tool_use]]
//! matcher = "bash"          # tool names joined by `|`; omit or "*" for all
//! command = "./scripts/guard.sh"
//! timeout_ms = 10000        # optional, default 30000
//!
//! [[hooks.post_tool_use]]
//! matcher = "write|edit"
//! command = "cargo fmt"
//!
//! [[hooks.user_prompt_submit]]
//! command = "git status --short"
//!
//! [[hooks.stop]]
//! command = "notify-send 'io finished'"
//! ```
//!
//! Each hook is run with `sh -c` in the project root, receives a JSON event
//! description on stdin (plus `IO_HOOK_EVENT` / `IO_TOOL_NAME` in the
//! environment), and communicates through its exit code:
//!
//! * `0` — success. For `user_prompt_submit`, stdout is added to the prompt
//!   as context; for `post_tool_use`, stdout is shown to the model.
//! * `2` — block. `pre_tool_use` stops the tool call and `user_prompt_submit`
//!   rejects the prompt, with stderr as the reason. For `post_tool_use`
//!   (the tool already ran) stderr is shown to the model as feedback.
//! * anything else, a spawn failure, or a timeout — a non-blocking error
//!   that is logged and otherwise ignored. Only an explicit exit 2 blocks, so
//!   a guard hook that crashes fails open.
//!
//! Hooks can only add restrictions: `pre_tool_use` runs before the
//! permission check and can veto a call, but can never grant permission.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Per-stream cap on captured hook output; the rest is drained and dropped.
const MAX_OUTPUT_BYTES: usize = 16 * 1024;
/// Exit code a hook uses to block the action it was invoked for.
const BLOCK_EXIT_CODE: i32 = 2;

/// The `[hooks]` section of the config file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HooksConfig {
    /// Before a tool call runs (and before the permission check).
    #[serde(default)]
    pub pre_tool_use: Vec<HookEntry>,
    /// After a tool call ran.
    #[serde(default)]
    pub post_tool_use: Vec<HookEntry>,
    /// When the user submits a prompt, before the model sees it.
    #[serde(default)]
    pub user_prompt_submit: Vec<HookEntry>,
    /// After a turn completes successfully.
    #[serde(default)]
    pub stop: Vec<HookEntry>,
}

/// One configured hook command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookEntry {
    /// Tool names joined by `|` (e.g. `"write|edit"`). Omitted or `"*"`
    /// matches every tool. Ignored for events that have no tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// Shell command, run with `sh -c`.
    pub command: String,
    /// Kill the hook after this many milliseconds. Defaults to 30 000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl HookEntry {
    fn matches_tool(&self, tool: &str) -> bool {
        match self.matcher.as_deref().map(str::trim) {
            None | Some("") | Some("*") => true,
            Some(pattern) => pattern.split('|').any(|name| name.trim() == tool),
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))
    }
}

/// What a finished hook process reported.
struct HookRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl HookRun {
    fn blocked(&self) -> bool {
        self.code == Some(BLOCK_EXIT_CODE)
    }

    /// stderr if it has anything, else stdout — the text a hook that exited
    /// 2 wants the user or model to see.
    fn explanation(&self) -> String {
        let stderr = self.stderr.trim();
        if stderr.is_empty() {
            self.stdout.trim().to_string()
        } else {
            stderr.to_string()
        }
    }
}

/// Runs the configured hooks. Cheap to clone; an empty `Hooks` (the default)
/// does nothing, so callers never need to branch on whether hooks exist.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    config: Arc<HooksConfig>,
    cwd: PathBuf,
}

impl Hooks {
    /// `cwd` is the directory hooks run in — the project root.
    pub fn new(config: HooksConfig, cwd: PathBuf) -> Self {
        Self {
            config: Arc::new(config),
            cwd,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self.config == HooksConfig::default()
    }

    /// The subset sub-agents inherit: tool hooks only. Sub-agents are driven
    /// by the parent model, not typed by the user, so `user_prompt_submit`
    /// and `stop` don't apply — but a `pre_tool_use` guard must, or
    /// delegating to a sub-agent would sidestep it.
    pub fn for_sub_agent(&self) -> Self {
        Self {
            config: Arc::new(HooksConfig {
                pre_tool_use: self.config.pre_tool_use.clone(),
                post_tool_use: self.config.post_tool_use.clone(),
                ..HooksConfig::default()
            }),
            cwd: self.cwd.clone(),
        }
    }

    /// Run the `pre_tool_use` hooks for a tool call. `Err(reason)` means a
    /// hook vetoed the call; the reason is what the model should see.
    pub async fn pre_tool_use(
        &self,
        session_id: &str,
        tool: &str,
        input: &serde_json::Value,
    ) -> Result<(), String> {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": session_id,
            "cwd": self.cwd,
            "tool_name": tool,
            "tool_input": input,
        });
        for entry in &self.config.pre_tool_use {
            if !entry.matches_tool(tool) {
                continue;
            }
            let Some(run) = self.run(entry, "PreToolUse", Some(tool), &payload).await else {
                continue;
            };
            if run.blocked() {
                return Err(block_reason(entry, &run));
            }
            log_failure(entry, &run);
        }
        Ok(())
    }

    /// Run the `post_tool_use` hooks after a tool call. Returns text to show
    /// the model alongside the tool's output (hook stdout on success, stderr
    /// on exit 2), or `None` if the hooks had nothing to say.
    pub async fn post_tool_use(
        &self,
        session_id: &str,
        tool: &str,
        input: &serde_json::Value,
        output: &str,
        success: bool,
    ) -> Option<String> {
        let payload = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": session_id,
            "cwd": self.cwd,
            "tool_name": tool,
            "tool_input": input,
            "tool_response": { "output": output, "success": success },
        });
        let mut feedback: Vec<String> = Vec::new();
        for entry in &self.config.post_tool_use {
            if !entry.matches_tool(tool) {
                continue;
            }
            let Some(run) = self.run(entry, "PostToolUse", Some(tool), &payload).await else {
                continue;
            };
            let text = if run.blocked() {
                run.explanation()
            } else if run.code == Some(0) {
                run.stdout.trim().to_string()
            } else {
                log_failure(entry, &run);
                continue;
            };
            if !text.is_empty() {
                feedback.push(text);
            }
        }
        (!feedback.is_empty()).then(|| feedback.join("\n"))
    }

    /// Run the `user_prompt_submit` hooks. `Err(reason)` rejects the prompt;
    /// `Ok(Some(context))` carries stdout to add to the prompt.
    pub async fn user_prompt_submit(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<Option<String>, String> {
        let payload = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "cwd": self.cwd,
            "prompt": prompt,
        });
        let mut context: Vec<String> = Vec::new();
        for entry in &self.config.user_prompt_submit {
            let Some(run) = self.run(entry, "UserPromptSubmit", None, &payload).await else {
                continue;
            };
            if run.blocked() {
                return Err(block_reason(entry, &run));
            }
            if run.code == Some(0) {
                let text = run.stdout.trim();
                if !text.is_empty() {
                    context.push(text.to_string());
                }
            } else {
                log_failure(entry, &run);
            }
        }
        Ok((!context.is_empty()).then(|| context.join("\n")))
    }

    /// Run the `stop` hooks after a turn completed. Results are ignored.
    pub async fn stop(&self, session_id: &str, response: &str) {
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": session_id,
            "cwd": self.cwd,
            "response": response,
        });
        for entry in &self.config.stop {
            if let Some(run) = self.run(entry, "Stop", None, &payload).await {
                log_failure(entry, &run);
            }
        }
    }

    /// Spawn one hook, feed it `payload` on stdin, and wait for it. `None`
    /// means it could not be run or timed out (already logged).
    async fn run(
        &self,
        entry: &HookEntry,
        event: &str,
        tool: Option<&str>,
        payload: &serde_json::Value,
    ) -> Option<HookRun> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&entry.command)
            .current_dir(&self.cwd)
            .env("IO_HOOK_EVENT", event)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            // Own process group, so a timeout can kill grandchildren too.
            .process_group(0);
        if let Some(tool) = tool {
            cmd.env("IO_TOOL_NAME", tool);
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                tracing::warn!("hook `{}` failed to start: {e}", entry.command);
                return None;
            }
        };
        let pid = child.id();
        let mut stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let input = payload.to_string();

        let work = async {
            // Writing, reading and waiting run concurrently so a hook that
            // ignores stdin, or floods stdout, cannot deadlock on a full pipe.
            let write = async {
                if let Some(stdin) = stdin.as_mut() {
                    // A hook that exits without reading stdin closes the
                    // pipe; that's not an error worth reporting.
                    let _ = stdin.write_all(input.as_bytes()).await;
                }
                drop(stdin.take());
            };
            let (_, out, err, status) = tokio::join!(
                write,
                read_capped(stdout),
                read_capped(stderr),
                child.wait()
            );
            (out, err, status)
        };

        match tokio::time::timeout(entry.timeout(), work).await {
            Ok((stdout, stderr, status)) => match status {
                Ok(status) => Some(HookRun {
                    code: status.code(),
                    stdout,
                    stderr,
                }),
                Err(e) => {
                    tracing::warn!("hook `{}` failed: {e}", entry.command);
                    None
                }
            },
            Err(_) => {
                tracing::warn!(
                    "hook `{}` timed out after {:?} and was killed",
                    entry.command,
                    entry.timeout()
                );
                if let Some(pid) = pid {
                    // SAFETY: plain signal to the process group we created.
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                let _ = child.start_kill();
                None
            }
        }
    }
}

fn block_reason(entry: &HookEntry, run: &HookRun) -> String {
    let reason = run.explanation();
    if reason.is_empty() {
        format!("blocked by hook `{}`", entry.command)
    } else {
        reason
    }
}

/// Log a hook that ran but reported an error other than the block code.
fn log_failure(entry: &HookEntry, run: &HookRun) {
    match run.code {
        Some(0) => {}
        code => tracing::warn!(
            "hook `{}` exited with {}: {}",
            entry.command,
            code.map_or_else(|| "a signal".to_string(), |c| c.to_string()),
            run.stderr.trim()
        ),
    }
}

/// Read a stream to the end, keeping at most [`MAX_OUTPUT_BYTES`] and
/// draining (discarding) the rest so the writer never blocks on a full pipe.
async fn read_capped<R: AsyncRead + Unpin>(stream: Option<R>) -> String {
    let Some(mut stream) = stream else {
        return String::new();
    };
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn entry(matcher: Option<&str>, command: &str) -> HookEntry {
        HookEntry {
            matcher: matcher.map(str::to_string),
            command: command.to_string(),
            timeout_ms: None,
        }
    }

    fn hooks_with(config: HooksConfig) -> Hooks {
        Hooks::new(config, std::env::temp_dir())
    }

    fn temp_file(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("io-hook-{}-{n}-{name}", std::process::id()))
    }

    #[test]
    fn matcher_selects_tools() {
        assert!(entry(None, "x").matches_tool("bash"));
        assert!(entry(Some("*"), "x").matches_tool("bash"));
        assert!(entry(Some(""), "x").matches_tool("bash"));
        assert!(entry(Some("bash"), "x").matches_tool("bash"));
        assert!(!entry(Some("bash"), "x").matches_tool("write"));
        assert!(entry(Some("write|edit"), "x").matches_tool("edit"));
        assert!(entry(Some("write | edit"), "x").matches_tool("edit"));
        assert!(!entry(Some("write|edit"), "x").matches_tool("bash"));
        // Exact names, not substrings.
        assert!(!entry(Some("bash"), "x").matches_tool("bash_extra"));
    }

    #[test]
    fn config_parses_from_toml() {
        let config: HooksConfig = toml::from_str(
            r#"
            [[pre_tool_use]]
            matcher = "bash"
            command = "guard.sh"
            timeout_ms = 500

            [[post_tool_use]]
            command = "cargo fmt"
            "#,
        )
        .unwrap();
        assert_eq!(config.pre_tool_use.len(), 1);
        assert_eq!(config.pre_tool_use[0].matcher.as_deref(), Some("bash"));
        assert_eq!(config.pre_tool_use[0].timeout_ms, Some(500));
        assert_eq!(config.post_tool_use[0].matcher, None);
        assert!(config.stop.is_empty());
        assert!(HooksConfig::default() == toml::from_str("").unwrap());
    }

    #[test]
    fn empty_default_does_nothing() {
        assert!(Hooks::default().is_empty());
        assert!(!hooks_with(HooksConfig {
            stop: vec![entry(None, "true")],
            ..Default::default()
        })
        .is_empty());
    }

    #[tokio::test]
    async fn pre_tool_use_blocks_on_exit_2_with_stderr_reason() {
        let hooks = hooks_with(HooksConfig {
            pre_tool_use: vec![entry(Some("bash"), "echo 'no rm here' >&2; exit 2")],
            ..Default::default()
        });
        let err = hooks
            .pre_tool_use("s", "bash", &serde_json::json!({"command": "rm x"}))
            .await
            .unwrap_err();
        assert_eq!(err, "no rm here");
    }

    #[tokio::test]
    async fn pre_tool_use_block_without_message_names_the_hook() {
        let hooks = hooks_with(HooksConfig {
            pre_tool_use: vec![entry(None, "exit 2")],
            ..Default::default()
        });
        let err = hooks
            .pre_tool_use("s", "bash", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(err, "blocked by hook `exit 2`");
    }

    #[tokio::test]
    async fn pre_tool_use_passes_on_success_other_failures_and_matcher_miss() {
        let hooks = hooks_with(HooksConfig {
            pre_tool_use: vec![
                entry(Some("bash"), "exit 0"),
                // Any non-2 failure is non-blocking.
                entry(Some("bash"), "exit 1"),
                // Would block, but does not match this tool.
                entry(Some("write"), "exit 2"),
            ],
            ..Default::default()
        });
        assert!(hooks
            .pre_tool_use("s", "bash", &serde_json::json!({}))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn hook_receives_json_on_stdin_and_env() {
        let stdin_file = temp_file("stdin.json");
        let env_file = temp_file("env.txt");
        let command = format!(
            "cat > {}; printf '%s %s' \"$IO_HOOK_EVENT\" \"$IO_TOOL_NAME\" > {}",
            stdin_file.display(),
            env_file.display()
        );
        let hooks = hooks_with(HooksConfig {
            pre_tool_use: vec![entry(None, &command)],
            ..Default::default()
        });
        hooks
            .pre_tool_use("sess-1", "bash", &serde_json::json!({"command": "ls"}))
            .await
            .unwrap();

        let payload: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&stdin_file).unwrap()).unwrap();
        assert_eq!(payload["hook_event_name"], "PreToolUse");
        assert_eq!(payload["session_id"], "sess-1");
        assert_eq!(payload["tool_name"], "bash");
        assert_eq!(payload["tool_input"]["command"], "ls");
        assert_eq!(
            std::fs::read_to_string(&env_file).unwrap(),
            "PreToolUse bash"
        );
        std::fs::remove_file(&stdin_file).ok();
        std::fs::remove_file(&env_file).ok();
    }

    #[tokio::test]
    async fn hook_runs_in_the_configured_directory() {
        let out = temp_file("pwd.txt");
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let hooks = Hooks::new(
            HooksConfig {
                stop: vec![entry(None, &format!("pwd > {}", out.display()))],
                ..Default::default()
            },
            dir.clone(),
        );
        hooks.stop("s", "done").await;
        let ran_in = std::fs::read_to_string(&out).unwrap();
        assert_eq!(PathBuf::from(ran_in.trim()).canonicalize().unwrap(), dir);
        std::fs::remove_file(&out).ok();
    }

    #[tokio::test]
    async fn post_tool_use_returns_stdout_and_exit_2_stderr() {
        let hooks = hooks_with(HooksConfig {
            post_tool_use: vec![
                entry(Some("write"), "echo formatted"),
                entry(Some("write"), "echo 'lint failed' >&2; exit 2"),
                // Failing hooks and non-matching hooks contribute nothing.
                entry(Some("write"), "echo ignored; exit 1"),
                entry(Some("read"), "echo wrong tool"),
            ],
            ..Default::default()
        });
        let feedback = hooks
            .post_tool_use("s", "write", &serde_json::json!({}), "ok", true)
            .await
            .unwrap();
        assert_eq!(feedback, "formatted\nlint failed");

        let quiet = hooks_with(HooksConfig {
            post_tool_use: vec![entry(None, "true")],
            ..Default::default()
        });
        assert!(quiet
            .post_tool_use("s", "write", &serde_json::json!({}), "ok", true)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn post_tool_use_payload_carries_the_tool_response() {
        let out = temp_file("post.json");
        let hooks = hooks_with(HooksConfig {
            post_tool_use: vec![entry(None, &format!("cat > {}", out.display()))],
            ..Default::default()
        });
        hooks
            .post_tool_use("s", "read", &serde_json::json!({}), "file body", false)
            .await;
        let payload: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(payload["hook_event_name"], "PostToolUse");
        assert_eq!(payload["tool_response"]["output"], "file body");
        assert_eq!(payload["tool_response"]["success"], false);
        std::fs::remove_file(&out).ok();
    }

    #[tokio::test]
    async fn user_prompt_submit_can_block_or_add_context() {
        let adds = hooks_with(HooksConfig {
            user_prompt_submit: vec![entry(None, "echo branch: main")],
            ..Default::default()
        });
        assert_eq!(
            adds.user_prompt_submit("s", "hi").await.unwrap().as_deref(),
            Some("branch: main")
        );

        let blocks = hooks_with(HooksConfig {
            user_prompt_submit: vec![entry(None, "echo 'secrets in prompt' >&2; exit 2")],
            ..Default::default()
        });
        assert_eq!(
            blocks.user_prompt_submit("s", "hi").await.unwrap_err(),
            "secrets in prompt"
        );

        assert_eq!(
            Hooks::default().user_prompt_submit("s", "hi").await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn timeout_is_non_blocking_and_kills_the_hook() {
        let marker = temp_file("late.txt");
        let hooks = hooks_with(HooksConfig {
            pre_tool_use: vec![HookEntry {
                matcher: None,
                command: format!("sleep 5; echo done > {}", marker.display()),
                timeout_ms: Some(100),
            }],
            ..Default::default()
        });
        let started = std::time::Instant::now();
        assert!(hooks
            .pre_tool_use("s", "bash", &serde_json::json!({}))
            .await
            .is_ok());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "timeout should cut the hook short"
        );
        assert!(!marker.exists(), "timed-out hook must not keep running");
    }

    #[tokio::test]
    async fn large_output_does_not_deadlock_and_is_capped() {
        let hooks = hooks_with(HooksConfig {
            user_prompt_submit: vec![entry(None, "head -c 200000 /dev/zero | tr '\\0' x")],
            ..Default::default()
        });
        let ctx = hooks
            .user_prompt_submit("s", "hi")
            .await
            .unwrap()
            .expect("stdout captured");
        assert_eq!(ctx.len(), MAX_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn hook_that_ignores_a_large_stdin_payload_does_not_hang() {
        let hooks = hooks_with(HooksConfig {
            post_tool_use: vec![entry(None, "echo ok")],
            ..Default::default()
        });
        let big = "y".repeat(1_000_000);
        let feedback = hooks
            .post_tool_use("s", "read", &serde_json::json!({}), &big, true)
            .await;
        assert_eq!(feedback.as_deref(), Some("ok"));
    }

    #[test]
    fn sub_agents_inherit_tool_hooks_only() {
        let hooks = hooks_with(HooksConfig {
            pre_tool_use: vec![entry(None, "a")],
            post_tool_use: vec![entry(None, "b")],
            user_prompt_submit: vec![entry(None, "c")],
            stop: vec![entry(None, "d")],
        });
        let sub = hooks.for_sub_agent();
        assert_eq!(sub.config.pre_tool_use.len(), 1);
        assert_eq!(sub.config.post_tool_use.len(), 1);
        assert!(sub.config.user_prompt_submit.is_empty());
        assert!(sub.config.stop.is_empty());
    }
}
