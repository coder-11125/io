pub mod agent;
pub mod command_safety;
pub mod compact;
pub mod config;
pub mod export;
pub mod hooks;
pub mod instructions;
pub mod mcp;
pub mod memory;
pub mod oauth;
pub mod os_sandbox;
pub mod output_style;
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

/// The `<project-context>` block for a session at `root` (see
/// [`instructions`] for what is loaded). Returns `None` if there are no
/// memory files.
pub fn load_project_context(root: &std::path::Path) -> Option<String> {
    instructions::load(root, root).0
}
