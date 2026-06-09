use std::sync::Arc;
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
use async_trait::async_trait;
use io_agents::agent_config::ToolAccess;
use crate::provider::ProviderKind;
use crate::tools::{Tool, ToolInput, ToolOutput, filtered_registry};
use crate::sandbox::PermissionChecker;
use crate::memory::SessionStore;
use crate::agent::Agent;

/// A tool that lets a full agent delegate a scoped task to a sub-agent.
///
/// Sub-agents run with their own restricted tool set, a fresh ephemeral
/// session, and no interactive permission prompts. They cannot spawn further
/// agents, preventing infinite recursion.
///
/// Holds a shared cancellation flag — when set (e.g. by Esc in the UI),
/// the spawned agent aborts early and returns a cancellation message.
pub struct SpawnAgentTool {
    provider: Arc<ProviderKind>,
    model_id: String,
    max_tokens: u32,
    description: String,
    cancel: Mutex<Arc<AtomicBool>>,
}

impl SpawnAgentTool {
    pub fn new(provider: Arc<ProviderKind>, model_id: String, max_tokens: u32) -> Self {
        let sub_agents: Vec<String> = io_agents::builtin::all()
            .into_iter()
            .filter(|a| a.tool_access != ToolAccess::All)
            .map(|a| format!("{} — {}", a.id, a.description))
            .collect();

        let description = format!(
            "Spawn a sub-agent to handle a focused, scoped task and return its output.\n\
             Use this when a part of the current task is best handled by a specialist.\n\
             Sub-agents run read-only or with restricted tools — they cannot spawn\n\
             further agents.\n\n\
             Available sub-agents:\n{}",
            sub_agents
                .iter()
                .map(|s| format!("  - {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        );

        Self { provider, model_id, max_tokens, description, cancel: Mutex::new(Arc::new(AtomicBool::new(false))) }
    }

}

#[async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &str { "spawn_agent" }

    fn description(&self) -> &str { &self.description }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "ID of the sub-agent to spawn (e.g. diagnose, explore, review, test, security, docs, git, general)"
                },
                "task": {
                    "type": "string",
                    "description": "The full task or question to pass to the sub-agent. Be specific — the sub-agent has no prior context."
                }
            },
            "required": ["agent_id", "task"]
        })
    }

    fn set_cancel(&self, cancel: Arc<AtomicBool>) {
        *self.cancel.lock().unwrap() = cancel;
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        if self.cancel.lock().unwrap().load(Ordering::Relaxed) {
            return ToolOutput::err("cancelled");
        }

        let agent_id = match input.args.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return ToolOutput::err("missing agent_id"),
        };
        let task = match input.args.get("task").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return ToolOutput::err("missing task"),
        };

        if self.cancel.lock().unwrap().load(Ordering::Relaxed) {
            return ToolOutput::err("cancelled");
        }

        let config = match io_agents::builtin::by_id(&agent_id) {
            Some(c) => c,
            None => return ToolOutput::err(
                &format!("unknown agent_id '{agent_id}' — check available sub-agents in the tool description")
            ),
        };

        // Block full agents from being spawned to prevent recursive loops.
        if config.tool_access == ToolAccess::All {
            return ToolOutput::err(
                &format!("'{agent_id}' is a full agent and cannot be spawned as a sub-agent")
            );
        }

        let tools = match &config.tool_access {
            ToolAccess::Only(names) => {
                let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                filtered_registry(&name_refs)
            }
            ToolAccess::All => unreachable!(),
        };

        if self.cancel.lock().unwrap().load(Ordering::Relaxed) {
            return ToolOutput::err("cancelled");
        }

        let memory = match SessionStore::new() {
            Ok(m) => m,
            Err(e) => return ToolOutput::err(&format!("failed to init session store: {e}")),
        };

        let model_id = config.suggested_model
            .unwrap_or(self.model_id.as_str())
            .to_string();

        let permissions = PermissionChecker::new("allow");

        let cancel = self.cancel.lock().unwrap().clone();
        let agent = Agent::new(
            self.provider.clone(),
            tools,
            memory,
            permissions,
            config.system_prompt.clone(),
            None,
            model_id,
            self.max_tokens,
            false,
        );
        agent.set_cancel(cancel);

        let session_id = agent.session_id().await;
        let result = agent.run_turn(&task).await;

        // Remove the ephemeral session — it's single-use and would accumulate in the DB.
        if let Ok(store) = SessionStore::new() {
            let _ = store.delete_session(session_id);
        }

        match result {
            Ok(r) => ToolOutput::ok(r),
            Err(e) => ToolOutput::err(format!("sub-agent '{agent_id}' failed: {e}")),
        }
    }
}
