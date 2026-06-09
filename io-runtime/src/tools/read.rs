use super::{Tool, ToolInput, ToolOutput};

pub struct ReadTool;

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str { "read" }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports optional offset and limit for reading partial files."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file to read" },
                "offset": { "type": "integer", "description": "Line number to start reading from (0-indexed)", "default": 0 },
                "limit": { "type": "integer", "description": "Maximum number of lines to read", "default": 2000 }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let file_path = match input.args.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::err("missing required argument: file_path"),
        };

        let resolved = match super::resolve_safe_path(file_path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };

        let offset = input.args.get("offset").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
        let limit = input.args.get("limit").and_then(|v| v.as_i64()).unwrap_or(2000) as usize;

        match std::fs::read_to_string(&resolved) {
            Ok(contents) => {
                let lines: Vec<&str> = contents.lines().collect();
                let total = lines.len();

                if offset >= total {
                    return ToolOutput::ok(format!("[offset {offset} beyond file length {total}]"));
                }

                let end = (offset + limit).min(total);
                let excerpt: Vec<String> = lines[offset..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{i}:{line}"))
                    .collect();

                let mut result = excerpt.join("\n");
                if end < total {
                    result.push_str(&format!("\n... ({}/{}) lines shown", end - offset, total));
                } else {
                    result.push_str(&format!("\n--- {total} total lines ---"));
                }

                ToolOutput::ok(result)
            }
            Err(e) => ToolOutput::err(format!("failed to read {}: {e}", resolved.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "read".into(),
            args: args.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("io_read_test_{}_{}", uuid::Uuid::new_v4(), name));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn missing_file_path_returns_error() {
        let out = ReadTool.execute(input(serde_json::json!({}))).await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: file_path"));
    }

    #[tokio::test]
    async fn nonexistent_file_returns_error() {
        let out = ReadTool.execute(input(serde_json::json!({
            "file_path": "/tmp/io_test_nonexistent_file_xyz_123"
        }))).await;
        assert!(!out.success);
    }

    #[tokio::test]
    async fn reads_file_content() {
        let path = write_temp("basic.txt", "hello\nworld\n");
        let out = ReadTool.execute(input(serde_json::json!({
            "file_path": path.to_str().unwrap()
        }))).await;
        std::fs::remove_file(&path).ok();
        assert!(out.success);
        assert!(out.data.contains("hello"));
        assert!(out.data.contains("world"));
    }

    #[tokio::test]
    async fn offset_skips_lines() {
        let path = write_temp("offset.txt", "line1\nline2\nline3\n");
        let out = ReadTool.execute(input(serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "offset": 1
        }))).await;
        std::fs::remove_file(&path).ok();
        assert!(out.success);
        assert!(!out.data.contains("line1"));
        assert!(out.data.contains("line2"));
    }

    #[tokio::test]
    async fn limit_caps_output() {
        let content = (1..=10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let path = write_temp("limit.txt", &content);
        let out = ReadTool.execute(input(serde_json::json!({
            "file_path": path.to_str().unwrap(),
            "limit": 3
        }))).await;
        std::fs::remove_file(&path).ok();
        assert!(out.success);
        assert!(out.data.contains("line1"));
        assert!(!out.data.contains("line4"));
    }
}
