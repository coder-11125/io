//! `exit_plan_mode`: how the model hands a finished plan to the user while
//! plan mode (read-only) is active. The agent loop owns the approval — it
//! shows the plan, asks the user, and on approval switches out of plan mode —
//! so the tool itself only validates the plan and confirms.

use super::{Tool, ToolInput, ToolOutput};

pub struct ExitPlanModeTool;

#[async_trait::async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "exit_plan_mode"
    }

    fn description(&self) -> &str {
        "Use this only in plan mode, once you have researched the task and are ready to present \
         your implementation plan for the user's approval. Pass the plan as concise markdown \
         (what will change, where, in what order). Do not use it to ask questions or for \
         research tasks that don't end in code changes. If the user rejects the plan you stay \
         in plan mode and should revise it."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "plan": { "type": "string", "description": "The implementation plan, in markdown" }
            },
            "required": ["plan"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolOutput {
        match input.args.get("plan").and_then(|v| v.as_str()) {
            Some(plan) if !plan.trim().is_empty() => ToolOutput::ok(
                "The user approved your plan and plan mode is over. You can now make changes; \
                 start by updating your todo list if the plan has several steps.",
            ),
            Some(_) => ToolOutput::err("the plan must not be empty"),
            None => ToolOutput::err("missing required argument: plan"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(args: serde_json::Value) -> ToolOutput {
        let args = args
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        futures::executor::block_on(ExitPlanModeTool.execute(ToolInput {
            name: "exit_plan_mode".into(),
            args,
        }))
    }

    #[test]
    fn requires_a_non_empty_plan() {
        assert!(call(serde_json::json!({}))
            .data
            .contains("missing required argument"));
        assert!(call(serde_json::json!({"plan": "  "}))
            .data
            .contains("must not be empty"));
        let ok = call(serde_json::json!({"plan": "1. do it"}));
        assert!(ok.success);
        assert!(ok.data.contains("approved"));
    }
}
