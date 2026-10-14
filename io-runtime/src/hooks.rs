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
//! Beyond the four tool/turn events, `session_start`, `session_end`,
//! `notification` and `pre_compact` fire at the matching
//! points of a session. For events with a natural name to match, `matcher`
//! selects on it: the session-start source (`startup` / `resume`), the
//! compaction trigger (`manual` / `auto`), or the notification kind
//! (`permission`).
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
//! A hook that exits 0 may instead print a JSON object for finer control:
//!
//! ```json
//! { "decision": "deny", "reason": "no rm here", "additionalContext": "…" }
//! ```
//!
//! `decision` is `block`/`deny` (same as exit 2), `ask` (make the permission
//! check prompt even if it would have allowed the call) or `allow` (a no-op:
//! hooks never grant permission). `additionalContext` is extra text for the
//! model, `continue: false` with `stopReason` is another way to block, and the
//! Claude-Code-style `hookSpecificOutput.permissionDecision` /
//! `permissionDecisionReason` spelling is accepted too. Output that is not a
//! JSON object is treated as plain text, as before.
//!
//! Hooks can only add restrictions: `pre_tool_use` runs before the
//! permission check and can veto a call or force a prompt, but can never
//! grant permission.

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
    /// Once per session, before its first turn (`startup` or `resume`).
    /// stdout is added to that first prompt as context.
    #[serde(default)]
    pub session_start: Vec<HookEntry>,
    /// When a session ends (the app exits, or `/new` replaces it).
    #[serde(default)]
    pub session_end: Vec<HookEntry>,
    /// When io needs the user's attention (a permission prompt). Fire and
    /// forget: never delays the prompt.
    #[serde(default)]
    pub notification: Vec<HookEntry>,
    /// Before the conversation is compacted (`manual` or `auto`). Exit 2
    /// cancels the compaction; stdout becomes extra summarization guidance.
    #[serde(default)]
    pub pre_compact: Vec<HookEntry>,
}

