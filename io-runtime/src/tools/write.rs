use super::{Tool, ToolInput, ToolOutput};
use similar::{ChangeTag, TextDiff};

pub struct WriteTool;

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str { "write" }

    fn description(&self) -> &str {
        "Write content to a file, creating it or overwriting it. Returns a unified diff of what changed."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path":    { "type": "string", "description": "Path to the file to write" },
                "content": { "type": "string", "description": "Full content to write to the file" }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let path_str = match input.args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::err("missing required argument: path"),
        };
        let path = match super::resolve_safe_path(path_str) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let path = path.to_string_lossy().into_owned();
        let content = match input.args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err("missing required argument: content"),
        };

        let old_content = std::fs::read_to_string(&path).unwrap_or_default();
        let is_new = !std::path::Path::new(&path).exists();

        if let Some(parent) = std::path::Path::new(&path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutput::err(format!("failed to create parent directories: {e}"));
            }
        }

        if let Err(e) = std::fs::write(&path, &content) {
            return ToolOutput::err(format!("failed to write {path}: {e}"));
        }

        if is_new {
            // Return a synthetic "all additions" diff
            let mut out = format!("--- /dev/null\n+++ {path}\n@@ -0,0 +1,{} @@\n", content.lines().count());
            for line in content.lines() {
                out.push('+');
                out.push_str(line);
                out.push('\n');
            }
            return ToolOutput::ok(out);
        }

        ToolOutput::ok(make_diff(&old_content, &content, &path))
    }
}

pub fn make_diff(old: &str, new: &str, path: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = format!("--- {path}\n+++ {path}\n");
    for group in diff.grouped_ops(3).iter() {
        let a0 = group[0].old_range().start;
        let a1 = group.last().unwrap().old_range().end;
        let b0 = group[0].new_range().start;
        let b1 = group.last().unwrap().new_range().end;
        out.push_str(&format!("@@ -{},{} +{},{} @@\n", a0 + 1, a1 - a0, b0 + 1, b1 - b0));
        for op in group {
            for change in diff.iter_changes(op) {
                let prefix = match change.tag() {
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                    ChangeTag::Equal  => ' ',
                };
                out.push(prefix);
                out.push_str(change.value());
                if !change.value().ends_with('\n') { out.push('\n'); }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "write".into(),
            args: args.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("io_write_test_{}_{}", uuid::Uuid::new_v4(), name))
    }

    #[tokio::test]
    async fn missing_path_returns_error() {
        let out = WriteTool.execute(input(serde_json::json!({ "content": "hi" }))).await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: path"));
    }

    #[tokio::test]
    async fn missing_content_returns_error() {
        let out = WriteTool.execute(input(serde_json::json!({ "path": "/tmp/x" }))).await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: content"));
    }

    #[tokio::test]
    async fn creates_new_file() {
        let path = temp_path("new.txt");
        let out = WriteTool.execute(input(serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "hello world"
        }))).await;
        let written = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert_eq!(written.as_deref(), Some("hello world"));
        assert!(out.data.contains("+hello world"));
    }

    #[tokio::test]
    async fn overwrites_existing_file_and_diffs() {
        let path = temp_path("overwrite.txt");
        std::fs::write(&path, "old content\n").unwrap();
        let out = WriteTool.execute(input(serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "new content\n"
        }))).await;
        let written = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert_eq!(written.as_deref(), Some("new content\n"));
        assert!(out.data.contains("-old content"));
        assert!(out.data.contains("+new content"));
    }
}
