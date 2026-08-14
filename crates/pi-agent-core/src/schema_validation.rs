//! Private JSON Schema validation adapter.
//!
//! The public kernel boundary uses `pi_agent_protocol::JsonValue` and
//! `SerializedJson`. `jsonschema` and `serde_json` stay in this module so a
//! validator replacement cannot alter the public API.

use crate::error::ToolError;
use crate::state::SerializedJson;
use pi_agent_protocol::JsonValue;

pub(crate) fn validate_tool_arguments(
    tool_name: &str,
    schema: &JsonValue,
    arguments: &SerializedJson,
) -> Result<(), ToolError> {
    let schema = parse_schema_json(tool_name, schema)?;
    let arguments =
        serde_json::from_str(arguments.as_str()).map_err(|error| ToolError::InvalidArguments {
            tool: tool_name.to_owned(),
            message: format!("tool-call arguments are not valid JSON: {error}"),
        })?;
    let validator =
        jsonschema::validator_for(&schema).map_err(|error| ToolError::InvalidArguments {
            tool: tool_name.to_owned(),
            message: format!("tool schema is invalid: {error}"),
        })?;
    validator
        .validate(&arguments)
        .map_err(|error| ToolError::InvalidArguments {
            tool: tool_name.to_owned(),
            message: error.to_string(),
        })
}

fn parse_schema_json(tool_name: &str, schema: &JsonValue) -> Result<serde_json::Value, ToolError> {
    let schema = schema
        .to_json_string()
        .map_err(|error| ToolError::InvalidArguments {
            tool: tool_name.to_owned(),
            message: format!("tool schema cannot be encoded: {error}"),
        })?;
    serde_json::from_str(&schema).map_err(|error| ToolError::InvalidArguments {
        tool: tool_name.to_owned(),
        message: format!("tool schema cannot be decoded: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::validate_tool_arguments;
    use crate::state::SerializedJson;
    use pi_agent_protocol::JsonValue;

    #[test]
    fn validator_accepts_matching_arguments_and_rejects_a_missing_required_property() {
        let schema = JsonValue::parse(
            r#"{"type":"object","required":["text"],"properties":{"text":{"type":"string"}}}"#,
        )
        .expect("schema JSON");

        validate_tool_arguments("echo", &schema, &SerializedJson::new(r#"{"text":"hello"}"#))
            .expect("matching arguments");
        let error = validate_tool_arguments("echo", &schema, &SerializedJson::new("{}"))
            .expect_err("required property must be enforced");
        assert!(matches!(
            error,
            crate::error::ToolError::InvalidArguments { .. }
        ));
    }
}
