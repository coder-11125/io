pub mod agent;
pub mod command_safety;
pub mod compact;
pub mod config;
pub mod hooks;
pub mod mcp;
pub mod memory;
pub mod oauth;
pub mod pricing;
pub mod provider;
pub mod rewind;
pub mod rules;
pub mod sandbox;
pub mod settings;
pub mod thinking;
pub mod tools;
pub mod types;

pub use agent::{Agent, AgentEvent, Cancelled, PermissionReply, PromptBlocked, PromptFn};
pub use compact::CompactResult;
pub use hooks::{HookEntry, Hooks, HooksConfig};
pub use pricing::{
    cache_rates_for_model, get_pricing_for_model, get_provider_pricing_category, CacheRates,
    ModelPricing, ProviderPricingCategory,
};
pub use rewind::RewindResult;
pub use sandbox::InteractionMode;
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
