use crate::types::Session;
use crate::provider::{ContentBlock, Message, Role};
use crate::tools::ToolSpec;

pub struct ContextManager {
    #[allow(dead_code)]
    max_tokens: usize,
    system_prompt: String,
}

impl ContextManager {
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens, system_prompt: String::new() }
    }

    pub fn set_system_prompt(&mut self, prompt: String) {
        self.system_prompt = prompt;
    }

    pub fn build_messages(&self, session: &Session, user_input: &str, tools: &[ToolSpec]) -> Vec<Message> {
        let mut messages = Vec::new();

        if !self.system_prompt.is_empty() {
            let tool_descriptions: Vec<String> = tools.iter().map(|t| {
                format!("- `{}`: {} (input: {})", t.name, t.description, t.input_schema)
            }).collect();

            let system_text = format!(
                "{}\n\n## Available Tools\n\n{}",
                self.system_prompt,
                tool_descriptions.join("\n"),
            );

            messages.push(Message {
                role: Role::System,
                content: vec![ContentBlock::Text { text: system_text }],
            });
        }

        for turn in &session.turns {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: turn.user_message.clone() }],
            });

            if let Some(ref reply) = turn.assistant_message {
                messages.push(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text { text: reply.clone() }],
                });
            }

            for tc in &turn.tool_calls {
                messages.push(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: format!("tool_{}", tc.tool_name),
                        name: tc.tool_name.clone(),
                        input: tc.input.clone(),
                    }],
                });

                messages.push(Message {
                    role: Role::Tool,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: format!("tool_{}", tc.tool_name),
                        content: tc.output.clone(),
                        is_error: Some(!tc.success),
                    }],
                });
            }
        }

        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: user_input.to_string() }],
        });

        messages
    }
}
