//! Capability manifest construction, JSON encoding, and decoding.

use super::domain::{
    operation_name, AgentOperation, CapabilityError, CapabilityGrant, CapabilityModule,
    CapabilityOperation, CapabilityRequest, JsonOperation, McpOperation, McpPermission,
    TaskOperation, TimeOperation, TraceOperation, WorldOperation, CAPABILITY_ABI_VERSION,
};
use tea_protocol::{JsonAdapter, JsonError, JsonNumber, JsonValue};
use std::collections::BTreeMap;

/// A complete, deterministic capability grant set for one VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityManifest {
    abi_version: u64,
    grants: BTreeMap<CapabilityModule, CapabilityGrant>,
}

impl CapabilityManifest {
    /// Construct a manifest from one optional grant per virtual module.
    pub fn new(grants: impl IntoIterator<Item = CapabilityGrant>) -> Result<Self, CapabilityError> {
        let mut by_module = BTreeMap::new();
        for grant in grants {
            let module = grant.module;
            if by_module.insert(module, grant).is_some() {
                return Err(CapabilityError::DuplicateGrant { module });
            }
        }
        Ok(Self {
            abi_version: CAPABILITY_ABI_VERSION,
            grants: by_module,
        })
    }

    /// Construct an empty manifest with no authority.
    pub fn empty() -> Self {
        Self {
            abi_version: CAPABILITY_ABI_VERSION,
            grants: BTreeMap::new(),
        }
    }

    /// Return the ABI version encoded by this manifest.
    pub const fn abi_version(&self) -> u64 {
        self.abi_version
    }

    /// Return the grant for a module, if that module has authority.
    pub fn grant(&self, module: CapabilityModule) -> Option<&CapabilityGrant> {
        self.grants.get(&module)
    }

    /// Return all grants in module-name order.
    pub fn grants(&self) -> impl Iterator<Item = &CapabilityGrant> {
        self.grants.values()
    }

    /// Parse and validate a complete manifest from JSON text.
    pub fn parse_json(input: &str) -> Result<Self, CapabilityError> {
        let value = JsonValue::parse(input).map_err(CapabilityError::Json)?;
        Self::from_json(&value)
    }

    /// Encode this manifest as canonical, deterministic JSON text.
    pub fn to_json_string(&self) -> Result<String, CapabilityError> {
        self.to_json()
            .and_then(|value| value.to_json_string().map_err(CapabilityError::Json))
    }

    /// Check whether a request is authorized by this manifest.
    pub fn check(&self, request: &CapabilityRequest) -> Result<(), CapabilityError> {
        let module = request.operation.module();
        if self
            .grant(module)
            .is_some_and(|grant| grant.allows(&request.operation))
        {
            Ok(())
        } else {
            Err(CapabilityError::Denied {
                module,
                operation: operation_name(&request.operation),
            })
        }
    }

    fn from_json(value: &JsonValue) -> Result<Self, CapabilityError> {
        let object = expect_object(value, "manifest")?;
        let abi_version = required_u64(object, "abi_version")?;
        if abi_version != CAPABILITY_ABI_VERSION {
            return Err(CapabilityError::UnsupportedAbiVersion {
                expected: CAPABILITY_ABI_VERSION,
                actual: abi_version,
            });
        }

        let known = [
            "abi_version",
            "agent",
            "world",
            "trace",
            "task",
            "json",
            "time",
        ];
        reject_unknown_keys(object, &known, "manifest")?;

        let mut grants = Vec::new();
        for module in [
            CapabilityModule::Agent,
            CapabilityModule::World,
            CapabilityModule::Trace,
            CapabilityModule::Task,
            CapabilityModule::Json,
            CapabilityModule::Time,
        ] {
            if let Some(value) = object.get(module.json_key()) {
                grants.push(parse_grant(module, value)?);
            }
        }
        let mut manifest = Self::new(grants)?;
        manifest.abi_version = abi_version;
        Ok(manifest)
    }

