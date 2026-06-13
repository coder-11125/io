use super::{Tool, ToolInput, ToolOutput};

const MAX_OUTPUT_BYTES: usize = 100_000;

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

pub struct BashTool;

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command. Returns stdout, stderr, and exit code."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute" },
                "timeout": { "type": "integer", "description": "Timeout in milliseconds", "default": 30000 },
                "workdir": { "type": "string", "description": "Working directory for the command", "default": "." }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let command = match input.args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err("missing required argument: command"),
        };

        let timeout_ms = input
            .args
            .get("timeout")
            .and_then(|v| v.as_i64())
            .unwrap_or(30_000) as u64;
        let workdir = input
            .args
            .get("workdir")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| ALLOWED_SHELLS.contains(&s.as_str()))
            .unwrap_or_else(|| "/bin/sh".to_string());

        // Prepend resource limits: 512 MB virtual memory, 60 s CPU, 200 MB file size.
        // Applied at execution time so the sandbox analysis sees the original command.
        let guarded = format!("ulimit -v 524288 -t 60 -f 204800 2>/dev/null; {command}");
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            execute_command(&shell, "-c", &guarded, &workdir),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut text = String::new();
                if !output.stdout.is_empty() {
                    text.push_str(&output.stdout);
                }
                if !output.stderr.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&output.stderr);
                }
                if text.len() > MAX_OUTPUT_BYTES {
                    text.truncate(MAX_OUTPUT_BYTES);
                    text.push_str("\n[output truncated]");
                }
                if !output.status {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("exit code: {}", output.exit_code));
                }
                if output.status {
                    ToolOutput::ok(text)
                } else {
                    ToolOutput::err(text)
                }
            }
            Ok(Err(e)) => ToolOutput::err(format!("command execution error: {e}")),
            Err(_) => ToolOutput::err(format!("command timed out after {timeout_ms}ms")),
        }
    }
}

struct CommandOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
    status: bool,
}

async fn execute_command(
    shell: &str,
    flag: &str,
    command: &str,
    workdir: &str,
) -> std::io::Result<CommandOutput> {
    let output = tokio::process::Command::new(shell)
        .arg(flag)
        .arg(command)
        .current_dir(workdir)
        .output()
        .await?;
    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
        status: output.status.success(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn missing_command_returns_error() {
        let out = BashTool.execute(input(serde_json::json!({}))).await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: command"));
    }

    #[tokio::test]
    async fn echo_returns_stdout() {
        let out = BashTool
            .execute(input(serde_json::json!({ "command": "echo hello" })))
            .await;
        assert!(out.success, "{}", out.data);
        assert!(out.data.trim() == "hello");
    }

    #[tokio::test]
    async fn failing_command_returns_error() {
        let out = BashTool
            .execute(input(serde_json::json!({ "command": "exit 1" })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("exit code: 1"));
    }

    #[tokio::test]
    async fn timeout_is_respected() {
        let out = BashTool
            .execute(input(serde_json::json!({
                "command": "sleep 10",
                "timeout": 100
            })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("timed out"));
    }
}
