use serde::{Deserialize, Serialize};

/// Which tools an agent is permitted to call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAccess {
    /// Agent may use every tool in the registry.
    All,
    /// Agent is restricted to the listed tool names.
    Only(Vec<String>),
}

impl ToolAccess {
    pub fn only(tools: &[&str]) -> Self {
        Self::Only(tools.iter().map(|s| s.to_string()).collect())
    }
}

/// A fully self-contained description of how an agent should behave.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Stable machine identifier, e.g. `"build"`.
    pub id: &'static str,

    /// Human-readable name shown in the UI.
    pub name: &'static str,

    /// One-line description of what this agent does.
    pub description: &'static str,

    /// Full system prompt injected at the start of every conversation.
    pub system_prompt: String,

    /// Which tools the agent is allowed to use.
    pub tool_access: ToolAccess,

    /// Recommended model for this agent role, e.g. `"claude-sonnet-4-6"`.
    /// `None` means: use whatever the user configured as their default.
    pub suggested_model: Option<&'static str>,

    /// Whether the agent should stop after a single LLM call (no tool loop).
    pub single_shot: bool,

    /// When true, write and edit tool calls are auto-allowed without prompting
    /// the user. Should be false for plan (which confirms before acting) and
    /// read-only agents (which have no write access anyway).
    pub auto_allow_writes: bool,
}

impl AgentConfig {
    /// Returns `true` if the given tool name is accessible to this agent.
    pub fn allows_tool(&self, tool_name: &str) -> bool {
        match &self.tool_access {
            ToolAccess::All => true,
            ToolAccess::Only(list) => list.iter().any(|t| t == tool_name),
        }
    }
}