    fn to_json(&self) -> Result<JsonValue, CapabilityError> {
        let mut object = BTreeMap::new();
        object.insert(
            "abi_version".to_owned(),
            JsonValue::Number(JsonNumber::Unsigned(self.abi_version)),
        );
        for grant in self.grants.values() {
            let operations = grant
                .operations
                .iter()
                .map(operation_to_json)
                .collect::<Result<Vec<_>, _>>()?;
            object.insert(
                grant.module.json_key().to_owned(),
                JsonValue::Array(operations),
            );
        }
        Ok(JsonValue::Object(object))
    }
}

impl JsonAdapter for CapabilityManifest {
    fn to_json(&self) -> Result<JsonValue, JsonError> {
        CapabilityManifest::to_json(self).map_err(|error| JsonError::Message(error.to_string()))
    }

    fn from_json(value: &JsonValue) -> Result<Self, JsonError> {
        CapabilityManifest::from_json(value).map_err(|error| JsonError::Message(error.to_string()))
    }
}

fn parse_grant(
    module: CapabilityModule,
    value: &JsonValue,
) -> Result<CapabilityGrant, CapabilityError> {
    let values = expect_array(value, module.json_key())?;
    let mut operations = Vec::new();
    for (index, value) in values.iter().enumerate() {
        operations.push(parse_operation(
            module,
            value,
            &format!("{}[{index}]", module.json_key()),
        )?);
    }
    CapabilityGrant::new(module, operations)
}

fn parse_operation(
    module: CapabilityModule,
    value: &JsonValue,
    path: &str,
) -> Result<CapabilityOperation, CapabilityError> {
    match value {
        JsonValue::String(name) => parse_simple_operation(module, name, path),
        JsonValue::Object(object) if module == CapabilityModule::World => {
            reject_unknown_keys(object, &["mcp"], path)?;
            let mcp = object
                .get("mcp")
                .ok_or_else(|| malformed(path, "missing mcp"))?;
            parse_mcp_operation(mcp, path).map(CapabilityOperation::World)
        }
        _ => Err(malformed(path, "expected a known operation string")),
    }
}

fn parse_simple_operation(
    module: CapabilityModule,
    name: &str,
    path: &str,
) -> Result<CapabilityOperation, CapabilityError> {
    let operation = match module {
        CapabilityModule::Agent => AgentOperation::parse(name).map(CapabilityOperation::Agent),
        CapabilityModule::World => match name {
            "fs.read" => Some(CapabilityOperation::World(WorldOperation::FsRead)),
            "fs.write" => Some(CapabilityOperation::World(WorldOperation::FsWrite)),
            "exec" => Some(CapabilityOperation::World(WorldOperation::Exec)),
            _ => None,
        },
        CapabilityModule::Trace => TraceOperation::Emit
            .as_str()
            .eq(name)
            .then_some(CapabilityOperation::Trace(TraceOperation::Emit)),
        CapabilityModule::Task => TaskOperation::parse(name).map(CapabilityOperation::Task),
        CapabilityModule::Json => JsonOperation::parse(name).map(CapabilityOperation::Json),
        CapabilityModule::Time => TimeOperation::parse(name).map(CapabilityOperation::Time),
    };
    operation.ok_or_else(|| malformed(path, format!("unknown operation {name:?} for {module}")))
}

fn parse_mcp_operation(value: &JsonValue, path: &str) -> Result<WorldOperation, CapabilityError> {
    let object = expect_object(value, &format!("{path}.mcp"))?;
    reject_unknown_keys(
        object,
        &["server", "operation", "target"],
        &format!("{path}.mcp"),
    )?;
    let server = required_string(object, "server", &format!("{path}.mcp"))?;
    let operation_name = required_string(object, "operation", &format!("{path}.mcp"))?;
    let operation = McpOperation::parse(&operation_name).ok_or_else(|| {
        malformed(
            format!("{path}.mcp.operation"),
            format!("unknown MCP operation {operation_name:?}"),
        )
    })?;
    let target = object
        .get("target")
        .map(|value| expect_string(value, &format!("{path}.mcp.target")))
        .transpose()?
        .map(str::to_owned);
    McpPermission::new(server, operation, target)
        .map(WorldOperation::Mcp)
        .map_err(|error| malformed(path, error.to_string()))
}

