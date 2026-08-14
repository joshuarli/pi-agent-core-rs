//! Durable protocol values shared by event producers and consumers.
//!
//! These types intentionally describe meaning rather than a provider's wire
//! format.  IDs are opaque newtypes so a request ID cannot accidentally be
//! passed where a tool-call ID is expected.  Message content is similarly
//! explicit about tool calls and results; adapter crates own provider-specific
//! serialization.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::json::JsonValue;

macro_rules! identifier {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Construct an identifier.  Empty identifiers are rejected at
            /// the protocol boundary so an absent value remains distinguishable.
            pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                if value.is_empty() {
                    return Err(IdentifierError::Empty {
                        kind: stringify!($name),
                    });
                }
                Ok(Self(value))
            }

            /// Borrow the stable textual representation.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the newtype and return its textual representation.
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdentifierError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdentifierError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

identifier!(
    /// Identifies a client request crossing the protocol boundary.
    RequestId
);
identifier!(
    /// Identifies a conversation or session.
    ConversationId
);
identifier!(
    /// Identifies a run of the agent state machine.
    RunId
);
identifier!(
    /// Identifies a message in a conversation.
    MessageId
);
identifier!(
    /// Identifies a model or model alias selected for a request.
    ModelId
);
identifier!(
    /// Identifies one model response stream.
    ModelResponseId
);
identifier!(
    /// Identifies a tool invocation.
    ToolCallId
);

/// Error returned when an opaque protocol identifier cannot be constructed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentifierError {
    /// The identifier was empty.
    Empty {
        /// The identifier type that rejected the value.
        kind: &'static str,
    },
}

impl fmt::Display for IdentifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { kind } => write!(formatter, "{kind} cannot be empty"),
        }
    }
}

impl Error for IdentifierError {}

/// Milliseconds since the Unix epoch, as supplied by the event producer.
///
/// This is an opaque transport value rather than a claim that every producer
/// has a synchronized wall clock.  Ordering should use event sequence numbers
/// when available.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TimestampMillis(pub u64);

/// The semantic author of a message.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MessageRole {
    /// System or developer instructions.
    System,
    /// Human or external caller input.
    User,
    /// Model-authored output.
    Assistant,
    /// Output returned by a tool invocation.
    Tool,
}

/// A message body part in the provider-neutral protocol model.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentPart {
    /// Human-readable text.
    Text(String),
    /// Model reasoning content, when a provider exposes it separately.
    ///
    /// Whether this content is persisted or shown is a policy decision above
    /// this crate; the protocol only preserves its distinction from text.
    Reasoning(String),
    /// A request for a tool invocation.
    ToolCall {
        /// Invocation identity.
        call_id: ToolCallId,
        /// Registered tool name.
        name: String,
        /// Structured invocation arguments.
        arguments: JsonValue,
    },
    /// A tool's result for an earlier invocation.
    ToolResult {
        /// Invocation identity this result settles.
        call_id: ToolCallId,
        /// Structured result content.
        content: JsonValue,
        /// Whether the tool failed while producing this result.
        is_error: bool,
    },
}

/// A provider-neutral conversation message.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// Optional stable identity.  Some transient deltas precede allocation of
    /// a durable message ID.
    pub id: Option<MessageId>,
    /// Semantic author of this message.
    pub role: MessageRole,
    /// Ordered body parts.
    pub content: Vec<ContentPart>,
    /// Non-semantic annotations retained by the protocol boundary.
    pub metadata: BTreeMap<String, String>,
}

impl Message {
    /// Create a message with no ID or metadata.
    pub fn new(role: MessageRole, content: impl IntoIterator<Item = ContentPart>) -> Self {
        Self {
            id: None,
            role,
            content: content.into_iter().collect(),
            metadata: BTreeMap::new(),
        }
    }

    /// Create a text-only message with no ID or metadata.
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self::new(role, [ContentPart::Text(text.into())])
    }
}

/// Token accounting attached to a completed model response.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenUsage {
    /// Tokens sent to the model.
    pub input_tokens: u64,
    /// Tokens returned by the model.
    pub output_tokens: u64,
    /// Optional provider-reported cached input tokens.
    pub cached_input_tokens: Option<u64>,
}

impl TokenUsage {
    /// Return the sum of input and output tokens represented here.
    pub const fn total_tokens(self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}
