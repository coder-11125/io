use super::write::make_diff;
use super::{Tool, ToolInput, ToolOutput};

pub struct EditTool;

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace the first occurrence of `old_string` with `new_string` in a file. \
         Returns a unified diff of what changed."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path":       { "type": "string", "description": "Path to the file to edit" },
                "old_string": { "type": "string", "description": "Exact text to find and replace" },
                "new_string": { "type": "string", "description": "Text to replace it with" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let path_str = match input.args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolOutput::err("missing required argument: path"),
        };
        let path = match super::resolve_safe_path(path_str) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => return ToolOutput::err(e),
        };
        let old_str = match input.args.get("old_string").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::err("missing required argument: old_string"),
        };
        let new_str = match input.args.get("new_string").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::err("missing required argument: new_string"),
        };

        let old_content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("failed to read {path}: {e}")),
        };

        if !old_content.contains(&old_str) {
            return ToolOutput::err(format!("old_string not found in {path}"));
        }

        let new_content = old_content.replacen(&old_str, &new_str, 1);

        if let Err(e) = std::fs::write(&path, &new_content) {
            return ToolOutput::err(format!("failed to write {path}: {e}"));
        }

        ToolOutput::ok(make_diff(&old_content, &new_content, &path))
            .with_snapshot(path, Some(old_content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "edit".into(),
            args: args
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("io_edit_test_{}_{}", uuid::Uuid::new_v4(), name));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn missing_path_returns_error() {
        let out = EditTool
            .execute(input(serde_json::json!({
                "old_string": "a", "new_string": "b"
            })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: path"));
    }

    #[tokio::test]
    async fn nonexistent_file_returns_error() {
        let out = EditTool
            .execute(input(serde_json::json!({
                "path": "/tmp/io_edit_nonexistent_xyz",
                "old_string": "a",
                "new_string": "b"
            })))
            .await;
        assert!(!out.success);
    }

    #[tokio::test]
    async fn old_string_not_found_returns_error() {
        let path = write_temp("notfound.txt", "hello world\n");
        let out = EditTool
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "MISSING",
                "new_string": "replacement"
            })))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("not found"));
    }

    #[tokio::test]
    async fn replaces_first_occurrence() {
        let path = write_temp("replace.txt", "foo bar foo\n");
        let out = EditTool
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "foo",
                "new_string": "baz"
            })))
            .await;
        let result = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert_eq!(result.as_deref(), Some("baz bar foo\n"));
        let snapshot = out.pre_edit_snapshot.expect("snapshot should be captured");
        assert_eq!(snapshot.prior_content.as_deref(), Some("foo bar foo\n"));
    }
}
