//! Dependency-free JSON Schema descriptions and schema adapter contracts.
//!
//! The schema model is deliberately small.  It is sufficient for protocol
//! boundaries and tool declarations while leaving draft/version-specific
//! keywords to a transport adapter.  It must not be mistaken for a complete
//! JSON Schema validator.

use std::collections::{BTreeMap, BTreeSet};

use crate::json::JsonValue;

/// The primitive shape described by a [`JsonSchema`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaType {
    /// Any JSON value.
    Any,
    /// JSON `null`.
    Null,
    /// JSON boolean.
    Boolean,
    /// JSON integer.
    Integer,
    /// JSON number, including integers.
    Number,
    /// JSON string.
    String,
    /// JSON array.
    Array,
    /// JSON object.
    Object,
}

/// A compact, draft-neutral JSON Schema description.
///
/// The fields are private to keep construction on the builder methods and to
/// leave room for tightening invariants before a wire schema is frozen.  A
/// future adapter may expose additional draft-specific keywords without
/// adding them to this stable core type.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonSchema {
    schema_type: SchemaType,
    title: Option<String>,
    description: Option<String>,
    properties: BTreeMap<String, JsonSchema>,
    required: BTreeSet<String>,
    items: Option<Box<JsonSchema>>,
    additional_properties: bool,
    enum_values: Vec<JsonValue>,
}

impl JsonSchema {
    /// Start a schema for `schema_type`.
    pub fn new(schema_type: SchemaType) -> Self {
        Self {
            schema_type,
            title: None,
            description: None,
            properties: BTreeMap::new(),
            required: BTreeSet::new(),
            items: None,
            additional_properties: true,
            enum_values: Vec::new(),
        }
    }

    /// Return the primitive shape of this schema.
    pub const fn schema_type(&self) -> SchemaType {
        self.schema_type
    }

    /// Return the optional human-facing title.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Return the optional human-facing description.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Return object properties in deterministic key order.
    pub fn properties(&self) -> &BTreeMap<String, JsonSchema> {
        &self.properties
    }

    /// Return the set of required object property names.
    pub fn required(&self) -> &BTreeSet<String> {
        &self.required
    }

    /// Return the item schema for an array, if one was declared.
    pub fn items(&self) -> Option<&JsonSchema> {
        self.items.as_deref()
    }

    /// Whether unspecified object properties are allowed.
    pub const fn additional_properties(&self) -> bool {
        self.additional_properties
    }

    /// Return explicitly enumerated values, if any.
    pub fn enum_values(&self) -> &[JsonValue] {
        &self.enum_values
    }

    /// Set a title and return the modified schema.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set a description and return the modified schema.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Add an object property and return the modified schema.
    pub fn with_property(mut self, name: impl Into<String>, schema: JsonSchema) -> Self {
        self.properties.insert(name.into(), schema);
        self
    }

    /// Mark an object property as required and return the modified schema.
    pub fn with_required(mut self, name: impl Into<String>) -> Self {
        self.required.insert(name.into());
        self
    }

    /// Set the item schema for an array and return the modified schema.
    pub fn with_items(mut self, schema: JsonSchema) -> Self {
        self.items = Some(Box::new(schema));
        self
    }

    /// Configure whether unspecified object properties are accepted.
    pub fn with_additional_properties(mut self, allowed: bool) -> Self {
        self.additional_properties = allowed;
        self
    }

    /// Restrict the value to the supplied JSON values.
    pub fn with_enum_values(mut self, values: impl IntoIterator<Item = JsonValue>) -> Self {
        self.enum_values = values.into_iter().collect();
        self
    }
}

/// A type that can publish its protocol schema to an adapter.
pub trait SchemaAdapter {
    /// Return a draft-neutral schema for this type.
    fn schema() -> JsonSchema;
}

impl SchemaAdapter for JsonValue {
    fn schema() -> JsonSchema {
        JsonSchema::new(SchemaType::Any)
    }
}

/// Return the schema advertised by `T`.
pub fn schema_for<T: SchemaAdapter>() -> JsonSchema {
    T::schema()
}

#[cfg(test)]
mod tests {
    use super::{JsonSchema, SchemaType};

    #[test]
    fn schema_builder_preserves_object_contract() {
        let schema = JsonSchema::new(SchemaType::Object)
            .with_property("name", JsonSchema::new(SchemaType::String))
            .with_required("name")
            .with_additional_properties(false);

        assert!(schema.properties().contains_key("name"));
        assert!(schema.required().contains("name"));
        assert!(!schema.additional_properties());
    }
}
