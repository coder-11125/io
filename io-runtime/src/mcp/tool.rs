//! A tool backed by an MCP server, named `mcp__<server>__<tool>`.

use super::client::McpClient;
use crate::tools::{Tool, ToolInput, ToolOutput};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Providers cap tool names around this length.
const MAX_TOOL_NAME: usize = 64;

/// `mcp__<server>__<tool>`, with the tool part cleaned to what providers
/// accept (`[A-Za-z0-9_-]`) and the whole thing kept within 64 characters.
pub fn tool_name(server: &str, tool: &str) -> String {
    let clean: String = tool
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut name = format!("mcp__{server}__{clean}");
    name.truncate(MAX_TOOL_NAME);
    name
}

/// Providers want an object schema; some servers omit it or send something
/// else. Anything that isn't `type: object` becomes an empty-object schema.
fn normalize_schema(schema: &Value) -> Value {
    if schema.get("type").and_then(Value::as_str) == Some("object") {
        schema.clone()
    } else {
        json!({ "type": "object", "properties": {} })
    }
}

pub struct McpTool {
    full_name: String,
    remote_name: String,
    description: String,
    schema: Value,
    client: Arc<McpClient>,
    timeout: Duration,
    read_only: bool,
}

impl McpTool {
    pub fn new(
        server: &str,
        info: &super::client::ToolInfo,
        client: Arc<McpClient>,
        timeout: Duration,
    ) -> Self {
        let blurb = if info.description.trim().is_empty() {
            format!("Tool '{}' from the '{server}' MCP server.", info.name)
        } else {
            format!("[{server} MCP] {}", info.description.trim())
        };
        Self {
            full_name: tool_name(server, &info.name),
            remote_name: info.name.clone(),
            description: blurb,
            schema: normalize_schema(&info.input_schema),
            client,
            timeout,
            read_only: info.read_only,
        }
    }

    /// Whether the server marked this tool read-only. Only a hint.
    pub fn read_only(&self) -> bool {
        self.read_only
    }
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let arguments: serde_json::Map<String, Value> = input.args.into_iter().collect();
        let call = self
            .client
            .call_tool(&self.remote_name, Value::Object(arguments));
        let outcome = match tokio::time::timeout(self.timeout, call).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) => return ToolOutput::err(format!("MCP call failed: {e}")),
            Err(_) => {
                return ToolOutput::err(format!(
                    "MCP call timed out after {}s",
                    self.timeout.as_secs()
                ))
            }
        };
        let mut out = if outcome.is_error {
            ToolOutput::err(if outcome.text.is_empty() {
                "the tool reported an error".to_string()
            } else {
                outcome.text
            })
        } else if outcome.text.is_empty() && outcome.images.is_empty() {
            ToolOutput::ok("(the tool returned no output)")
        } else {
            ToolOutput::ok(outcome.text)
        };
        out.images = outcome.images;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_prefixed_cleaned_and_bounded() {
        assert_eq!(
            tool_name("github", "search_issues"),
            "mcp__github__search_issues"
        );
        assert_eq!(tool_name("fs", "read.file/v2"), "mcp__fs__read_file_v2");
        let long = tool_name("srv", &"t".repeat(100));
        assert_eq!(long.len(), MAX_TOOL_NAME);
        assert!(long.starts_with("mcp__srv__t"));
    }

    #[test]
    fn schemas_are_forced_to_objects() {
        let ok = json!({ "type": "object", "properties": { "a": {} }, "required": ["a"] });
        assert_eq!(normalize_schema(&ok), ok);
        for bad in [
            json!({}),
            json!({ "type": "string" }),
            json!(null),
            json!([]),
        ] {
            assert_eq!(normalize_schema(&bad)["type"], "object");
        }
    }
}
