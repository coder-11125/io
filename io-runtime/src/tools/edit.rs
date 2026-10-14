use super::file_state::FileTracker;
use super::write::make_diff;
use super::{Tool, ToolInput, ToolOutput};
use std::sync::Arc;

/// Replace `old` with `new` in `content`: the first occurrence, or every one
/// with `replace_all`. Shared by `edit` and `multi_edit`.
pub(crate) fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, String> {
    if old.is_empty() {
        return Err("old_string must not be empty".to_string());
    }
    if old == new {
        return Err("old_string and new_string are identical — nothing to change".to_string());
    }
    if !content.contains(old) {
        return Err("old_string not found".to_string());
    }
    Ok(if replace_all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    })
}

pub struct EditTool {
    tracker: Arc<FileTracker>,
}

impl EditTool {
    pub fn new(tracker: Arc<FileTracker>) -> Self {
        Self { tracker }
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new(FileTracker::shared())
    }
}

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace the first occurrence of `old_string` with `new_string` in a file (every \
         occurrence with `replace_all`). The file must have been read first, and the edit fails \
         if the file changed on disk since — read it again. Use multi_edit for several changes \
         to one file. Returns a unified diff of what changed."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path":       { "type": "string", "description": "Path to the file to edit" },
                "old_string": { "type": "string", "description": "Exact text to find and replace" },
                "new_string": { "type": "string", "description": "Text to replace it with" },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of the first", "default": false }
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
            Ok(p) => p,
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
        let replace_all = input
            .args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let old_content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("failed to read {}: {e}", path.display())),
        };
        if let Err(e) = self.tracker.check(&path, &old_content) {
            return ToolOutput::err(e);
        }

        let new_content = match apply_edit(&old_content, &old_str, &new_str, replace_all) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("{e} in {}", path.display())),
        };

        if let Err(e) = std::fs::write(&path, &new_content) {
            return ToolOutput::err(format!("failed to write {}: {e}", path.display()));
        }
        self.tracker.record(&path, &new_content);

        let path = path.to_string_lossy().into_owned();
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

    /// An edit tool that has already "read" `path` as it currently is.
    fn tool_that_read(path: &std::path::Path) -> EditTool {
        let tracker = FileTracker::shared();
        tracker.record(path, &std::fs::read_to_string(path).unwrap());
        EditTool::new(tracker)
    }

    #[tokio::test]
    async fn missing_path_returns_error() {
        let out = EditTool::default()
            .execute(input(serde_json::json!({
                "old_string": "a", "new_string": "b"
            })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: path"));
    }

    #[tokio::test]
    async fn nonexistent_file_returns_error() {
        let out = EditTool::default()
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
        let out = tool_that_read(&path)
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
        let out = tool_that_read(&path)
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

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let path = write_temp("all.txt", "foo bar foo\n");
        let out = tool_that_read(&path)
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "foo",
                "new_string": "baz",
                "replace_all": true
            })))
            .await;
        let result = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert_eq!(result.as_deref(), Some("baz bar baz\n"));
    }

    #[tokio::test]
    async fn refuses_to_edit_a_file_that_was_never_read() {
        let path = write_temp("unread.txt", "hello\n");
        let out = EditTool::default()
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "hello",
                "new_string": "bye"
            })))
            .await;
        let result = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("has not been read yet"), "{}", out.data);
        assert_eq!(result.as_deref(), Some("hello\n"), "file must be untouched");
    }

    #[tokio::test]
    async fn refuses_when_the_file_changed_since_it_was_read() {
        let path = write_temp("stale.txt", "hello\n");
        let tool = tool_that_read(&path);
        std::fs::write(&path, "hello, edited elsewhere\n").unwrap();
        let out = tool
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "hello",
                "new_string": "bye"
            })))
            .await;
        let result = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("modified since"), "{}", out.data);
        assert_eq!(result.as_deref(), Some("hello, edited elsewhere\n"));
    }

    #[tokio::test]
    async fn consecutive_edits_do_not_need_a_re_read() {
        let path = write_temp("chain.txt", "a b c\n");
        let tool = tool_that_read(&path);
        for (old, new) in [("a", "x"), ("b", "y")] {
            let out = tool
                .execute(input(serde_json::json!({
                    "path": path.to_str().unwrap(),
                    "old_string": old,
                    "new_string": new
                })))
                .await;
            assert!(out.success, "{}", out.data);
        }
        let result = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert_eq!(result.as_deref(), Some("x y c\n"));
    }

    #[test]
    fn apply_edit_validates_its_arguments() {
        assert!(apply_edit("abc", "", "x", false)
            .unwrap_err()
            .contains("must not be empty"));
        assert!(apply_edit("abc", "a", "a", false)
            .unwrap_err()
            .contains("identical"));
        assert!(apply_edit("abc", "z", "x", false)
            .unwrap_err()
            .contains("not found"));
        assert_eq!(apply_edit("aaa", "a", "b", false).unwrap(), "baa");
        assert_eq!(apply_edit("aaa", "a", "b", true).unwrap(), "bbb");
    }
}
