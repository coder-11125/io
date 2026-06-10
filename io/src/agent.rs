use io_agents::AgentConfig;

/// Show an interactive picker of full agents. Returns the chosen config.
pub fn run(current_id: &str) -> anyhow::Result<AgentConfig> {
    let agents = io_agents::builtin::full_agents();
    let items: Vec<(&str, &str)> = agents.iter().map(|a| (a.name, a.description)).collect();
    let current = agents.iter().position(|a| a.id == current_id);

    println!();
    let picked = crate::picker::pick_with_hint(&items, current)?;
    println!();

    agents
        .into_iter()
        .nth(picked)
        .ok_or_else(|| anyhow::anyhow!("picker returned out-of-range index {picked}"))
}
