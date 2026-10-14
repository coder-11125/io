//! Model Context Protocol client: connect to MCP servers over stdio, HTTP or
//! SSE and expose their tools to the agent as `mcp__<server>__<tool>`.
//!
//! [`McpManager`] owns the connections for the life of the app; agents are
//! rebuilt often (every `/model` switch) and share it rather than restarting
//! servers. Servers come from `~/.io/mcp.json` (user) and the project's
//! `.mcp.json`; project servers launch only after the user approves them.

pub mod client;
pub mod config;
pub mod sse;
pub mod tool;
pub mod transport;

pub use client::{CallOutcome, McpClient, ToolInfo};
pub use config::{
    collect_entries, project_config_path, read_servers, remove_server, user_config_path,
    validate_server_name, write_server, Launch, McpServerConfig, Scope, ServerEntry, Trust,
    TrustStore,
};
pub use tool::{tool_name, McpTool};

use crate::tools::{SharedTool, Tool};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// Timeouts for connecting and for tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub startup_timeout: Duration,
    pub tool_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
        }
    }
}

impl Settings {
    /// Defaults, overridable with `IO_MCP_STARTUP_TIMEOUT_MS` and
    /// `IO_MCP_TOOL_TIMEOUT_MS`.
    pub fn from_env() -> Self {
        let ms = |var: &str, default: Duration| {
            std::env::var(var)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&n| n > 0)
                .map_or(default, Duration::from_millis)
        };
        Self {
            startup_timeout: ms("IO_MCP_STARTUP_TIMEOUT_MS", DEFAULT_STARTUP_TIMEOUT),
            tool_timeout: ms("IO_MCP_TOOL_TIMEOUT_MS", DEFAULT_TOOL_TIMEOUT),
        }
    }
}

/// Where a server stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Connected {
        tools: usize,
    },
    Failed(String),
    /// A project server the user hasn't approved yet.
    NeedsApproval,
    /// A project server the user declined.
    Denied,
}

