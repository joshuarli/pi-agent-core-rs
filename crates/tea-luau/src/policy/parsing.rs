//! Policy declaration and pre-tool decision parsing.

use super::{PolicyError, PolicyTool};
use mlua::{Function, Table, Value};
use tea_core::hooks::BeforeToolCall;
use tea_core::tool::ToolExecutionMode;
use tea_protocol::JsonValue;
use std::collections::BTreeSet;

pub(super) fn parse_declaration(
    declaration: &Table,
) -> Result<(String, Vec<PolicyTool>, Option<Function>), PolicyError> {
    let system_prompt_append = declaration
        .get::<String>("system_prompt_append")
        .map_err(contract_error)?;
    let before_tool_call = declaration
        .get::<Option<Function>>("before_tool_call")
        .map_err(contract_error)?;
    let Some(declared_tools) = declaration
        .get::<Option<Table>>("tools")
        .map_err(contract_error)?
    else {
        return Ok((system_prompt_append, Vec::new(), before_tool_call));
    };

    let mut names = BTreeSet::new();
    let mut tools = Vec::new();
    for declared_tool in declared_tools.sequence_values::<Table>() {
        let declared_tool = declared_tool.map_err(contract_error)?;
        let tool = parse_tool(&declared_tool)?;
        if !names.insert(tool.name.clone()) {
            return Err(PolicyError::Contract {
                message: format!("tools contains duplicate name {:?}", tool.name),
            });
        }
        tools.push(tool);
    }
    Ok((system_prompt_append, tools, before_tool_call))
}

fn parse_tool(declaration: &Table) -> Result<PolicyTool, PolicyError> {
    let name = required_field(declaration, "name")?;
    let description = required_field(declaration, "description")?;
    let capability = required_field(declaration, "capability")?;
    let schema_json = required_field(declaration, "schema_json")?;
    let handler_source = declaration
        .get::<Option<String>>("handler_source")
        .map_err(contract_error)?;
    for (field, value) in [
        ("name", name.as_str()),
        ("description", description.as_str()),
        ("capability", capability.as_str()),
        ("schema_json", schema_json.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(PolicyError::Contract {
                message: format!("tool field {field:?} must not be empty"),
            });
        }
    }
    let execution_mode = match required_field(declaration, "execution_mode")?.as_str() {
        "sequential" => ToolExecutionMode::Sequential,
        "parallel" => ToolExecutionMode::Parallel,
        value => {
            return Err(PolicyError::Contract {
                message: format!(
                    "tool {name:?} has invalid execution_mode {value:?}; expected sequential or parallel"
                ),
            });
        }
    };
    let schema = JsonValue::parse(&schema_json).map_err(|error| PolicyError::Contract {
        message: format!("tool {name:?} schema_json is invalid: {error}"),
    })?;
    if handler_source
        .as_deref()
        .is_some_and(|source| source.trim().is_empty())
    {
        return Err(PolicyError::Contract {
            message: format!("tool {name:?} handler_source must not be empty when declared"),
        });
    }
    Ok(PolicyTool {
        name,
        description,
        schema,
        capability,
        execution_mode,
        handler_source,
    })
}

fn required_field(declaration: &Table, name: &str) -> Result<String, PolicyError> {
    declaration
        .get::<String>(name)
        .map_err(|error| PolicyError::Contract {
            message: format!("tool field {name:?} is required and must be a string: {error}"),
        })
}

pub(super) fn parse_decision(value: Value) -> Result<BeforeToolCall, PolicyError> {
    match value {
        Value::String(value) if value.to_str().map_err(runtime_error)?.as_ref() == "allow" => {
            Ok(BeforeToolCall::Allow)
        }
        Value::Table(value) => {
            let action: String = value.get("action").map_err(contract_error)?;
            let reason: String = value.get("reason").map_err(contract_error)?;
            if reason.trim().is_empty() {
                return Err(PolicyError::Contract {
                    message: "before_tool_call denial reason must not be empty".to_owned(),
                });
            }
            match action.as_str() {
                "block" => Ok(BeforeToolCall::Block { reason }),
                "terminate" => Ok(BeforeToolCall::Terminate { reason }),
                _ => Err(PolicyError::Contract {
                    message: format!(
                        "before_tool_call action {action:?} must be block or terminate"
                    ),
                }),
            }
        }
        _ => Err(PolicyError::Contract {
            message: "before_tool_call must return \"allow\" or { action, reason }".to_owned(),
        }),
    }
}

pub(super) fn runtime_error(error: mlua::Error) -> PolicyError {
    PolicyError::Runtime {
        message: error.to_string(),
    }
}

fn contract_error(error: mlua::Error) -> PolicyError {
    PolicyError::Contract {
        message: error.to_string(),
    }
}
