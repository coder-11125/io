pub mod build;
pub mod debug;
pub mod docs;
pub mod explore;
pub mod general;
pub mod git;
pub mod plan;
pub mod refactor;
pub mod review;
pub mod security;
pub mod test;

use crate::agent_config::AgentConfig;

/// Returns all built-in agent configurations.
pub fn all() -> Vec<AgentConfig> {
    vec![
        general::config(),
        explore::config(),
        build::config(),
        plan::config(),
        review::config(),
        test::config(),
        debug::config(),
        debug::subagent_config(),
        refactor::config(),
        docs::config(),
        security::config(),
        git::config(),
    ]
}

/// Returns only the full agents (those with unrestricted tool access).
pub fn full_agents() -> Vec<AgentConfig> {
    use crate::agent_config::ToolAccess;
    all()
        .into_iter()
        .filter(|a| a.tool_access == ToolAccess::All)
        .collect()
}

/// Looks up a built-in agent by its `id` field.
pub fn by_id(id: &str) -> Option<AgentConfig> {
    all().into_iter().find(|a| a.id == id)
}
