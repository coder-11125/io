use super::file_state::FileTracker;
use super::{Tool, ToolInput, ToolOutput};
use similar::{ChangeTag, TextDiff};
use std::sync::Arc;

pub struct WriteTool {
    tracker: Arc<FileTracker>,
}

impl WriteTool {
    pub fn new(tracker: Arc<FileTracker>) -> Self {
        Self { tracker }
    }
}

impl Default for WriteTool {
    fn default() -> Self {
        Self::new(FileTracker::shared())
    }
}

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating it or overwriting it. An existing file must have been \
         read first (and not changed since); a new file needs no read. Prefer edit/multi_edit \
         for changes to existing files. Returns a unified diff of what changed."
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
        let resolved = match super::resolve_safe_path(path_str) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let path = resolved.to_string_lossy().into_owned();
        let content = match input.args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err("missing required argument: content"),
        };

        let is_new = !resolved.exists();
        let old_content = if is_new {
            String::new()
        } else {
            match std::fs::read_to_string(&resolved) {
                Ok(c) => c,
                Err(e) => return ToolOutput::err(format!("failed to read {path}: {e}")),
            }
        };
        // Overwriting needs the model to have seen the file as it is now.
        if !is_new {
            if let Err(e) = self.tracker.check(&resolved, &old_content) {
                return ToolOutput::err(e);
            }
        }

        if let Some(parent) = std::path::Path::new(&path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutput::err(format!("failed to create parent directories: {e}"));
            }
        }

        if let Err(e) = std::fs::write(&path, &content) {
            return ToolOutput::err(format!("failed to write {path}: {e}"));
        }
        self.tracker.record(&resolved, &content);

        let prior_content = if is_new {
            None
        } else {
            Some(old_content.clone())
        };

        if is_new {
            // Return a synthetic "all additions" diff
            let mut out = format!(
                "--- /dev/null\n+++ {path}\n@@ -0,0 +1,{} @@\n",
                content.lines().count()
            );
            for line in content.lines() {
                out.push('+');
                out.push_str(line);
                out.push('\n');
            }
            return ToolOutput::ok(out).with_snapshot(path, prior_content);
        }

        ToolOutput::ok(make_diff(&old_content, &content, &path)).with_snapshot(path, prior_content)
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
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            a0 + 1,
            a1 - a0,
            b0 + 1,
            b1 - b0
        ));
        for op in group {
            for change in diff.iter_changes(op) {
                let prefix = match change.tag() {
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                    ChangeTag::Equal => ' ',
                };
                out.push(prefix);
                out.push_str(change.value());
                if !change.value().ends_with('\n') {
                    out.push('\n');
                }
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
            args: args
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("io_write_test_{}_{}", uuid::Uuid::new_v4(), name))
    }

    /// A write tool that has already "read" `path` as it currently is.
    fn tool_that_read(path: &std::path::Path) -> WriteTool {
        let tracker = FileTracker::shared();
        tracker.record(path, &std::fs::read_to_string(path).unwrap());
        WriteTool::new(tracker)
    }

    #[tokio::test]
    async fn missing_path_returns_error() {
        let out = WriteTool::default()
            .execute(input(serde_json::json!({ "content": "hi" })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: path"));
    }

    #[tokio::test]
    async fn missing_content_returns_error() {
        let out = WriteTool::default()
            .execute(input(serde_json::json!({ "path": "/tmp/x" })))
            .await;
        assert!(!out.success);
        assert!(out.data.contains("missing required argument: content"));
    }

    #[tokio::test]
    async fn creates_new_file() {
        let path = temp_path("new.txt");
        let out = WriteTool::default()
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "hello world"
            })))
            .await;
        let written = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert_eq!(written.as_deref(), Some("hello world"));
        assert!(out.data.contains("+hello world"));
        let snapshot = out.pre_edit_snapshot.expect("snapshot should be captured");
        assert_eq!(
            snapshot.prior_content, None,
            "new file should snapshot as nonexistent"
        );
    }

    #[tokio::test]
    async fn overwrites_existing_file_and_diffs() {
        let path = temp_path("overwrite.txt");
        std::fs::write(&path, "old content\n").unwrap();
        let out = tool_that_read(&path)
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "new content\n"
            })))
            .await;
        let written = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert_eq!(written.as_deref(), Some("new content\n"));
        assert!(out.data.contains("-old content"));
        assert!(out.data.contains("+new content"));
        let snapshot = out.pre_edit_snapshot.expect("snapshot should be captured");
        assert_eq!(snapshot.prior_content.as_deref(), Some("old content\n"));
    }

    #[tokio::test]
    async fn oversized_prior_content_is_not_snapshotted() {
        let path = temp_path("huge.txt");
        std::fs::write(&path, "x".repeat(super::super::MAX_SNAPSHOT_BYTES + 1)).unwrap();
        let out = tool_that_read(&path)
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "new content\n"
            })))
            .await;
        std::fs::remove_file(&path).ok();
        assert!(out.success, "{}", out.data);
        assert!(
            out.pre_edit_snapshot.is_none(),
            "oversized prior content should be skipped"
        );
    }

    #[tokio::test]
    async fn refuses_to_overwrite_an_existing_file_that_was_never_read() {
        let path = temp_path("unread.txt");
        std::fs::write(&path, "precious\n").unwrap();
        let out = WriteTool::default()
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "clobbered"
            })))
            .await;
        let after = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("has not been read yet"), "{}", out.data);
        assert_eq!(after.as_deref(), Some("precious\n"));
    }

    #[tokio::test]
    async fn refuses_to_overwrite_a_file_modified_since_it_was_read() {
        let path = temp_path("stale.txt");
        std::fs::write(&path, "v1\n").unwrap();
        let tool = tool_that_read(&path);
        std::fs::write(&path, "v2 by the user\n").unwrap();
        let out = tool
            .execute(input(serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "v3"
            })))
            .await;
        let after = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert!(!out.success);
        assert!(out.data.contains("modified since"), "{}", out.data);
        assert_eq!(after.as_deref(), Some("v2 by the user\n"));
    }

    #[tokio::test]
    async fn a_file_the_tool_just_created_can_be_rewritten_without_a_read() {
        let path = temp_path("created.txt");
        let tool = WriteTool::default();
        for content in ["first", "second"] {
            let out = tool
                .execute(input(serde_json::json!({
                    "path": path.to_str().unwrap(),
                    "content": content
                })))
                .await;
            assert!(out.success, "{}", out.data);
        }
        let after = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok();
        assert_eq!(after.as_deref(), Some("second"));
    }
}