impl ServerStatus {
    pub fn label(&self) -> String {
        match self {
            ServerStatus::Connected { tools } => {
                format!(
                    "connected, {tools} tool{}",
                    if *tools == 1 { "" } else { "s" }
                )
            }
            ServerStatus::Failed(why) => format!("failed: {why}"),
            ServerStatus::NeedsApproval => "needs approval".to_string(),
            ServerStatus::Denied => "not approved".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerState {
    pub name: String,
    pub scope: Scope,
    pub transport: &'static str,
    pub summary: String,
    pub status: ServerStatus,
}

/// The project servers sorted by what the trust store says.
#[derive(Debug, Default)]
pub struct TrustPartition {
    /// User/plugin servers and approved project servers: safe to start.
    pub start: Vec<ServerEntry>,
    /// Project servers nobody has decided about.
    pub unknown: Vec<ServerEntry>,
    /// Project servers the user declined.
    pub denied: Vec<ServerEntry>,
}

/// Split `entries` by trust. Only project-scope servers need approval: the
/// project file is untrusted input that can name any command.
pub fn partition_by_trust(
    entries: Vec<ServerEntry>,
    root: &Path,
    trust: &TrustStore,
) -> TrustPartition {
    let mut out = TrustPartition::default();
    for entry in entries {
        if entry.scope != Scope::Project {
            out.start.push(entry);
            continue;
        }
        match trust.status(root, &entry.name, &entry.config) {
            Trust::Approved => out.start.push(entry),
            Trust::Denied => out.denied.push(entry),
            Trust::Unknown => out.unknown.push(entry),
        }
    }
    out
}

fn skipped_state(entry: &ServerEntry, status: ServerStatus) -> ServerState {
    ServerState {
        name: entry.name.clone(),
        scope: entry.scope.clone(),
        transport: entry.config.transport_kind().unwrap_or("?"),
        summary: entry.config.summary(),
        status,
    }
}

/// Live MCP connections and the tools they provide.
#[derive(Default)]
pub struct McpManager {
    states: Vec<ServerState>,
    tools: Vec<Arc<McpTool>>,
    // Keeps the connections (and their child processes) alive.
    _clients: Vec<Arc<McpClient>>,
}

impl McpManager {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Connect to `start` concurrently. `skipped` are servers that were not
    /// started (awaiting approval, declined); they appear in `states()` only.
    pub async fn start(
        start: Vec<ServerEntry>,
        skipped: Vec<(ServerEntry, ServerStatus)>,
        settings: Settings,
    ) -> Self {
        let results =
            futures::future::join_all(start.iter().map(|entry| connect_one(entry, settings))).await;

        let mut manager = Self::default();
        let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (entry, result) in start.iter().zip(results) {
            let mut state = skipped_state(entry, ServerStatus::Failed(String::new()));
            match result {
                Ok((client, infos)) => {
                    let mut count = 0;
                    for info in &infos {
                        let tool =
                            McpTool::new(&entry.name, info, client.clone(), settings.tool_timeout);
                        if !taken.insert(tool.name().to_string()) {
                            tracing::warn!(
                                "MCP tool name clash: {} from '{}' ignored",
                                tool.name(),
                                entry.name
                            );
                            continue;
                        }
                        manager.tools.push(Arc::new(tool));
                        count += 1;
                    }
                    state.status = ServerStatus::Connected { tools: count };
                    manager._clients.push(client);
                }
                Err(e) => state.status = ServerStatus::Failed(format!("{e:#}")),
            }
            manager.states.push(state);
        }
        for (entry, status) in skipped {
            manager.states.push(skipped_state(&entry, status));
        }
        manager.states.sort_by(|a, b| a.name.cmp(&b.name));
        manager
    }

    pub fn states(&self) -> &[ServerState] {
        &self.states
    }

    /// The tools to register with an agent. Cheap: they share the live
    /// connections.
    pub fn tools(&self) -> Vec<Box<dyn Tool>> {
        self.tools
            .iter()
            .map(|t| Box::new(SharedTool(t.clone())) as Box<dyn Tool>)
            .collect()
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// Names of tools their servers marked read-only (advisory).
    pub fn read_only_tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|t| t.read_only())
            .map(|t| t.name().to_string())
            .collect()
    }
}

async fn connect_one(
    entry: &ServerEntry,
    settings: Settings,
) -> anyhow::Result<(Arc<McpClient>, Vec<ToolInfo>)> {
    let launch = entry.config.resolve().map_err(|e| anyhow::anyhow!("{e}"))?;
    let client = Arc::new(
        McpClient::connect(
            &entry.name,
            &launch,
            settings.startup_timeout,
            settings.tool_timeout,
        )
        .await?,
    );
    let tools = tokio::time::timeout(settings.startup_timeout, client.list_tools())
        .await
        .map_err(|_| anyhow::anyhow!("tools/list timed out"))??;
    Ok((client, tools))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, scope: Scope, cmd: &str) -> ServerEntry {
        ServerEntry {
            name: name.to_string(),
            scope,
            config: McpServerConfig {
                command: Some(cmd.to_string()),
                ..Default::default()
            },
        }
    }

    #[test]
    fn only_project_servers_need_approval() {
        let root = Path::new("/work/app");
        let mut trust = TrustStore::load_from(
            std::env::temp_dir().join(format!("io_mcp_part_{}.json", uuid::Uuid::new_v4())),
        );
        let approved = entry("ok", Scope::Project, "a");
        let declined = entry("no", Scope::Project, "b");
        trust.set(root, "ok", &approved.config, true).unwrap();
        trust.set(root, "no", &declined.config, false).unwrap();

        let parts = partition_by_trust(
            vec![
                entry("u", Scope::User, "c"),
                entry("p", Scope::Plugin("pl".into()), "d"),
                approved,
                declined,
                entry("new", Scope::Project, "e"),
            ],
            root,
            &trust,
        );
        let names = |v: &[ServerEntry]| v.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&parts.start), ["u", "p", "ok"]);
        assert_eq!(names(&parts.unknown), ["new"]);
        assert_eq!(names(&parts.denied), ["no"]);
    }

    #[test]
    fn statuses_read_naturally() {
        assert_eq!(
            ServerStatus::Connected { tools: 1 }.label(),
            "connected, 1 tool"
        );
        assert_eq!(
            ServerStatus::Connected { tools: 3 }.label(),
            "connected, 3 tools"
        );
        assert!(ServerStatus::Failed("boom".into()).label().contains("boom"));
        assert_eq!(ServerStatus::NeedsApproval.label(), "needs approval");
    }

    #[tokio::test]
    async fn an_unlaunchable_server_is_reported_not_fatal() {
        let manager = McpManager::start(
            vec![entry("broken", Scope::User, "/definitely/not/a/program")],
            vec![(
                entry("pending", Scope::Project, "x"),
                ServerStatus::NeedsApproval,
            )],
            Settings::default(),
        )
        .await;
        assert_eq!(manager.tool_count(), 0);
        let states = manager.states();
        assert_eq!(states.len(), 2);
        let broken = states.iter().find(|s| s.name == "broken").unwrap();
        assert!(matches!(&broken.status, ServerStatus::Failed(m) if m.contains("failed to start")));
        let pending = states.iter().find(|s| s.name == "pending").unwrap();
        assert_eq!(pending.status, ServerStatus::NeedsApproval);
    }

    #[test]
    fn timeouts_can_be_overridden_from_the_environment() {
        let d = Settings::default();
        assert_eq!(d.startup_timeout, Duration::from_secs(20));
        assert_eq!(d.tool_timeout, Duration::from_secs(60));
    }
}