fn operation_to_json(operation: &CapabilityOperation) -> Result<JsonValue, CapabilityError> {
    let _ = operation.sort_key();
    Ok(match operation {
        CapabilityOperation::Agent(operation) => JsonValue::String(operation.as_str().to_owned()),
        CapabilityOperation::World(WorldOperation::FsRead) => {
            JsonValue::String("fs.read".to_owned())
        }
        CapabilityOperation::World(WorldOperation::FsWrite) => {
            JsonValue::String("fs.write".to_owned())
        }
        CapabilityOperation::World(WorldOperation::Exec) => JsonValue::String("exec".to_owned()),
        CapabilityOperation::World(WorldOperation::Mcp(permission)) => {
            let mut mcp = BTreeMap::new();
            mcp.insert(
                "server".to_owned(),
                JsonValue::String(permission.server.clone()),
            );
            mcp.insert(
                "operation".to_owned(),
                JsonValue::String(permission.operation.as_str().to_owned()),
            );
            if let Some(target) = permission.target.as_deref() {
                mcp.insert("target".to_owned(), JsonValue::String(target.to_owned()));
            }
            JsonValue::object([("mcp", JsonValue::Object(mcp))])
        }
        CapabilityOperation::Trace(operation) => JsonValue::String(operation.as_str().to_owned()),
        CapabilityOperation::Task(operation) => JsonValue::String(operation.as_str().to_owned()),
        CapabilityOperation::Json(operation) => JsonValue::String(operation.as_str().to_owned()),
        CapabilityOperation::Time(operation) => JsonValue::String(operation.as_str().to_owned()),
    })
}

fn expect_object<'a>(
    value: &'a JsonValue,
    path: &str,
) -> Result<&'a BTreeMap<String, JsonValue>, CapabilityError> {
    match value {
        JsonValue::Object(object) => Ok(object),
        other => Err(malformed(
            path,
            format!("expected object, got {:?}", other.kind()),
        )),
    }
}

fn expect_array<'a>(
    value: &'a JsonValue,
    path: &str,
) -> Result<&'a Vec<JsonValue>, CapabilityError> {
    match value {
        JsonValue::Array(array) => Ok(array),
        other => Err(malformed(
            path,
            format!("expected array, got {:?}", other.kind()),
        )),
    }
}

fn expect_string<'a>(value: &'a JsonValue, path: &str) -> Result<&'a str, CapabilityError> {
    match value {
        JsonValue::String(value) if !value.trim().is_empty() => Ok(value),
        JsonValue::String(_) => Err(malformed(path, "must not be empty")),
        other => Err(malformed(
            path,
            format!("expected string, got {:?}", other.kind()),
        )),
    }
}

fn required_string(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
    path: &str,
) -> Result<String, CapabilityError> {
    let value = object
        .get(key)
        .ok_or_else(|| malformed(format!("{path}.{key}"), "missing required field"))?;
    Ok(expect_string(value, &format!("{path}.{key}"))?.to_owned())
}

fn required_u64(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<u64, CapabilityError> {
    match object.get(key) {
        Some(JsonValue::Number(JsonNumber::Unsigned(value))) => Ok(*value),
        Some(JsonValue::Number(JsonNumber::Signed(value))) if *value >= 0 => Ok(*value as u64),
        Some(value) => Err(malformed(
            key,
            format!("expected non-negative integer, got {:?}", value.kind()),
        )),
        None => Err(malformed(key, "missing required field")),
    }
}

fn reject_unknown_keys(
    object: &BTreeMap<String, JsonValue>,
    known: &[&str],
    path: &str,
) -> Result<(), CapabilityError> {
    if let Some(key) = object.keys().find(|key| !known.contains(&key.as_str())) {
        return Err(malformed(path, format!("unknown field {key:?}")));
    }
    Ok(())
}

fn malformed(path: impl Into<String>, message: impl Into<String>) -> CapabilityError {
    CapabilityError::Malformed {
        path: path.into(),
        message: message.into(),
    }
}
