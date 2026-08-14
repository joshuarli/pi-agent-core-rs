//! JSON values and narrow Miniserde conversion boundaries.
//!
//! [`JsonValue`] is intentionally an in-memory representation, not a parser
//! or a provider-specific wire representation. The built-in text codec uses
//! Miniserde, while provider and transport adapters may implement
//! [`JsonAdapter`] for their own types. No Serde trait or `serde_json` type is
//! exposed by this crate.

use miniserde::json::{
    Array as MiniArray, Number as MiniNumber, Object as MiniObject, Value as MiniValue,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// A JSON value that can cross the protocol boundary without a serialization
/// dependency in its public API.
///
/// Objects use [`BTreeMap`] so iteration and any eventual canonical encoding
/// are deterministic.  `Float` values must be finite when constructed through
/// [`JsonValue::number`]; JSON does not have representations for NaN or
/// infinity.  This type does not currently parse JSON text; that is an
/// intentional adapter responsibility.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonValue {
    /// The JSON `null` value.
    Null,
    /// A JSON boolean.
    Bool(bool),
    /// A JSON number represented without loss for the common integer forms.
    Number(JsonNumber),
    /// A JSON string.
    String(String),
    /// A JSON array.
    Array(Vec<JsonValue>),
    /// A JSON object with deterministic key ordering.
    Object(BTreeMap<String, JsonValue>),
}

/// Numeric forms supported by [`JsonValue`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JsonNumber {
    /// A signed integer.
    Signed(i64),
    /// An unsigned integer.
    Unsigned(u64),
    /// A finite IEEE-754 number.
    Float(f64),
}

impl JsonValue {
    /// Parse one complete JSON value using Miniserde.
    ///
    /// Miniserde intentionally provides no parse diagnostic. The protocol
    /// preserves that fact rather than inventing unstable text for a wire
    /// error. Callers that need a provider-specific diagnostic add it at their
    /// adapter boundary.
    pub fn parse(input: &str) -> Result<Self, JsonError> {
        let value = miniserde::json::from_str::<MiniValue>(input)
            .map_err(|_| JsonError::Message("invalid JSON".into()))?;
        Self::from_miniserde(value)
    }

    /// Encode this value as canonical JSON text using deterministic object-key
    /// order.
    pub fn to_json_string(&self) -> Result<String, JsonError> {
        Ok(miniserde::json::to_string(&self.to_miniserde()?))
    }

    /// Construct a numeric value, rejecting non-finite floating point values.
    pub fn number(number: JsonNumber) -> Result<Self, JsonError> {
        match number {
            JsonNumber::Float(value) if !value.is_finite() => Err(JsonError::InvalidNumber(
                "JSON numbers must be finite".into(),
            )),
            _ => Ok(Self::Number(number)),
        }
    }

    /// Return the broad JSON kind of this value.
    pub const fn kind(&self) -> JsonKind {
        match self {
            Self::Null => JsonKind::Null,
            Self::Bool(_) => JsonKind::Boolean,
            Self::Number(_) => JsonKind::Number,
            Self::String(_) => JsonKind::String,
            Self::Array(_) => JsonKind::Array,
            Self::Object(_) => JsonKind::Object,
        }
    }

    /// Return an object member, if this value is an object containing `key`.
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(values) => values.get(key),
            _ => None,
        }
    }

    /// Construct an object from key/value pairs.
    pub fn object<I, K>(values: I) -> Self
    where
        I: IntoIterator<Item = (K, JsonValue)>,
        K: Into<String>,
    {
        Self::Object(
            values
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    fn from_miniserde(value: MiniValue) -> Result<Self, JsonError> {
        match value {
            MiniValue::Null => Ok(Self::Null),
            MiniValue::Bool(value) => Ok(Self::Bool(value)),
            MiniValue::Number(MiniNumber::I64(value)) => {
                Ok(Self::Number(JsonNumber::Signed(value)))
            }
            MiniValue::Number(MiniNumber::U64(value)) => {
                Ok(Self::Number(JsonNumber::Unsigned(value)))
            }
            MiniValue::Number(MiniNumber::F64(value)) => Self::number(JsonNumber::Float(value)),
            MiniValue::String(value) => Ok(Self::String(value)),
            MiniValue::Array(values) => values
                .into_iter()
                .map(Self::from_miniserde)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::Array),
            MiniValue::Object(values) => values
                .into_iter()
                .map(|(key, value)| Ok((key, Self::from_miniserde(value)?)))
                .collect::<Result<BTreeMap<_, _>, JsonError>>()
                .map(Self::Object),
        }
    }

    fn to_miniserde(&self) -> Result<MiniValue, JsonError> {
        match self {
            Self::Null => Ok(MiniValue::Null),
            Self::Bool(value) => Ok(MiniValue::Bool(*value)),
            Self::Number(JsonNumber::Signed(value)) => {
                Ok(MiniValue::Number(MiniNumber::I64(*value)))
            }
            Self::Number(JsonNumber::Unsigned(value)) => {
                Ok(MiniValue::Number(MiniNumber::U64(*value)))
            }
            Self::Number(JsonNumber::Float(value)) if value.is_finite() => {
                Ok(MiniValue::Number(MiniNumber::F64(*value)))
            }
            Self::Number(JsonNumber::Float(_)) => Err(JsonError::InvalidNumber(
                "JSON numbers must be finite".into(),
            )),
            Self::String(value) => Ok(MiniValue::String(value.clone())),
            Self::Array(values) => values
                .iter()
                .map(Self::to_miniserde)
                .collect::<Result<MiniArray, _>>()
                .map(MiniValue::Array),
            Self::Object(values) => values
                .iter()
                .map(|(key, value)| Ok((key.clone(), value.to_miniserde()?)))
                .collect::<Result<MiniObject, JsonError>>()
                .map(MiniValue::Object),
        }
    }
}

