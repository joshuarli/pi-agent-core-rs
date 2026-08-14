//! OpenAI-compatible context conversion for the concrete HTTP adapters.

use crate::error::HookError;
use crate::hooks::{AfterToolCall, BeforeToolCall, ContextEnvelope, HookSet, NextTurn};
use crate::json::JsonValue;
use crate::state::Message;

/// Convert the core transcript into an OpenAI Chat Completions message array.
///
/// OpenRouter and Command Code both consume this host-produced context shape. The hook remains
/// explicit because the core's default [`crate::hooks::NoHooks`] representation is diagnostic
/// Rust text, not a provider wire format.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenAiContextHook;

impl HookSet for OpenAiContextHook {
    fn before_tool_call(&self, _call: &crate::tool::ToolCall) -> Result<BeforeToolCall, HookError> {
        Ok(BeforeToolCall::Allow)
    }

    fn after_tool_call(
        &self,
        _call: &crate::tool::ToolCall,
        _result: &crate::tool::ToolResult,
    ) -> Result<AfterToolCall, HookError> {
        Ok(AfterToolCall::default())
    }

    fn transform_context(&self, context: ContextEnvelope) -> Result<ContextEnvelope, HookError> {
        Ok(context)
    }

    fn convert_to_llm(&self, context: ContextEnvelope) -> Result<String, HookError> {
        let messages = context
            .messages
            .iter()
            .map(openai_message)
            .collect::<Result<Vec<_>, _>>()?;
        JsonValue::Array(messages)
            .to_json_string()
            .map_err(|error| HookError::new("convert_to_llm", error.to_string()))
    }

    fn should_stop_after_turn(&self, _context: &ContextEnvelope) -> Result<bool, HookError> {
        Ok(false)
    }

    fn prepare_next_turn(&self, _context: ContextEnvelope) -> Result<NextTurn, HookError> {
        Ok(NextTurn::default())
    }
}

fn openai_message(message: &Message) -> Result<JsonValue, HookError> {
    match message {
        Message::User { content, .. } => Ok(JsonValue::object([
            ("role", JsonValue::from("user")),
            ("content", JsonValue::from(content.clone())),
        ])),
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let calls = tool_calls
                .iter()
                .map(|call| {
                    JsonValue::object([
                        ("id", JsonValue::from(call.id.as_str())),
                        ("type", JsonValue::from("function")),
                        (
                            "function",
                            JsonValue::object([
                                ("name", JsonValue::from(call.name.clone())),
                                ("arguments", JsonValue::from(call.arguments.as_str())),
                            ]),
                        ),
                    ])
                })
                .collect::<Vec<_>>();
            Ok(JsonValue::object([
                ("role", JsonValue::from("assistant")),
                (
                    "content",
                    if content.is_empty() {
                        JsonValue::Null
                    } else {
                        JsonValue::from(content.clone())
                    },
                ),
                ("tool_calls", JsonValue::Array(calls)),
            ]))
        }
        Message::ToolResult {
            tool_call_id,
            content,
            ..
        } => Ok(JsonValue::object([
            ("role", JsonValue::from("tool")),
            ("tool_call_id", JsonValue::from(tool_call_id.as_str())),
            ("content", JsonValue::from(content.clone())),
        ])),
    }
}
