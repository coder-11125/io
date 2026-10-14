//! The working mode (default / accept edits / plan) and the extended-thinking
//! setting as the user sees them: one process-wide mode that survives agent
//! rebuilds (`/model`, `/agent`, …), and the badge in the status row.

use io_runtime::config::{Config, ThinkingConfig};
use io_runtime::{Agent, InteractionMode};
use std::sync::Mutex;

/// The mode a freshly built agent starts in. Agents are rebuilt on every
/// provider/model/agent switch, so the mode lives here rather than on them.
static MODE: Mutex<InteractionMode> = Mutex::new(InteractionMode::Default);

pub fn current_mode() -> InteractionMode {
    *MODE.lock().unwrap()
}

/// The status-row text for a mode and thinking setting, e.g.
/// `⏸ plan mode · ✻ thinking 8K`. Empty when both are at their defaults.
pub fn badge_text(mode: InteractionMode, thinking: &ThinkingConfig) -> String {
    let mut parts: Vec<String> = Vec::new();
    match mode {
        InteractionMode::Default => {}
        InteractionMode::AcceptEdits => parts.push("⏵⏵ accept edits".to_string()),
        InteractionMode::Plan => parts.push("⏸ plan mode".to_string()),
    }
    if thinking.enabled {
        parts.push(format!(
            "✻ thinking {}K",
            thinking.budget_tokens.div_ceil(1000)
        ));
    }
    parts.join(" · ")
}

/// Redraw-ready badge for the given state.
pub fn update_badge(mode: InteractionMode, thinking: &ThinkingConfig) {
    io_tui::render::set_status_badge(&badge_text(mode, thinking));
}

/// Make `mode` current for `agent` and for agents built later.
pub fn set_mode(agent: &Agent, mode: InteractionMode) {
    *MODE.lock().unwrap() = mode;
    agent.set_interaction_mode(mode);
    update_badge(mode, &agent.thinking());
}

/// Shift+Tab: default → accept edits → plan → default.
pub fn cycle_mode(agent: &Agent) -> InteractionMode {
    let next = agent.interaction_mode().next();
    set_mode(agent, next);
    next
}

/// Pick up a mode change the agent made itself — approving a plan leaves plan
/// mode — so the badge and later agents follow.
pub fn sync_from_agent(agent: &Agent) {
    set_mode(agent, agent.interaction_mode());
}

fn describe(thinking: &ThinkingConfig) -> String {
    if thinking.enabled {
        format!(
            "Extended thinking on · budget {} tokens",
            thinking.budget_tokens
        )
    } else {
        "Extended thinking off".to_string()
    }
}

/// Apply a `/think` argument (empty toggles), persist it to the config, and
/// update the agent and badge. Returns the message to show.
pub fn apply_think_command(agent: &Agent, arg: &str) -> anyhow::Result<String> {
    let mut config = Config::load()?;
    let mut thinking = config.thinking.clone();
    if arg.trim().is_empty() {
        thinking.enabled = !thinking.enabled;
    } else {
        let (enabled, budget) =
            io_runtime::thinking::parse_setting(arg).map_err(|e| anyhow::anyhow!(e))?;
        thinking.enabled = enabled;
        if let Some(budget) = budget {
            thinking.budget_tokens = budget;
        }
    }
    config.thinking = thinking.clone();
    config.save()?;
    agent.set_thinking(thinking.clone());
    update_badge(agent.interaction_mode(), &thinking);
    let mut message = describe(&thinking);
    if thinking.enabled && !agent.supports_thinking() {
        message.push_str(" — but the current model doesn't support it, so it has no effect");
    }
    Ok(message)
}

/// Alt+T: flip extended thinking.
pub fn toggle_thinking(agent: &Agent) -> anyhow::Result<String> {
    apply_think_command(agent, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thinking(enabled: bool, budget: u32) -> ThinkingConfig {
        ThinkingConfig {
            enabled,
            budget_tokens: budget,
            keywords: true,
        }
    }

    #[test]
    fn badge_is_empty_at_the_defaults() {
        assert_eq!(
            badge_text(InteractionMode::Default, &thinking(false, 8000)),
            ""
        );
    }

    #[test]
    fn badge_names_the_mode_and_thinking_budget() {
        assert_eq!(
            badge_text(InteractionMode::Plan, &thinking(false, 8000)),
            "⏸ plan mode"
        );
        assert_eq!(
            badge_text(InteractionMode::AcceptEdits, &thinking(false, 8000)),
            "⏵⏵ accept edits"
        );
        assert_eq!(
            badge_text(InteractionMode::Default, &thinking(true, 8000)),
            "✻ thinking 8K"
        );
        assert_eq!(
            badge_text(InteractionMode::Plan, &thinking(true, 31_999)),
            "⏸ plan mode · ✻ thinking 32K"
        );
    }

    #[test]
    fn describe_reports_the_state() {
        assert_eq!(describe(&thinking(false, 8000)), "Extended thinking off");
        assert_eq!(
            describe(&thinking(true, 4000)),
            "Extended thinking on · budget 4000 tokens"
        );
    }
}
