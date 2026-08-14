//! Private JSON Schema validation adapter.
//!
//! The public kernel boundary uses `pi_agent_protocol::JsonValue` and
//! `SerializedJson`. `jsonschema` and `serde_json` stay in this module so a
//! validator replacement cannot alter the public API.

use crate::error::ToolError;
use crate::state::SerializedJson;
use jsonschema::error::ValidationErrorKind;
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
    let errors = validator
        .iter_errors(&arguments)
        .map(format_validation_error)
        .collect::<Vec<_>>();
    if errors.is_empty() {
        return Ok(());
    }

    // Pi returns a stable, tool-specific diagnostic rather than leaking the
    // particular JSON Schema engine's display text.  Keep that boundary here:
    // changing the private validator must not change model-visible recovery
    // context for the selected SDK subset.
    let received =
        serde_json::to_string_pretty(&arguments).map_err(|error| ToolError::InvalidArguments {
            tool: tool_name.to_owned(),
            message: format!("tool-call arguments cannot be rendered: {error}"),
        })?;
    Err(ToolError::InvalidArguments {
        tool: tool_name.to_owned(),
        message: format!(
            "Validation failed for tool {tool_name:?}:\n{}\n\nReceived arguments:\n{received}",
            errors
                .iter()
                .map(|error| format!("  - {error}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    })
}

fn format_validation_error(error: jsonschema::ValidationError<'_>) -> String {
    let pointer = error.instance_path().as_str();
    let path = match error.kind() {
        ValidationErrorKind::Required { property } => {
            let property = property.as_str().unwrap_or_default();
            join_validation_path(pointer, property)
        }
        _ => pointer.trim_start_matches('/').replace('/', "."),
    };
    let path = if path.is_empty() { "root" } else { &path };
    let message = match error.kind() {
        ValidationErrorKind::Required { property } => format!(
            "must have required properties {}",
            property.as_str().unwrap_or_default()
        ),
        _ => error.to_string(),
    };
    format!("{path}: {message}")
}

fn join_validation_path(pointer: &str, property: &str) -> String {
    let base = pointer.trim_start_matches('/').replace('/', ".");
    if base.is_empty() {
        property.to_owned()
    } else {
        format!("{base}.{property}")
    }
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
        assert_eq!(
            error,
            crate::error::ToolError::InvalidArguments {
                tool: "echo".into(),
                message: "Validation failed for tool \"echo\":\n  - text: must have required properties text\n\nReceived arguments:\n{}".into(),
            }
        );
    }
}
