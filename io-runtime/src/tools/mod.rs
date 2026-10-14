use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub mod bash;
pub mod edit;
pub mod fetch;
pub mod file_state;
pub mod glob;
pub mod grep;
pub mod html;
pub mod multi_edit;
pub mod notebook;
pub mod plan;
pub mod read;
pub mod spawn;
pub mod todo;
pub mod web_search;
pub mod write;

pub use bash::BashTool;
pub use edit::EditTool;
pub use fetch::FetchTool;
pub use file_state::FileTracker;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use multi_edit::MultiEditTool;
pub use notebook::NotebookEditTool;
pub use plan::ExitPlanModeTool;
pub use read::ReadTool;
pub use spawn::SpawnAgentTool;
pub use todo::TodoWriteTool;
pub use web_search::WebSearchTool;
pub use write::WriteTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInput {
    pub name: String,
    pub args: HashMap<String, serde_json::Value>,
}

/// A file's content immediately before a `write`/`edit` tool call changed it,
/// captured so a later rewind can restore it. `prior_content: None` means the
/// file did not exist before the call (rewind should delete it, not write it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub prior_content: Option<String>,
}

/// Files larger than this are not snapshotted — the write/edit still
/// succeeds, it just won't be revertible by a later rewind, to keep the
/// session's persisted JSON blob from growing unbounded.
pub const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

/// An image a tool returned for the model to look at (base64 `data`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolImage {
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub success: bool,
    pub data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_edit_snapshot: Option<FileSnapshot>,
    /// Images to show the model alongside the text result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ToolImage>,
}

impl ToolOutput {
    pub fn ok(data: impl Into<String>) -> Self {
        Self {
            success: true,
            data: data.into(),
            error: None,
            pre_edit_snapshot: None,
            images: Vec::new(),
        }
    }

    pub fn err(error: impl Into<String>) -> Self {
        let msg = error.into();
        Self {
            success: false,
            data: msg.clone(),
            error: Some(msg),
            pre_edit_snapshot: None,
            images: Vec::new(),
        }
    }

    /// Attach an image for the model to see (e.g. a screenshot read from disk).
    pub fn with_image(mut self, media_type: impl Into<String>, data: impl Into<String>) -> Self {
        self.images.push(ToolImage {
            media_type: media_type.into(),
            data: data.into(),
        });
        self
    }

