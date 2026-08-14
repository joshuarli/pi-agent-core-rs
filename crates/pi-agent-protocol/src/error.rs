//! Errors that are safe to expose across protocol boundaries.
//!
//! Protocol errors retain a category and structured context instead of forcing
//! callers to parse a display string.  Runtime-specific errors should be
//! translated at the adapter boundary; this crate does not know about HTTP,
//! provider SDKs, task executors, or VM error types.

use std::error::Error;
use std::fmt;

/// Broad classification for a [`ProtocolError`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ErrorCategory {
    /// The caller supplied a malformed or unsupported value.
    InvalidInput,
    /// A stream or lifecycle event violated its ordering contract.
    InvalidTransition,
    /// Work was intentionally stopped by cancellation.
    Cancelled,
    /// An implementation or transport feature is not available.
    Unsupported,
    /// A lower layer failed and was translated to a protocol-safe message.
    Internal,
}

/// Structured error crossing the stable protocol boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    /// A field or value is not valid for the requested operation.
    InvalidInput {
        /// The logical field or boundary that rejected the value.
        field: String,
        /// Human-readable explanation suitable for logs and diagnostics.
        message: String,
    },
    /// An event or operation was received in an invalid state.
    InvalidTransition {
        /// State observed by the validator.
        state: String,
        /// Event or operation that could not be accepted.
        event: String,
    },
    /// Work stopped because its cancellation scope was cancelled.
    Cancelled {
        /// Optional cancellation reason supplied by the owner.
        reason: Option<String>,
    },
    /// A protocol feature is not implemented by this adapter.
    Unsupported {
        /// Feature name or boundary description.
        feature: String,
    },
    /// A lower-level failure that cannot be represented more specifically.
    Internal {
        /// Redacted, stable diagnostic text.
        message: String,
    },
}

impl ProtocolError {
    /// Return the stable category without requiring callers to match variants.
    pub const fn category(&self) -> ErrorCategory {
        match self {
            Self::InvalidInput { .. } => ErrorCategory::InvalidInput,
            Self::InvalidTransition { .. } => ErrorCategory::InvalidTransition,
            Self::Cancelled { .. } => ErrorCategory::Cancelled,
            Self::Unsupported { .. } => ErrorCategory::Unsupported,
            Self::Internal { .. } => ErrorCategory::Internal,
        }
    }

    /// Construct an invalid-input error without exposing field storage.
    pub fn invalid_input(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidInput {
            field: field.into(),
            message: message.into(),
        }
    }

    /// Construct an unsupported-feature error.
    pub fn unsupported(feature: impl Into<String>) -> Self {
        Self::Unsupported {
            feature: feature.into(),
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field, message } => {
                write!(formatter, "invalid input for {field}: {message}")
            }
            Self::InvalidTransition { state, event } => {
                write!(
                    formatter,
                    "invalid transition: event {event} in state {state}"
                )
            }
            Self::Cancelled {
                reason: Some(reason),
            } => write!(formatter, "cancelled: {reason}"),
            Self::Cancelled { reason: None } => formatter.write_str("cancelled"),
            Self::Unsupported { feature } => {
                write!(formatter, "unsupported protocol feature: {feature}")
            }
            Self::Internal { message } => write!(formatter, "internal protocol error: {message}"),
        }
    }
}

impl Error for ProtocolError {}

impl From<crate::cancellation::Cancelled> for ProtocolError {
    fn from(cancelled: crate::cancellation::Cancelled) -> Self {
        Self::Cancelled {
            reason: cancelled.reason().map(str::to_owned),
        }
    }
}

impl From<crate::json::JsonError> for ProtocolError {
    fn from(error: crate::json::JsonError) -> Self {
        Self::InvalidInput {
            field: "json".into(),
            message: error.to_string(),
        }
    }
}

impl From<crate::model_stream::ModelStreamError> for ProtocolError {
    fn from(error: crate::model_stream::ModelStreamError) -> Self {
        Self::InvalidTransition {
            state: "model_stream".into(),
            event: error.to_string(),
        }
    }
}

impl From<crate::types::IdentifierError> for ProtocolError {
    fn from(error: crate::types::IdentifierError) -> Self {
        Self::InvalidInput {
            field: "identifier".into(),
            message: error.to_string(),
        }
    }
}
