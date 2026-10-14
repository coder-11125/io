//! `todo_write`: the model's own task list for planning and tracking
//! multi-step work. Every call replaces the whole list; the TUI renders the
//! list live from the call's input, so the tool itself holds no state.

use super::{Tool, ToolInput, ToolOutput};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Imperative description, e.g. "Run the tests".
    pub content: String,
    pub status: TodoStatus,
    /// Present-continuous form shown while the item runs, e.g. "Running the
    /// tests". Falls back to `content`.
    #[serde(default, rename = "activeForm", alias = "active_form")]
    pub active_form: Option<String>,
}

impl TodoItem {
    /// The text to show for this item: the active form while in progress.
    pub fn display(&self) -> &str {
        match (self.status, self.active_form.as_deref()) {
            (TodoStatus::InProgress, Some(active)) if !active.trim().is_empty() => active,
            _ => &self.content,
        }
    }
}

/// Parse the `todos` argument of a `todo_write` call.
pub fn parse_todos(value: &serde_json::Value) -> Result<Vec<TodoItem>, String> {
    let todos: Vec<TodoItem> = serde_json::from_value(value.clone()).map_err(|e| {
        format!(
            "todos must be an array of {{content, status, activeForm}} objects with status \
             pending, in_progress or completed ({e})"
        )
    })?;
    if todos.iter().any(|t| t.content.trim().is_empty()) {
        return Err("every todo needs non-empty content".to_string());
    }
    Ok(todos)
}

/// One-line count summary, e.g. `2 pending, 1 in progress, 3 completed`.
pub fn summarize(todos: &[TodoItem]) -> String {
    let count = |s| todos.iter().filter(|t| t.status == s).count();
    format!(
        "{} pending, {} in progress, {} completed",
        count(TodoStatus::Pending),
        count(TodoStatus::InProgress),
        count(TodoStatus::Completed)
    )
}

pub struct TodoWriteTool;

#[async_trait::async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todo_write"
    }

    fn description(&self) -> &str {
        "Create and update a task list to plan and track progress on multi-step work; the user \
         sees it live. Use it for tasks with three or more steps or when the user gives several \
         tasks, not for trivial requests. Each call REPLACES the whole list, so send every item \
         each time. Keep exactly one item `in_progress` while you work on it, and mark items \
         `completed` as soon as they are done — never batch completions up. Do not mark an item \
         completed if tests fail or the work is partial."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The full, updated todo list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "What to do, in the imperative (e.g. \"Run the tests\")" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "activeForm": { "type": "string", "description": "Present-continuous form shown while in progress (e.g. \"Running the tests\")" }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        let Some(todos) = input.args.get("todos") else {
            return ToolOutput::err("missing required argument: todos");
        };
        match parse_todos(todos) {
            Ok(list) => ToolOutput::ok(format!(
                "Todo list updated ({}). Keep using it to track progress.",
                summarize(&list)
            )),
            Err(e) => ToolOutput::err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(args: serde_json::Value) -> ToolOutput {
        let args = args
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        futures::executor::block_on(TodoWriteTool.execute(ToolInput {
            name: "todo_write".into(),
            args,
        }))
    }

    #[test]
    fn parses_items_with_camel_case_active_form() {
        let todos = parse_todos(&json!([
            {"content": "Write code", "status": "in_progress", "activeForm": "Writing code"},
            {"content": "Test it", "status": "pending"},
            {"content": "Ship", "status": "completed", "active_form": "Shipping"}
        ]))
        .unwrap();
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[0].display(), "Writing code");
        assert_eq!(todos[1].display(), "Test it");
        // The active form only shows while in progress.
        assert_eq!(todos[2].display(), "Ship");
        assert_eq!(summarize(&todos), "1 pending, 1 in progress, 1 completed");
    }

    #[test]
    fn rejects_bad_status_and_empty_content() {
        assert!(parse_todos(&json!([{"content": "x", "status": "done"}])).is_err());
        assert!(parse_todos(&json!("nope")).is_err());
        assert!(
            parse_todos(&json!([{"content": "  ", "status": "pending"}]))
                .unwrap_err()
                .contains("non-empty")
        );
    }

    #[test]
    fn tool_confirms_with_a_summary_and_errors_on_bad_input() {
        let ok = call(json!({"todos": [
            {"content": "a", "status": "completed"},
            {"content": "b", "status": "pending"}
        ]}));
        assert!(ok.success);
        assert!(ok.data.contains("1 pending, 0 in progress, 1 completed"));

        assert!(call(json!({}))
            .data
            .contains("missing required argument: todos"));
        assert!(!call(json!({"todos": [{"content": "a", "status": "later"}]})).success);
        // An empty list clears the list and is valid.
        assert!(call(json!({"todos": []})).success);
    }
}