    /// Attach a pre-edit file snapshot for later rewind, if it wasn't skipped
    /// for exceeding `MAX_SNAPSHOT_BYTES`.
    pub fn with_snapshot(mut self, path: String, prior_content: Option<String>) -> Self {
        let too_large = prior_content
            .as_ref()
            .map(|c| c.len() > MAX_SNAPSHOT_BYTES)
            .unwrap_or(false);
        if !too_large {
            self.pre_edit_snapshot = Some(FileSnapshot {
                path,
                prior_content,
            });
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, input: ToolInput) -> ToolOutput;
    /// Propagate a cancellation flag to tools that support it (e.g. spawn_agent).
    fn set_cancel(&self, _cancel: Arc<AtomicBool>) {}
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn all_tools(&self) -> Vec<&dyn Tool> {
        self.tools.values().map(|t| t.as_ref()).collect()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.all_tools()
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Propagate a cancellation flag to all tools.
    pub fn set_cancel(&self, cancel: Arc<AtomicBool>) {
        for tool in self.tools.values() {
            tool.set_cancel(cancel.clone());
        }
    }
}

/// Resolves a file path safely, preventing relative traversal attacks.
///
/// Relative paths are resolved against cwd and must stay within it.
/// Absolute paths are normalized but not jailed (the caller explicitly chose them).
pub fn resolve_safe_path(path: &str) -> Result<std::path::PathBuf, String> {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return Ok(normalize_path(p));
    }
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot determine working directory: {e}"))?;
    let resolved = normalize_path(&cwd.join(p));
    if !resolved.starts_with(&cwd) {
        return Err(format!("path '{path}' escapes the working directory"));
    }
    Ok(resolved)
}

fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// Every built-in tool name, in registration order.
const BUILTIN_TOOLS: &[&str] = &[
    "read",
    "bash",
    "glob",
    "grep",
    "write",
    "edit",
    "multi_edit",
    "notebook_edit",
    "fetch",
    "web_search",
    "todo_write",
    "exit_plan_mode",
];

/// Construct a built-in tool by name — the single source of truth for the
/// name → implementation mapping used by both registry builders. The file
/// tools share `tracker`, so a read by one tool unlocks edits by another.
fn tool_by_name(name: &str, tracker: &Arc<FileTracker>) -> Option<Box<dyn Tool>> {
    match name {
        "read" => Some(Box::new(ReadTool::new(tracker.clone()))),
        "write" => Some(Box::new(WriteTool::new(tracker.clone()))),
        "edit" => Some(Box::new(EditTool::new(tracker.clone()))),
        "multi_edit" => Some(Box::new(MultiEditTool::new(tracker.clone()))),
        "notebook_edit" => Some(Box::new(NotebookEditTool::new(tracker.clone()))),
        "bash" => Some(Box::new(BashTool::new())),
        "glob" => Some(Box::new(GlobTool)),
        "grep" => Some(Box::new(GrepTool)),
        "fetch" => Some(Box::new(FetchTool::new())),
        "web_search" => Some(Box::new(WebSearchTool::new())),
        "todo_write" => Some(Box::new(TodoWriteTool)),
        "exit_plan_mode" => Some(Box::new(ExitPlanModeTool)),
        _ => None,
    }
}

pub fn default_registry() -> ToolRegistry {
    filtered_registry(BUILTIN_TOOLS)
}

/// Build a registry containing only the named tools.
/// Used by sub-agents to enforce their `ToolAccess::Only` restrictions.
/// Each registry has its own [`FileTracker`], so a sub-agent has to read a
/// file itself before editing it.
pub fn filtered_registry(names: &[&str]) -> ToolRegistry {
    let tracker = FileTracker::shared();
    let mut reg = ToolRegistry::new();
    for &name in names {
        if let Some(tool) = tool_by_name(name, &tracker) {
            reg.register(tool);
        }
    }
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_every_builtin_tool() {
        let reg = default_registry();
        for name in BUILTIN_TOOLS {
            assert!(reg.get(name).is_some(), "{name} missing");
        }
        assert_eq!(reg.specs().len(), BUILTIN_TOOLS.len());
    }

    #[test]
    fn filtered_registry_only_contains_the_named_tools() {
        let reg = filtered_registry(&["read", "grep", "not_a_tool"]);
        assert_eq!(reg.specs().len(), 2);
        assert!(reg.get("write").is_none());
    }

    #[tokio::test]
    async fn file_tools_in_one_registry_share_read_state() {
        let path = std::env::temp_dir().join(format!("io_registry_{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, "alpha\n").unwrap();
        let reg = default_registry();
        let call = |name: &str, args: serde_json::Value| {
            let input = ToolInput {
                name: name.to_string(),
                args: args
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            };
            let tool = reg.get(name).unwrap();
            async move { tool.execute(input).await }
        };
        let p = path.to_str().unwrap();
        let edit = serde_json::json!({"path": p, "old_string": "alpha", "new_string": "beta"});

        // Not read yet: refused.
        assert!(!call("edit", edit.clone()).await.success);
        // `read` unlocks `edit`, `multi_edit` and `write` alike.
        assert!(
            call("read", serde_json::json!({"file_path": p}))
                .await
                .success
        );
        assert!(call("edit", edit).await.success);
        let multi = serde_json::json!({"path": p, "edits": [{"old_string": "beta", "new_string": "gamma"}]});
        assert!(call("multi_edit", multi).await.success);
        assert!(
            call("write", serde_json::json!({"path": p, "content": "done"}))
                .await
                .success
        );
        let after = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(after, "done");
    }

    #[tokio::test]
    async fn separate_registries_do_not_share_read_state() {
        let path = std::env::temp_dir().join(format!("io_registry_{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, "alpha\n").unwrap();
        let p = path.to_str().unwrap().to_string();
        let read = filtered_registry(&["read"]);
        let edit = filtered_registry(&["edit"]);
        let args = |v: serde_json::Value| {
            v.as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let out = read
            .get("read")
            .unwrap()
            .execute(ToolInput {
                name: "read".into(),
                args: args(serde_json::json!({"file_path": p})),
            })
            .await;
        assert!(out.success);
        let out = edit
            .get("edit")
            .unwrap()
            .execute(ToolInput {
                name: "edit".into(),
                args: args(
                    serde_json::json!({"path": p, "old_string": "alpha", "new_string": "x"}),
                ),
            })
            .await;
        std::fs::remove_file(&path).ok();
        assert!(!out.success, "a sub-agent registry must read for itself");
    }
}
