pub mod agent;
pub mod command_safety;
pub mod compact;
pub mod config;
pub mod memory;
pub mod pricing;
pub mod provider;
pub mod sandbox;
pub mod tools;
pub mod types;

pub use agent::{Agent, AgentEvent, Cancelled, PermissionReply, PromptFn};
pub use compact::CompactResult;
pub use pricing::{
    get_pricing_for_model, get_provider_pricing_category, ModelPricing, ProviderPricingCategory,
};
pub use tools::SpawnAgentTool;
pub use types::{Session, SessionId};

/// Read `AGENTS.md` and `CLAUDE.md` from the project root and return their
/// contents as a `<project-context>` block. Returns `None` if neither exists.
pub fn load_project_context(root: &std::path::Path) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    for name in &["AGENTS.md", "CLAUDE.md"] {
        if let Ok(content) = std::fs::read_to_string(root.join(name)) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                sections.push(format!("### {name}\n\n{trimmed}"));
            }
        }
    }
    if sections.is_empty() {
        return None;
    }
    Some(format!(
        "<project-context>\n{}\n</project-context>",
        sections.join("\n\n")
    ))
}