impl HooksConfig {
    /// Append every entry of `other` after this config's own, event by event.
    /// Used to layer plugin-provided hooks onto the user's.
    pub fn merge(&mut self, other: HooksConfig) {
        self.pre_tool_use.extend(other.pre_tool_use);
        self.post_tool_use.extend(other.post_tool_use);
        self.user_prompt_submit.extend(other.user_prompt_submit);
        self.stop.extend(other.stop);
        self.session_start.extend(other.session_start);
        self.session_end.extend(other.session_end);
        self.notification.extend(other.notification);
        self.pre_compact.extend(other.pre_compact);
    }
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
    /// Whether this entry applies to `tool` — or, for the non-tool events, to
    /// the event's own name (source, trigger, kind, sub-agent id).
    fn matches_tool(&self, tool: &str) -> bool {
        match self.matcher.as_deref().map(str::trim) {
            None | Some("") | Some("*") => true,
            Some(pattern) => pattern.split('|').any(|name| glob_match(name.trim(), tool)),
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

/// A hook's structured (JSON) output. Unknown fields are ignored.
#[derive(Debug, Default, Deserialize)]
struct HookJson {
    decision: Option<String>,
    reason: Option<String>,
    #[serde(rename = "additionalContext", alias = "additional_context")]
    additional_context: Option<String>,
    #[serde(rename = "continue")]
    keep_going: Option<bool>,
    #[serde(rename = "stopReason", alias = "stop_reason")]
    stop_reason: Option<String>,
    #[serde(rename = "hookSpecificOutput")]
    specific: Option<HookSpecific>,
}

#[derive(Debug, Default, Deserialize)]
struct HookSpecific {
    #[serde(rename = "permissionDecision")]
    permission_decision: Option<String>,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: Option<String>,
    #[serde(rename = "additionalContext")]
    additional_context: Option<String>,
}

/// What a finished hook asked for, whichever way it said it (exit code, JSON
/// or plain text).
#[derive(Debug, Default, PartialEq)]
struct Interpreted {
    /// The hook blocks the action; this is the reason to show.
    blocked: Option<String>,
    /// The hook wants the permission check to prompt.
    ask: bool,
    /// Extra text for the model from structured output.
    context: Option<String>,
    /// stdout when it was not a JSON object (the pre-JSON behavior).
    plain: Option<String>,
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

fn interpret(entry: &HookEntry, run: &HookRun) -> Interpreted {
    if run.blocked() {
        return Interpreted {
            blocked: Some(block_reason(entry, run)),
            ..Default::default()
        };
    }
    if run.code != Some(0) {
        log_failure(entry, run);
        return Interpreted::default();
    }
    let out = run.stdout.trim();
    let parsed = out
        .starts_with('{')
        .then(|| serde_json::from_str::<HookJson>(out).ok())
        .flatten();
    let Some(json) = parsed else {
        return Interpreted {
            plain: (!out.is_empty()).then(|| out.to_string()),
            ..Default::default()
        };
    };
    let specific = json.specific.unwrap_or_default();
    let decision = specific
        .permission_decision
        .or(json.decision)
        .map(|d| d.to_ascii_lowercase());
    let reason = non_empty(
        specific
            .permission_decision_reason
            .as_deref()
            .or(json.reason.as_deref())
            .or(json.stop_reason.as_deref()),
    );
    let stop = json.keep_going == Some(false);
    let blocked = (matches!(decision.as_deref(), Some("block" | "deny")) || stop)
        .then(|| reason.unwrap_or_else(|| format!("blocked by hook `{}`", entry.command)));
    Interpreted {
        blocked,
        ask: decision.as_deref() == Some("ask"),
        context: non_empty(specific.additional_context.as_deref())
            .or_else(|| non_empty(json.additional_context.as_deref())),
        plain: None,
    }
}

/// What the `pre_tool_use` hooks decided for a call that was not vetoed.
#[derive(Debug, Default, PartialEq)]
pub struct PreToolOutcome {
    /// A hook asked that the permission check prompt for this call.
    pub ask: bool,
    /// Extra text for the model, appended to the tool's result.
    pub context: Option<String>,
}

/// Runs the configured hooks. Cheap to clone; an empty `Hooks` (the default)
/// does nothing, so callers never need to branch on whether hooks exist.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    config: Arc<HooksConfig>,
    cwd: PathBuf,
    /// Whether `session_start` has already fired for the current session.
    /// Shared across clones (and, via [`Hooks::with_session_gate`], across
    /// agent rebuilds) so a `/model` switch doesn't re-announce the session.
    session_started: Arc<std::sync::atomic::AtomicBool>,
}

impl Hooks {
    /// `cwd` is the directory hooks run in — the project root.
    pub fn new(config: HooksConfig, cwd: PathBuf) -> Self {
        Self {
            config: Arc::new(config),
            cwd,
            session_started: Arc::default(),
        }
    }

    /// Share the "session start already fired" flag with other `Hooks`, so
    /// rebuilding the agent keeps the same session announced only once.
    pub fn with_session_gate(mut self, gate: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.session_started = gate;
        self
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
            session_started: Arc::default(),
        }
    }

    /// Run the `pre_tool_use` hooks for a tool call. `Err(reason)` means a
    /// hook vetoed the call; the reason is what the model should see.
    /// `Ok` carries anything else the hooks asked for: a forced permission
    /// prompt and/or context for the model.
    pub async fn pre_tool_use(
        &self,
        session_id: &str,
        tool: &str,
        input: &serde_json::Value,
    ) -> Result<PreToolOutcome, String> {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": session_id,
            "cwd": self.cwd,
            "tool_name": tool,
            "tool_input": input,
        });
        let mut outcome = PreToolOutcome::default();
        let mut context: Vec<String> = Vec::new();
        for entry in &self.config.pre_tool_use {
            if !entry.matches_tool(tool) {
                continue;
            }
            let Some(run) = self.run(entry, "PreToolUse", Some(tool), &payload).await else {
                continue;
            };
            let verdict = interpret(entry, &run);
            if let Some(reason) = verdict.blocked {
                return Err(reason);
            }
            outcome.ask |= verdict.ask;
            context.extend(verdict.context);
        }
        outcome.context = (!context.is_empty()).then(|| context.join("\n"));
        Ok(outcome)
    }