impl From<bool> for JsonValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for JsonValue {
    fn from(value: i64) -> Self {
        Self::Number(JsonNumber::Signed(value))
    }
}

impl From<u64> for JsonValue {
    fn from(value: u64) -> Self {
        Self::Number(JsonNumber::Unsigned(value))
    }
}

impl From<String> for JsonValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for JsonValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl<T> From<Vec<T>> for JsonValue
where
    T: Into<JsonValue>,
{
    fn from(value: Vec<T>) -> Self {
        Self::Array(value.into_iter().map(Into::into).collect())
    }
}

/// The broad kind of a [`JsonValue`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonKind {
    /// `null`.
    Null,
    /// `true` or `false`.
    Boolean,
    /// Any JSON number.
    Number,
    /// A JSON string.
    String,
    /// A JSON array.
    Array,
    /// A JSON object.
    Object,
}

/// Errors raised by a JSON adapter or by a value invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JsonError {
    /// A value had a different kind than the adapter expected.
    TypeMismatch {
        /// The expected kind.
        expected: JsonKind,
        /// The received kind.
        actual: JsonKind,
    },
    /// A required object member was absent.
    MissingField(String),
    /// A number cannot be represented as valid JSON.
    InvalidNumber(String),
    /// The adapter does not support the requested shape yet.
    Unsupported(String),
    /// An adapter-specific validation error.
    Message(String),
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMismatch { expected, actual } => {
                write!(formatter, "expected JSON {expected:?}, got {actual:?}")
            }
            Self::MissingField(field) => write!(formatter, "missing JSON field {field:?}"),
            Self::InvalidNumber(message) => write!(formatter, "invalid JSON number: {message}"),
            Self::Unsupported(message) => {
                write!(formatter, "unsupported JSON operation: {message}")
            }
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

impl Error for JsonError {}

/// Conversion seam for protocol values and transport-specific JSON types.
///
/// This trait intentionally does not require a provider or transport wire
/// format. Implementations should preserve the semantic shape of the value and
/// return [`JsonError`] for validation failures. The schema half of the
/// contract is provided separately by [`crate::schema::SchemaAdapter`].
pub trait JsonAdapter: Sized {
    /// Convert this value to the dependency-free protocol representation.
    fn to_json(&self) -> Result<JsonValue, JsonError>;

    /// Reconstruct this value from the protocol representation.
    fn from_json(value: &JsonValue) -> Result<Self, JsonError>;
}

impl JsonAdapter for JsonValue {
    fn to_json(&self) -> Result<JsonValue, JsonError> {
        Ok(self.clone())
    }

    fn from_json(value: &JsonValue) -> Result<Self, JsonError> {
        Ok(value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{JsonKind, JsonNumber, JsonValue};

    #[test]
    fn object_keys_are_addressable() {
        let value = JsonValue::object([("answer", JsonValue::from(42_i64))]);

        assert_eq!(
            value.get("answer").map(JsonValue::kind),
            Some(JsonKind::Number)
        );
    }

    #[test]
    fn non_finite_numbers_are_rejected() {
        assert!(JsonValue::number(JsonNumber::Float(f64::NAN)).is_err());
    }

    #[test]
    fn miniserde_codec_preserves_json_shape_and_key_order() {
        let value = JsonValue::parse(r#"{"z":true,"a":[-1,2.5]}"#).expect("valid JSON");

        assert_eq!(
            value.to_json_string().expect("finite values serialize"),
            r#"{"a":[-1,2.5],"z":true}"#
        );
    }
}
