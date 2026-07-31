use super::{Tool, ToolInput, ToolOutput};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;

const MAX_OUTPUT_BYTES: usize = 100_000;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// How often the bash tool polls the shared cancellation flag so Esc can
/// interrupt a running command.
const CANCEL_POLL_MS: u64 = 100;

const ALLOWED_SHELLS: &[&str] = &[
    "/bin/sh",
    "/bin/bash",
    "/usr/bin/bash",
    "/bin/zsh",
    "/usr/bin/zsh",
    "/usr/local/bin/bash",
    "/usr/local/bin/zsh",
    "/usr/local/bin/sh",
];

/// Lifecycle of a background job.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JobStatus {
    Running,
    Exited { code: Option<i32> },
    TimedOut,
    Cancelled,
}

impl JobStatus {
    fn label(&self) -> String {
        match self {
            JobStatus::Running => "running".to_string(),
            JobStatus::Exited { code: Some(c) } => format!("exited (code {c})"),
            JobStatus::Exited { code: None } => "exited (killed by signal)".to_string(),
            JobStatus::TimedOut => "timed out".to_string(),
            JobStatus::Cancelled => "cancelled".to_string(),
        }
    }
}

/// A running (or finished) background job.
struct BashJob {
    id: String,
    command: String,
    started: Instant,
    pgid: Option<u32>,
    status: Arc<StdMutex<JobStatus>>,
    stdout: Arc<TokioMutex<Vec<u8>>>,
    stderr: Arc<TokioMutex<Vec<u8>>>,
    /// Reader tasks still draining the pipes; joined before reading output
    /// once the job has finished so no trailing bytes are missed.
    readers: StdMutex<Option<(JoinHandle<()>, JoinHandle<()>)>>,
    /// The task that waits for exit / timeout / cancellation.
    waiter: StdMutex<Option<JoinHandle<()>>>,
}

/// Shared registry of background jobs. Jobs belong to the tool instance that
/// started them (the parent agent keeps its tool across turns, so background
/// jobs survive across turns in a session). When the tool is dropped, all
/// still-running jobs are killed — no orphans leak.
struct BashJobManager {
    jobs: StdMutex<std::collections::HashMap<String, Arc<BashJob>>>,
    next_id: AtomicU64,
}

impl BashJobManager {
    fn new() -> Self {
        Self {
            jobs: StdMutex::new(std::collections::HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    fn shutdown(&self) {
        let jobs = self.jobs.lock().unwrap();
        for job in jobs.values() {
            let running = matches!(*job.status.lock().unwrap(), JobStatus::Running);
            if running {
                kill_process_group(job.pgid);
            }
        }
    }
}

pub struct BashTool {
    jobs: Arc<BashJobManager>,
    cancel: StdMutex<Arc<AtomicBool>>,
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(BashJobManager::new()),
            cancel: StdMutex::new(Arc::new(AtomicBool::new(false))),
        }
    }

    fn shell() -> String {
        std::env::var("SHELL")
            .ok()
            .filter(|s| ALLOWED_SHELLS.contains(&s.as_str()))
            .unwrap_or_else(|| "/bin/sh".to_string())
    }

    fn timeout_ms(input: &ToolInput, background: bool) -> Option<u64> {
        let raw = input.args.get("timeout").and_then(|v| v.as_i64());
        match raw {
            Some(n) if n > 0 => Some(n as u64),
            // 0 means "no timeout" (only meaningful for background jobs; for
            // foreground commands an absent timeout still uses the default).
            Some(_) => None,
            None if background => None,
            None => Some(DEFAULT_TIMEOUT_MS),
        }
    }

    async fn run_foreground(
        &self,
        shell: &str,
        command: &str,
        workdir: &str,
        timeout_ms: Option<u64>,
    ) -> CommandOutput {
        let mut child = match spawn_child(shell, command, workdir) {
            Ok(c) => c,
            Err(e) => return CommandOutput::failed(format!("failed to spawn command: {e}")),
        };
        let pgid = child.id();
        let (out_buf, err_buf, out_h, err_h) = start_pumps(&mut child);
        let cancel = self.cancel.lock().unwrap().clone();
        let mut cancel_poll = tokio::time::interval(Duration::from_millis(CANCEL_POLL_MS));
        let deadline = timeout_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));