    /// Run the `post_tool_use` hooks after a tool call. Returns text to show
    /// the model alongside the tool's output (hook stdout or
    /// `additionalContext` on success, the reason on a block), or `None` if
    /// the hooks had nothing to say.
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
            let v = interpret(entry, &run);
            // The tool already ran, so a block can only be feedback.
            feedback.extend(v.blocked.or(v.context).or(v.plain));
        }
        (!feedback.is_empty()).then(|| feedback.join("\n"))
    }

    /// Run the `user_prompt_submit` hooks. `Err(reason)` rejects the prompt;
    /// `Ok(Some(context))` carries text to add to the prompt.
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
            let v = interpret(entry, &run);
            if let Some(reason) = v.blocked {
                return Err(reason);
            }
            context.extend(v.context.or(v.plain));
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

    /// Run the `session_start` hooks, once per session (further calls return
    /// `None` until the gate is reset). `source` is `startup` for a fresh
    /// session and `resume` for one with history. The returned text — hook
    /// stdout or `additionalContext` — is context for the first prompt.
    pub async fn session_start(&self, session_id: &str, source: &str) -> Option<String> {
        use std::sync::atomic::Ordering;
        if self.config.session_start.is_empty() || self.session_started.swap(true, Ordering::SeqCst)
        {
            return None;
        }
        let payload = serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "cwd": self.cwd,
            "source": source,
        });
        let mut context: Vec<String> = Vec::new();
        for entry in &self.config.session_start {
            if !entry.matches_tool(source) {
                continue;
            }
            let Some(run) = self.run(entry, "SessionStart", None, &payload).await else {
                continue;
            };
            let v = interpret(entry, &run);
            context.extend(v.context.or(v.plain));
        }
        (!context.is_empty()).then(|| context.join("\n"))
    }

    /// Forget that the session start fired, so the next session announces
    /// itself (`/new`).
    pub fn reset_session_gate(&self) {
        self.session_started
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Run the `session_end` hooks. `reason` is `exit`, `new` or `single_shot`.
    pub async fn session_end(&self, session_id: &str, reason: &str) {
        let payload = serde_json::json!({
            "hook_event_name": "SessionEnd",
            "session_id": session_id,
            "cwd": self.cwd,
            "reason": reason,
        });
        for entry in &self.config.session_end {
            if !entry.matches_tool(reason) {
                continue;
            }
            if let Some(run) = self.run(entry, "SessionEnd", None, &payload).await {
                log_failure(entry, &run);
            }
        }
    }

    /// Run the `notification` hooks in the background. `kind` is currently
    /// `permission`. Never waits: a slow notifier must not delay the prompt
    /// it announces.
    pub fn notify(&self, session_id: &str, kind: &str, message: &str) {
        if self.config.notification.is_empty() {
            return;
        }
        let hooks = self.clone();
        let payload = serde_json::json!({
            "hook_event_name": "Notification",
            "session_id": session_id,
            "cwd": self.cwd,
            "notification_type": kind,
            "message": message,
        });
        let kind = kind.to_string();
        tokio::spawn(async move {
            for entry in &hooks.config.notification {
                if !entry.matches_tool(&kind) {
                    continue;
                }
                if let Some(run) = hooks.run(entry, "Notification", None, &payload).await {
                    log_failure(entry, &run);
                }
            }
        });
    }

    /// Run the `pre_compact` hooks. `Err(reason)` cancels the compaction;
    /// `Ok(Some(text))` is extra guidance for the summary. `trigger` is
    /// `manual` or `auto`.
    pub async fn pre_compact(
        &self,
        session_id: &str,
        trigger: &str,
    ) -> Result<Option<String>, String> {
        let payload = serde_json::json!({
            "hook_event_name": "PreCompact",
            "session_id": session_id,
            "cwd": self.cwd,
            "trigger": trigger,
        });
        let mut guidance: Vec<String> = Vec::new();
        for entry in &self.config.pre_compact {
            if !entry.matches_tool(trigger) {
                continue;
            }
            let Some(run) = self.run(entry, "PreCompact", None, &payload).await else {
                continue;
            };
            let v = interpret(entry, &run);
            if let Some(reason) = v.blocked {
                return Err(reason);
            }
            guidance.extend(v.context.or(v.plain));
        }
        Ok((!guidance.is_empty()).then(|| guidance.join("\n")))
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

/// Match `text` against `pattern`, where `*` stands for any run of
/// characters (so `mcp__github__*` selects a server's tools). Without a `*`
/// it is an exact comparison.
fn glob_match(pattern: &str, text: &str) -> bool {
    let Some((first, rest)) = pattern.split_once('*') else {
        return pattern == text;
    };
    let Some(mut remaining) = text.strip_prefix(first) else {
        return false;
    };
    let mut parts = rest.split('*').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            // The last piece must anchor at the end.
            return remaining.ends_with(part);
        }
        match remaining.find(part) {
            Some(i) => remaining = &remaining[i + part.len()..],
            None => return false,
        }
    }
    true
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
    fn matchers_support_star_wildcards_for_mcp_tools() {
        let gh = entry(Some("mcp__github__*"), "x");
        assert!(gh.matches_tool("mcp__github__search"));
        assert!(gh.matches_tool("mcp__github__"));
        assert!(!gh.matches_tool("mcp__gitlab__search"));
        assert!(!gh.matches_tool("bash"));
        let any_mcp = entry(Some("write|mcp__*"), "x");
        assert!(any_mcp.matches_tool("mcp__a__b"));
        assert!(any_mcp.matches_tool("write"));
        assert!(entry(Some("mcp__*__delete*"), "x").matches_tool("mcp__gh__delete_issue"));
        assert!(!entry(Some("mcp__*__delete*"), "x").matches_tool("mcp__gh__create"));
        assert!(entry(Some("*_issue"), "x").matches_tool("mcp__gh__close_issue"));
        assert!(!entry(Some("*_issue"), "x").matches_tool("mcp__gh__close_issue2"));
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
            session_start: vec![entry(None, "f")],
            ..Default::default()
        });
        let sub = hooks.for_sub_agent();
        assert_eq!(sub.config.pre_tool_use.len(), 1);
        assert_eq!(sub.config.post_tool_use.len(), 1);
        assert!(sub.config.user_prompt_submit.is_empty());
        assert!(sub.config.stop.is_empty());
        assert!(sub.config.session_start.is_empty());
    }

    // ── JSON output ───────────────────────────────────────────────────────

    fn json_hook(json: &str) -> HookEntry {
        // printf avoids echo's backslash handling.
        entry(None, &format!("printf '%s' '{json}'"))
    }

    async fn pre(json: &str) -> Result<PreToolOutcome, String> {
        hooks_with(HooksConfig {
            pre_tool_use: vec![json_hook(json)],
            ..Default::default()
        })
        .pre_tool_use("s", "bash", &serde_json::json!({}))
        .await
    }

    #[tokio::test]
    async fn json_deny_blocks_like_exit_2_with_its_reason() {
        for json in [
            r#"{"decision":"deny","reason":"nope"}"#,
            r#"{"decision":"block","reason":"nope"}"#,
            r#"{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"nope"}}"#,
            r#"{"continue":false,"stopReason":"nope"}"#,
        ] {
            assert_eq!(pre(json).await.unwrap_err(), "nope", "{json}");
        }
        // No reason given: the hook is named.
        assert!(pre(r#"{"decision":"deny"}"#)
            .await
            .unwrap_err()
            .starts_with("blocked by hook"));
    }

    #[tokio::test]
    async fn json_ask_forces_a_prompt_and_allow_is_a_no_op() {
        let ask = pre(r#"{"decision":"ask"}"#).await.unwrap();
        assert!(ask.ask);
        // Hooks never grant permission: "allow" changes nothing.
        let allow = pre(r#"{"decision":"allow","reason":"trusted"}"#)
            .await
            .unwrap();
        assert_eq!(allow, PreToolOutcome::default());
        let spec = pre(r#"{"hookSpecificOutput":{"permissionDecision":"ask"}}"#)
            .await
            .unwrap();
        assert!(spec.ask);
    }

    #[tokio::test]
    async fn json_additional_context_reaches_the_model_and_plain_stdout_does_not() {
        let out = pre(r#"{"additionalContext":"the build is red"}"#)
            .await
            .unwrap();
        assert_eq!(out.context.as_deref(), Some("the build is red"));
        // Plain stdout from a pre_tool_use hook is still ignored.
        let plain = pre("just words").await.unwrap();
        assert_eq!(plain, PreToolOutcome::default());
        // Malformed JSON is plain text, not an error or a block.
        let broken = pre(r#"{"decision": "deny""#).await.unwrap();
        assert_eq!(broken, PreToolOutcome::default());
    }

    #[tokio::test]
    async fn json_works_for_post_tool_and_prompt_hooks() {
        let post = hooks_with(HooksConfig {
            post_tool_use: vec![
                json_hook(r#"{"additionalContext":"formatted"}"#),
                json_hook(r#"{"decision":"block","reason":"lint failed"}"#),
            ],
            ..Default::default()
        });
        assert_eq!(
            post.post_tool_use("s", "write", &serde_json::json!({}), "ok", true)
                .await
                .as_deref(),
            Some("formatted\nlint failed")
        );

        let prompt = hooks_with(HooksConfig {
            user_prompt_submit: vec![json_hook(r#"{"additionalContext":"on main"}"#)],
            ..Default::default()
        });
        assert_eq!(
            prompt
                .user_prompt_submit("s", "hi")
                .await
                .unwrap()
                .as_deref(),
            Some("on main")
        );
        let blocked = hooks_with(HooksConfig {
            user_prompt_submit: vec![json_hook(r#"{"decision":"block","reason":"secrets"}"#)],
            ..Default::default()
        });
        assert_eq!(
            blocked.user_prompt_submit("s", "hi").await.unwrap_err(),
            "secrets"
        );
    }

    // ── New events ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn session_start_fires_once_and_returns_context() {
        let hooks = hooks_with(HooksConfig {
            session_start: vec![entry(None, "echo 'branch: main'")],
            ..Default::default()
        });
        assert_eq!(
            hooks.session_start("s", "startup").await.as_deref(),
            Some("branch: main")
        );
        // Already announced — even through a clone sharing the gate.
        assert_eq!(hooks.clone().session_start("s", "startup").await, None);
        hooks.reset_session_gate();
        assert!(hooks.session_start("s", "resume").await.is_some());
    }

    #[tokio::test]
    async fn session_start_matcher_selects_the_source_and_the_gate_can_be_shared() {
        let only_resume = hooks_with(HooksConfig {
            session_start: vec![entry(Some("resume"), "echo resumed")],
            ..Default::default()
        });
        assert_eq!(only_resume.session_start("s", "startup").await, None);

        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let config = HooksConfig {
            session_start: vec![entry(None, "echo hi")],
            ..Default::default()
        };
        let first =
            Hooks::new(config.clone(), std::env::temp_dir()).with_session_gate(gate.clone());
        let rebuilt = Hooks::new(config, std::env::temp_dir()).with_session_gate(gate);
        assert!(first.session_start("s", "startup").await.is_some());
        assert_eq!(rebuilt.session_start("s", "startup").await, None);
    }

    #[tokio::test]
    async fn session_end_receives_its_payload_and_honors_the_matcher() {
        let end = temp_file("end.json");
        let hooks = hooks_with(HooksConfig {
            session_end: vec![entry(Some("exit"), &format!("cat > {}", end.display()))],
            ..Default::default()
        });
        hooks.session_end("s1", "new").await;
        assert!(!end.exists(), "the matcher only selects `exit`");
        hooks.session_end("s1", "exit").await;
        let payload: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&end).unwrap()).unwrap();
        assert_eq!(payload["hook_event_name"], "SessionEnd");
        assert_eq!(payload["reason"], "exit");
        std::fs::remove_file(&end).ok();
    }

    #[tokio::test]
    async fn pre_compact_can_cancel_or_add_guidance() {
        let guide = hooks_with(HooksConfig {
            pre_compact: vec![entry(None, "echo 'keep the TODO list'")],
            ..Default::default()
        });
        assert_eq!(
            guide.pre_compact("s", "manual").await.unwrap().as_deref(),
            Some("keep the TODO list")
        );
        let cancel = hooks_with(HooksConfig {
            pre_compact: vec![entry(Some("auto"), "echo 'not now' >&2; exit 2")],
            ..Default::default()
        });
        assert_eq!(
            cancel.pre_compact("s", "auto").await.unwrap_err(),
            "not now"
        );
        // The matcher keeps it from applying to a manual compaction.
        assert_eq!(cancel.pre_compact("s", "manual").await, Ok(None));
    }

    #[tokio::test]
    async fn notify_runs_in_the_background() {
        let out = temp_file("notify.json");
        let hooks = hooks_with(HooksConfig {
            notification: vec![entry(None, &format!("cat > {}", out.display()))],
            ..Default::default()
        });
        hooks.notify("s", "permission", "io needs permission to run bash");
        for _ in 0..100 {
            if out.exists()
                && std::fs::metadata(&out)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let payload: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(payload["hook_event_name"], "Notification");
        assert_eq!(payload["notification_type"], "permission");
        assert!(payload["message"].as_str().unwrap().contains("permission"));
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn configs_merge_event_by_event_and_parse_the_new_tables() {
        let mut base = HooksConfig {
            stop: vec![entry(None, "a")],
            ..Default::default()
        };
        let plugin: HooksConfig = toml::from_str(
            r#"
            [[stop]]
            command = "b"
            [[session_start]]
            command = "c"
            [[pre_compact]]
            matcher = "auto"
            command = "d"
            [[session_end]]
            command = "f"
            [[notification]]
            command = "g"
            "#,
        )
        .unwrap();
        base.merge(plugin);
        assert_eq!(base.stop.len(), 2);
        assert_eq!(base.session_start.len(), 1);
        assert_eq!(base.pre_compact[0].matcher.as_deref(), Some("auto"));
        assert_eq!(base.session_end.len() + base.notification.len(), 2);
    }
}
