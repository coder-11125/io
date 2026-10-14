use super::edit::apply_edit;
use super::file_state::FileTracker;
use super::write::make_diff;
use super::{Tool, ToolInput, ToolOutput};
use std::sync::Arc;

/// Several `edit`s to one file, applied in order and all-or-nothing: either
/// every edit lands in a single write, or the file is left untouched.
pub struct MultiEditTool {
    tracker: Arc<FileTracker>,
}

impl MultiEditTool {
    pub fn new(tracker: Arc<FileTracker>) -> Self {
        Self { tracker }
    }
}

impl Default for MultiEditTool {
    fn default() -> Self {
        Self::new(FileTracker::shared())
    }
}

#[async_trait::async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "multi_edit"
    }

    fn description(&self) -> &str {
        "Make several edits to one file in a single call. Edits are applied in order, each to \
         the result of the previous one, and either all succeed or the file is left unchanged. \
         Each edit has `old_string`, `new_string` and optional `replace_all` (default: first \
         occurrence only). The file must have been read first. Returns a unified diff."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit" },
                "edits": {
                    "type": "array",
                    "description": "Edits to apply in order",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string":  { "type": "string", "description": "Exact text to find" },
                            "new_string":  { "type": "string", "description": "Text to replace it with" },
                            "replace_all": { "type": "boolean", "description": "Replace every occurrence", "default": false }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let Some(path_str) = input.args.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::err("missing required argument: path");
        };
        let path = match super::resolve_safe_path(path_str) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let Some(edits) = input.args.get("edits").and_then(|v| v.as_array()) else {
            return ToolOutput::err("missing required argument: edits (an array)");
        };
        if edits.is_empty() {
            return ToolOutput::err("edits must contain at least one edit");
        }

        let old_content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("failed to read {}: {e}", path.display())),
        };
        if let Err(e) = self.tracker.check(&path, &old_content) {
            return ToolOutput::err(e);
        }

        let mut content = old_content.clone();
        for (i, edit) in edits.iter().enumerate() {
            let n = i + 1;
            let Some(old) = edit.get("old_string").and_then(|v| v.as_str()) else {
                return ToolOutput::err(format!("edit {n}: missing old_string"));
            };
            let Some(new) = edit.get("new_string").and_then(|v| v.as_str()) else {
                return ToolOutput::err(format!("edit {n}: missing new_string"));
            };
            let replace_all = edit
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            content = match apply_edit(&content, old, new, replace_all) {
                Ok(c) => c,
                Err(e) => {
                    return ToolOutput::err(format!(
                        "edit {n} of {} failed: {e} — no changes were made to {}",
                        edits.len(),
                        path.display()
                    ))
                }
            };
        }

        if let Err(e) = std::fs::write(&path, &content) {
            return ToolOutput::err(format!("failed to write {}: {e}", path.display()));
        }
        self.tracker.record(&path, &content);

        let path = path.to_string_lossy().into_owned();
        ToolOutput::ok(make_diff(&old_content, &content, &path))
            .with_snapshot(path, Some(old_content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(tool: &MultiEditTool, args: serde_json::Value) -> ToolOutput {
        let args = args
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        futures::executor::block_on(tool.execute(ToolInput {
            name: "multi_edit".into(),
            args,
        }))
    }

    fn temp_file(content: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("io_multi_edit_{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&p, content).unwrap();
        p
    }

    fn tool_that_read(path: &std::path::Path) -> MultiEditTool {
        let tracker = FileTracker::shared();
        tracker.record(path, &std::fs::read_to_string(path).unwrap());
        MultiEditTool::new(tracker)
    }

    #[test]
    fn applies_edits_in_order_each_seeing_the_previous_result() {
        let path = temp_file("alpha beta\n");
        let out = run(
            &tool_that_read(&path),
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "edits": [
                    {"old_string": "alpha", "new_string": "gamma"},
                    // Only matches because the first edit already ran.
                    {"old_string": "gamma beta", "new_string": "done"}
                ]
            }),
        );
        let after = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert_eq!(after, "done\n");
        assert!(out.data.contains("-alpha beta") && out.data.contains("+done"));
        let snap = out.pre_edit_snapshot.expect("snapshot");
        assert_eq!(snap.prior_content.as_deref(), Some("alpha beta\n"));
    }

    #[test]
    fn a_failing_edit_leaves_the_file_untouched() {
        let path = temp_file("one two three\n");
        let out = run(
            &tool_that_read(&path),
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "edits": [
                    {"old_string": "one", "new_string": "1"},
                    {"old_string": "MISSING", "new_string": "x"}
                ]
            }),
        );
        let after = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("edit 2 of 2 failed"), "{}", out.data);
        assert!(out.data.contains("no changes were made"));
        assert_eq!(after, "one two three\n");
    }

    #[test]
    fn replace_all_applies_per_edit() {
        let path = temp_file("a a a\nb b\n");
        let out = run(
            &tool_that_read(&path),
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "edits": [
                    {"old_string": "a", "new_string": "x", "replace_all": true},
                    {"old_string": "b", "new_string": "y"}
                ]
            }),
        );
        let after = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert_eq!(after, "x x x\ny b\n");
    }

    #[test]
    fn validates_arguments() {
        let tool = MultiEditTool::default();
        assert!(run(&tool, serde_json::json!({"edits": []}))
            .data
            .contains("missing required argument: path"));
        assert!(run(&tool, serde_json::json!({"path": "/tmp/x"}))
            .data
            .contains("missing required argument: edits"));
        assert!(
            run(&tool, serde_json::json!({"path": "/tmp/x", "edits": []}))
                .data
                .contains("at least one edit")
        );

        let path = temp_file("x\n");
        let out = run(
            &tool_that_read(&path),
            serde_json::json!({"path": path.to_str().unwrap(), "edits": [{"old_string": "x"}]}),
        );
        std::fs::remove_file(&path).ok();
        assert!(out.data.contains("edit 1: missing new_string"));
    }

    #[test]
    fn enforces_read_before_edit_and_change_detection() {
        let path = temp_file("hello\n");
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "edits": [{"old_string": "hello", "new_string": "bye"}]
        });

        let unread = run(&MultiEditTool::default(), args.clone());
        assert!(
            unread.data.contains("has not been read yet"),
            "{}",
            unread.data
        );

        let tool = tool_that_read(&path);
        std::fs::write(&path, "hello!\n").unwrap();
        let stale = run(&tool, args);
        let after = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(stale.data.contains("modified since"), "{}", stale.data);
        assert_eq!(after, "hello!\n");
    }
}