        // child.wait() must not be held as a pinned future while we also need
        // to kill the child on timeout/cancel, so the wait runs in its own
        // task and reports through a oneshot.
        let (wait_tx, mut wait_rx) = tokio::sync::oneshot::channel();
        let wait_task = tokio::spawn(async move {
            let st = child.wait().await;
            let _ = wait_tx.send(st);
        });
        let mut wait_task = Some(wait_task);

        let outcome = loop {
            tokio::select! {
                res = &mut wait_rx => {
                    let status = res.ok().and_then(|r| r.ok());
                    let code = status.as_ref().and_then(|s| s.code());
                    let success = status.map(|s| s.success()).unwrap_or(false);
                    if let Some(t) = wait_task.take() {
                        let _ = t.await;
                    }
                    break CommandOutput {
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: code.unwrap_or(-1),
                        status: success,
                        timed_out: false,
                        cancelled: false,
                    };
                }
                _ = async { tokio::time::sleep_until(deadline.unwrap()).await }, if deadline.is_some() => {
                    kill_process_group(pgid);
                    if let Some(t) = wait_task.take() {
                        let _ = t.await;
                    }
                    break CommandOutput::failed("").timed_out();
                }
                _ = cancel_poll.tick() => {
                    if cancel.load(Ordering::Relaxed) {
                        kill_process_group(pgid);
                        if let Some(t) = wait_task.take() {
                            let _ = t.await;
                        }
                        break CommandOutput::failed("").cancelled();
                    }
                }
            }
        };

