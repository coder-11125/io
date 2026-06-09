use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use serde::{Deserialize, Serialize};

pub mod read;
pub mod bash;
pub mod glob;
pub mod grep;
pub mod write;
pub mod edit;
pub mod spawn;

pub use read::ReadTool;
pub use bash::BashTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use write::WriteTool;
pub use edit::EditTool;
pub use spawn::SpawnAgentTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInput {
    pub name: String,
    pub args: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub success: bool,
    pub data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolOutput {
    pub fn ok(data: impl Into<String>) -> Self {
        Self { success: true, data: data.into(), error: None }
    }

    pub fn err(error: impl Into<String>) -> Self {
        let msg = error.into();
        Self { success: false, data: msg.clone(), error: Some(msg) }
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
        Self { tools: HashMap::new() }
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
        self.all_tools().iter().map(|t| ToolSpec {
            name: t.name().to_string(),
            description: t.description().to_string(),
            input_schema: t.input_schema(),
        }).collect()
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
    let cwd = std::env::current_dir()
        .map_err(|e| format!("cannot determine working directory: {e}"))?;
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
            std::path::Component::ParentDir => { out.pop(); }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

pub fn default_registry() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(ReadTool));
    reg.register(Box::new(BashTool));
    reg.register(Box::new(GlobTool));
    reg.register(Box::new(GrepTool));
    reg.register(Box::new(WriteTool));
    reg.register(Box::new(EditTool));
    reg
}

/// Build a registry containing only the named tools.
/// Used by sub-agents to enforce their `ToolAccess::Only` restrictions.
pub fn filtered_registry(names: &[&str]) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    for &name in names {
        match name {
            "read"  => reg.register(Box::new(ReadTool)),
            "write" => reg.register(Box::new(WriteTool)),
            "edit"  => reg.register(Box::new(EditTool)),
            "bash"  => reg.register(Box::new(BashTool)),
            "glob"  => reg.register(Box::new(GlobTool)),
            "grep"  => reg.register(Box::new(GrepTool)),
            _       => {}
        }
    }
    reg
}
