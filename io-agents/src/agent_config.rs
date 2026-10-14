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

/// Guidance on the shared tool set, appended to the prompt of every agent
/// with full tool access (restricted sub-agents get only what their own
/// prompt says).
pub const TOOL_GUIDANCE: &str = indoc::indoc! {"
    ## Working with the tools

    Task tracking:
    - For work with three or more steps, or when the user gives several tasks, keep a todo
      list with todo_write. Keep exactly one item in_progress, mark items completed as soon
      as they are done, and never mark something completed while its tests fail. Skip the
      list for trivial requests.

    Editing files:
    - Read a file before you change it. write, edit, multi_edit and notebook_edit refuse a
      file you have not read, or one that changed on disk since you read it; read it again
      and retry.
    - Prefer edit for one change to an existing file and multi_edit for several. Use write
      for new files or complete rewrites, and notebook_edit for .ipynb cells.

    Reading media:
    - read shows you images (png, jpg, gif, webp) directly and extracts text from PDFs; pass
      `pages` for a long PDF. Notebooks are read cell by cell.

    The web:
    - web_search finds current information and fetch reads a URL. Give fetch a `prompt` to
      get just the answer you need from a long page. Cite the URLs you relied on.

    Plan mode:
    - When plan mode is active you can only read and research. Finish by calling
      exit_plan_mode with your plan, and do not start editing before the user approves it.
"};

impl AgentConfig {
    /// The system prompt to run this agent with: its own prompt plus, for
    /// full agents, the shared [`TOOL_GUIDANCE`].
    pub fn full_system_prompt(&self) -> String {
        match self.tool_access {
            ToolAccess::All => format!("{}\n\n{TOOL_GUIDANCE}", self.system_prompt.trim_end()),
            ToolAccess::Only(_) => self.system_prompt.clone(),
        }
    }

    /// Returns `true` if the given tool name is accessible to this agent.
    pub fn allows_tool(&self, tool_name: &str) -> bool {
        match &self.tool_access {
            ToolAccess::All => true,
            ToolAccess::Only(list) => list.iter().any(|t| t == tool_name),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::builtin;

    #[test]
    fn full_agents_get_the_tool_guidance_and_sub_agents_do_not() {
        let build = builtin::by_id("build").unwrap();
        let prompt = build.full_system_prompt();
        assert!(prompt.starts_with(build.system_prompt.trim_end()));
        for tool in ["todo_write", "multi_edit", "exit_plan_mode", "web_search"] {
            assert!(prompt.contains(tool), "guidance should mention {tool}");
        }
        let explore = builtin::by_id("explore").unwrap();
        assert_eq!(explore.full_system_prompt(), explore.system_prompt);
    }
}