        // Pipes are closed (child dead); drain whatever the readers captured.
        let _ = out_h.await;
        let _ = err_h.await;
        let mut out = String::from_utf8_lossy(&out_buf.lock().await).to_string();
        let mut err = String::from_utf8_lossy(&err_buf.lock().await).to_string();
        if out.len() > MAX_OUTPUT_BYTES {
            out.truncate(MAX_OUTPUT_BYTES);
        }
        if err.len() > MAX_OUTPUT_BYTES {
            err.truncate(MAX_OUTPUT_BYTES);
        }
        CommandOutput {
            stdout: out,
            stderr: err,
            ..outcome
        }
    }

    async fn start_background(
        &self,
        shell: &str,
        command: &str,
        workdir: &str,
        timeout_ms: Option<u64>,
    ) -> Result<String, String> {
        let mut child = spawn_child(shell, command, workdir)
            .map_err(|e| format!("failed to spawn command: {e}"))?;
        let pgid = child.id();
        let (out_buf, err_buf, out_h, err_h) = start_pumps(&mut child);
        let status = Arc::new(StdMutex::new(JobStatus::Running));
        let status_w = status.clone();
        let cancel = self.cancel.lock().unwrap().clone();
        let mut cancel_poll = tokio::time::interval(Duration::from_millis(CANCEL_POLL_MS));
        let deadline = timeout_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));

        // Same wait-task pattern as run_foreground: child.wait() lives in its
        // own task so the kill paths never fight for the child borrow.
        let (wait_tx, mut wait_rx) = tokio::sync::oneshot::channel();
        let wait_task = tokio::spawn(async move {
            let st = child.wait().await;
            let _ = wait_tx.send(st);
        });
        let mut wait_task = Some(wait_task);

        let waiter = tokio::spawn(async move {
            loop {
                tokio::select! {
                    res = &mut wait_rx => {
                        let status = res.ok().and_then(|r| r.ok());
                        let code = status.as_ref().and_then(|s| s.code());
                        if let Some(t) = wait_task.take() {
                            let _ = t.await;
                        }
                        *status_w.lock().unwrap() = JobStatus::Exited { code };
                        break;
                    }
                    _ = async { tokio::time::sleep_until(deadline.unwrap()).await }, if deadline.is_some() => {
                        kill_process_group(pgid);
                        if let Some(t) = wait_task.take() {
                            let _ = t.await;
                        }
                        *status_w.lock().unwrap() = JobStatus::TimedOut;
                        break;
                    }
                    _ = cancel_poll.tick() => {
                        if cancel.load(Ordering::Relaxed) {
                            kill_process_group(pgid);
                            if let Some(t) = wait_task.take() {
                                let _ = t.await;
                            }
                            *status_w.lock().unwrap() = JobStatus::Cancelled;
                            break;
                        }
                    }
                }
            }
        });

        let id = format!("job-{}", self.jobs.next_id.fetch_add(1, Ordering::Relaxed));
        let job = BashJob {
            id: id.clone(),
            command: command.to_string(),
            started: Instant::now(),
            pgid,
            status,
            stdout: out_buf,
            stderr: err_buf,
            readers: StdMutex::new(Some((out_h, err_h))),
            waiter: StdMutex::new(Some(waiter)),
        };
        self.jobs
            .jobs
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::new(job));
        Ok(id)
    }

    fn job(&self, job_id: &str) -> Result<Arc<BashJob>, String> {
        self.jobs
            .jobs
            .lock()
            .unwrap()
            .get(job_id)
            .cloned()
            .ok_or_else(|| format!("unknown job: {job_id}"))
    }

    fn job_status_text(&self, job: &BashJob) -> String {
        let st = job.status.lock().unwrap();
        let elapsed = job.started.elapsed().as_secs_f64();
        let cmd: String = job.command.chars().take(80).collect();
        format!("{}: {} ({:.1}s) {}", job.id, st.label(), elapsed, cmd)
    }

    async fn job_output(&self, job: &Arc<BashJob>) -> String {
        // Once the job is no longer running, drain the readers so no trailing
        // bytes are missed; for a running job, return what's captured so far.
        // The guard must be dropped before the awaits keep the future Send.
        let running = matches!(*job.status.lock().unwrap(), JobStatus::Running);
        if !running {
            let readers = job.readers.lock().unwrap().take();
            if let Some((h1, h2)) = readers {
                let _ = h1.await;
                let _ = h2.await;
            }
        }
        let out = String::from_utf8_lossy(&job.stdout.lock().await).to_string();
        let err = String::from_utf8_lossy(&job.stderr.lock().await).to_string();
        let st = job.status.lock().unwrap();
        let mut text = String::new();
        if !out.is_empty() {
            text.push_str("[stdout]\n");
            text.push_str(&out);
            text.push('\n');
        }
        if !err.is_empty() {
            text.push_str("[stderr]\n");
            text.push_str(&err);
            text.push('\n');
        }
        text.push_str(&format!("status: {}", st.label()));
        if text.len() > MAX_OUTPUT_BYTES {
            text.truncate(MAX_OUTPUT_BYTES);
            text.push_str("\n[output truncated]");
        }
        text
    }

    async fn kill_job(&self, job: &Arc<BashJob>) -> String {
        kill_process_group(job.pgid);
        let waiter = job.waiter.lock().unwrap().take();
        if let Some(w) = waiter {
            let _ = w.await;
        }
        let st = job.status.lock().unwrap();
        match *st {
            JobStatus::Exited { code: Some(c) } => {
                format!("job {} already exited (code {c})", job.id)
            }
            JobStatus::TimedOut => format!("job {} already timed out", job.id),
            JobStatus::Cancelled => format!("job {} already cancelled", job.id),
            _ => format!("killed job {} ({})", job.id, st.label()),
        }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for BashTool {
    fn drop(&mut self) {
        // No orphans: when the owning agent's tool registry is dropped
        // (sub-agent finished, agent rebuilt), kill anything still running.
        self.jobs.shutdown();
    }
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command, optionally in the background.\n\
         Actions (default: run):\n\
         - run: execute synchronously and return stdout/stderr/exit code. A timeout (ms) kills the command and returns partial output.\n\
         - start: launch a command in the background and return a job id. The job keeps running across turns; check it later.\n\
         - status <job_id>: running/exited/timeout/cancelled + elapsed time.\n\
         - output <job_id>: captured stdout/stderr so far.\n\
         - kill <job_id>: terminate a background job.\n\
         - list: enumerate background jobs.\n\
         Long-running work (builds, tests, servers) should use start + status/output so it doesn't block the turn."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute (required for run/start)" },
                "action": {
                    "type": "string",
                    "enum": ["run", "start", "status", "output", "kill", "list"],
                    "description": "run (default): synchronous execution; start: background job; status/output/kill: manage a job; list: show jobs"
                },
                "job_id": { "type": "string", "description": "Background job id for status/output/kill" },
                "timeout": { "type": "integer", "description": "Timeout in milliseconds. Default 30000 for run; 0 or absent for start means no timeout.", "default": 30000 },
                "workdir": { "type": "string", "description": "Working directory for the command", "default": "." }
            },
            "required": []
        })
    }

    fn set_cancel(&self, cancel: Arc<AtomicBool>) {
        *self.cancel.lock().unwrap() = cancel;
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let action = input
            .args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("run");
        let command = input.args.get("command").and_then(|v| v.as_str());
        let workdir = input
            .args
            .get("workdir")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let shell = Self::shell();

        match action {
            "list" => {
                let mut lines: Vec<String> = {
                    let jobs = self.jobs.jobs.lock().unwrap();
                    if jobs.is_empty() {
                        return ToolOutput::ok("no background jobs");
                    }
                    jobs.values().map(|j| self.job_status_text(j)).collect()
                };
                lines.sort();
                ToolOutput::ok(lines.join("\n"))
            }
            "status" => {
                let job_id = match input.args.get("job_id").and_then(|v| v.as_str()) {
                    Some(id) => id.to_string(),
                    None => return ToolOutput::err("missing job_id for status"),
                };
                match self.job(&job_id) {
                    Ok(job) => ToolOutput::ok(self.job_status_text(&job)),
                    Err(e) => ToolOutput::err(e),
                }
            }
            "output" => {
                let job_id = match input.args.get("job_id").and_then(|v| v.as_str()) {
                    Some(id) => id.to_string(),
                    None => return ToolOutput::err("missing job_id for output"),
                };
                match self.job(&job_id) {
                    Ok(job) => {
                        let text = self.job_output(&job).await;
                        ToolOutput::ok(text)
                    }
                    Err(e) => ToolOutput::err(e),
                }
            }
            "kill" => {
                let job_id = match input.args.get("job_id").and_then(|v| v.as_str()) {
                    Some(id) => id.to_string(),
                    None => return ToolOutput::err("missing job_id for kill"),
                };
                match self.job(&job_id) {
                    Ok(job) => {
                        let text = self.kill_job(&job).await;
                        ToolOutput::ok(text)
                    }
                    Err(e) => ToolOutput::err(e),
                }
            }
            "start" => {
                let command = match command {
                    Some(c) => c.to_string(),
                    None => return ToolOutput::err("missing required argument: command"),
                };
                let timeout_ms = Self::timeout_ms(&input, true);
                match self
                    .start_background(&shell, &command, &workdir, timeout_ms)
                    .await
                {
                    Ok(id) => ToolOutput::ok(id),
                    Err(e) => ToolOutput::err(e),
                }
            }
            _ => {
                // "run" and anything unrecognized fall back to synchronous.
                let command = match command {
                    Some(c) => c.to_string(),
                    None => return ToolOutput::err("missing required argument: command"),
                };
                let timeout_ms = Self::timeout_ms(&input, false);
                let output = self
                    .run_foreground(&shell, &command, &workdir, timeout_ms)
                    .await;
                output.into_tool_output(timeout_ms)
            }
        }
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
    status: bool,
    timed_out: bool,
    cancelled: bool,
}

impl CommandOutput {
    fn failed(msg: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            stderr: msg.into(),
            exit_code: -1,
            status: false,
            timed_out: false,
            cancelled: false,
        }
    }

    fn timed_out(mut self) -> Self {
        self.timed_out = true;
        self.status = false;
        self
    }

    fn cancelled(mut self) -> Self {
        self.cancelled = true;
        self.status = false;
        self
    }

    fn into_tool_output(self, timeout_ms: Option<u64>) -> ToolOutput {
        let mut text = String::new();
        if !self.stdout.is_empty() {
            text.push_str(&self.stdout);
        }
        if !self.stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&self.stderr);
        }
        if text.len() > MAX_OUTPUT_BYTES {
            text.truncate(MAX_OUTPUT_BYTES);
            text.push_str("\n[output truncated]");
        }
        if self.timed_out {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "command timed out after {}ms (partial output above)",
                timeout_ms.unwrap_or(0)
            ));
        } else if self.cancelled {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str("command cancelled");
        }
        if !self.status && !self.timed_out && !self.cancelled {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!("exit code: {}", self.exit_code));
        }
        if self.status {
            ToolOutput::ok(text)
        } else {
            ToolOutput::err(text)
        }
    }
}

/// Spawn `shell -c command` with resource limits (512 MB virtual memory,
/// 60 s CPU, 200 MB file size) prepended, piped stdio, and — on Unix — the
/// child in its own process group so a timeout/cancel can kill the whole
/// tree, not just the direct shell.
fn spawn_child(shell: &str, command: &str, workdir: &str) -> std::io::Result<Child> {
    let guarded = format!("ulimit -v 524288 -t 60 -f 204800 2>/dev/null; {command}");
    let mut cmd = tokio::process::Command::new(shell);
    cmd.arg("-c")
        .arg(&guarded)
        .current_dir(workdir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.spawn()
}

/// Kill the whole process group of `pgid` (the direct child was spawned with
/// `process_group(0)`, so its pgid equals its pid). Falls back to killing
/// only the direct child on non-Unix.
fn kill_process_group(pgid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pgid {
        // Negative pid targets the process group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

/// Captured output buffers plus the reader tasks draining the pipes.
type PumpSet = (
    Arc<TokioMutex<Vec<u8>>>,
    Arc<TokioMutex<Vec<u8>>>,
    JoinHandle<()>,
    JoinHandle<()>,
);

/// Spawn two reader tasks that drain the child's stdout/stderr pipes into
/// bounded buffers, returning the buffers and the task handles.
fn start_pumps(child: &mut Child) -> PumpSet {
    let stdout_pipe = child.stdout.take().expect("stdout is piped");
    let stderr_pipe = child.stderr.take().expect("stderr is piped");
    let out_buf = Arc::new(TokioMutex::new(Vec::new()));
    let err_buf = Arc::new(TokioMutex::new(Vec::new()));
    let out_h = tokio::spawn(pump(stdout_pipe, out_buf.clone()));
    let err_h = tokio::spawn(pump(stderr_pipe, err_buf.clone()));
    (out_buf, err_buf, out_h, err_h)
}

async fn pump(mut pipe: impl tokio::io::AsyncRead + Unpin, buf: Arc<TokioMutex<Vec<u8>>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut b = buf.lock().await;
                if b.len() < MAX_OUTPUT_BYTES {
                    let room = MAX_OUTPUT_BYTES - b.len();
                    b.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "bash".into(),
            args: args
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn tool() -> BashTool {
        BashTool::new()
    }

    async fn wait_status(tool: &BashTool, id: &str, expect: &str) -> String {
        for _ in 0..50 {
            let out = tool
                .execute(input(
                    serde_json::json!({ "action": "status", "job_id": id }),
                ))
                .await;
            if out.data.contains(expect) {
                return out.data;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("job {id} never reached '{expect}'");
    }

    #[tokio::test]
    async fn missing_command_returns_error() {
        let out = tool().execute(input(serde_json::json!({}))).await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: command"));
    }

    #[tokio::test]
    async fn echo_returns_stdout() {
        let out = tool()
            .execute(input(serde_json::json!({ "command": "echo hello" })))
            .await;
        assert!(out.success, "{}", out.data);
        assert!(out.data.trim() == "hello");
    }

    #[tokio::test]
    async fn failing_command_returns_error() {
        let out = tool()
            .execute(input(serde_json::json!({ "command": "exit 1" })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("exit code: 1"));
    }

    #[tokio::test]
    async fn timeout_is_respected() {
        let out = tool()
            .execute(input(serde_json::json!({
                "command": "sleep 10",
                "timeout": 100
            })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("timed out"));
    }

    #[tokio::test]
    async fn timeout_returns_partial_output() {
        let out = tool()
            .execute(input(serde_json::json!({
                "command": "echo start; sleep 10; echo end",
                "timeout": 300
            })))
            .await;
        assert!(!out.success, "{}", out.data);
        assert!(
            out.data.contains("start"),
            "partial output missing: {}",
            out.data
        );
        assert!(out.data.contains("timed out"), "{}", out.data);
    }

    #[tokio::test]
    async fn background_job_start_status_output() {
        let t = tool();
        let start = t
            .execute(input(serde_json::json!({
                "action": "start",
                "command": "echo hi; sleep 1"
            })))
            .await;
        assert!(start.success, "{}", start.data);
        let id = start.data.trim().to_string();
        assert!(id.starts_with("job-"), "{id}");

        let s = wait_status(&t, &id, "exited").await;
        assert!(s.contains("exited (code 0)"), "{s}");

        let out = t
            .execute(input(
                serde_json::json!({ "action": "output", "job_id": id }),
            ))
            .await;
        assert!(out.success, "{}", out.data);
        assert!(out.data.contains("hi"), "{}", out.data);
        assert!(out.data.contains("status: exited (code 0)"), "{}", out.data);
    }

    #[tokio::test]
    async fn background_job_kill() {
        let t = tool();
        let start = t
            .execute(input(serde_json::json!({
                "action": "start",
                "command": "sleep 100"
            })))
            .await;
        assert!(start.success, "{}", start.data);
        let id = start.data.trim().to_string();

        let s = wait_status(&t, &id, "running").await;
        assert!(s.contains("running"), "{s}");

        let k = t
            .execute(input(serde_json::json!({ "action": "kill", "job_id": id })))
            .await;
        assert!(k.success, "{}", k.data);
        assert!(k.data.contains("killed"), "{}", k.data);

        let s = t
            .execute(input(
                serde_json::json!({ "action": "status", "job_id": id }),
            ))
            .await;
        assert!(
            s.data.contains("exited") || s.data.contains("killed"),
            "{}",
            s.data
        );
    }

    #[tokio::test]
    async fn background_job_timeout() {
        let t = tool();
        let start = t
            .execute(input(serde_json::json!({
                "action": "start",
                "command": "sleep 100",
                "timeout": 200
            })))
            .await;
        assert!(start.success, "{}", start.data);
        let id = start.data.trim().to_string();

        let s = wait_status(&t, &id, "timed out").await;
        assert!(s.contains("timed out"), "{s}");
    }

    #[tokio::test]
    async fn background_requires_command() {
        let out = tool()
            .execute(input(serde_json::json!({ "action": "start" })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: command"));
    }

    #[tokio::test]
    async fn unknown_job_is_an_error() {
        let out = tool()
            .execute(input(
                serde_json::json!({ "action": "status", "job_id": "job-999" }),
            ))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("unknown job"));
    }

    #[tokio::test]
    async fn list_shows_background_jobs() {
        let t = tool();
        let empty = t
            .execute(input(serde_json::json!({ "action": "list" })))
            .await;
        assert!(empty.success && empty.data.contains("no background jobs"));

        let start = t
            .execute(input(serde_json::json!({
                "action": "start",
                "command": "sleep 5"
            })))
            .await;
        assert!(start.success, "{}", start.data);
        let list = t
            .execute(input(serde_json::json!({ "action": "list" })))
            .await;
        assert!(list.success, "{}", list.data);
        assert!(list.data.contains("job-"), "{}", list.data);
        assert!(list.data.contains("running"), "{}", list.data);
    }
}
