//! Stable protocol contracts shared by the agent kernel and its adapters.
//!
//! This crate deliberately has no runtime or async framework dependency. It
//! describes values and boundaries; executors and provider transports belong
//! in crates above it. Its JSON text codec uses Miniserde, but the types here
//! do not expose provider SDK types, Serde traits, `serde_json` values, or
//! Tokio cancellation tokens. The small adapter traits in [`json`] and
//! [`schema`] are the seam where integrations can be added without changing
//! the kernel-facing protocol.
//!
//! The modules have intentionally narrow ownership:
//!
//! * [`types`] contains durable identifiers and message/value types.
//! * [`events`] contains observable run and operation events.
//! * [`error`] contains errors that can cross a protocol boundary.
//! * [`model_stream`] validates the ordering grammar of model output.
//! * [`cancellation`] contains the std-only cancellation primitive used at
//!   synchronous boundaries.  An async runtime can adapt its notification.
//! * [`json`] contains a stable JSON value tree, a Miniserde text codec, and a
//!   conversion seam.
//! * [`schema`] contains the dependency-free schema description seam.
//!
//! These contracts are a scaffold, not a wire-format promise.  Callers should
//! treat fields and variants marked as provisional in their documentation as
//! subject to review before a protocol version is declared stable.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cancellation;
pub mod error;
pub mod events;
pub mod json;
pub mod model_stream;
pub mod schema;
pub mod types;

pub use cancellation::{CancellationSource, CancellationToken, Cancelled};
pub use error::{ErrorCategory, ProtocolError};
pub use events::{Event, EventEnvelope, EventKind, RunOutcome};
pub use json::{JsonAdapter, JsonError, JsonKind, JsonNumber, JsonValue};
pub use model_stream::{
    FinishReason, ModelStream, ModelStreamError, ModelStreamItem, ModelStreamPhase,
};
pub use schema::{schema_for, JsonSchema, SchemaAdapter, SchemaType};
pub use types::{
    ContentPart, ConversationId, IdentifierError, Message, MessageId, MessageRole, ModelId,
    ModelResponseId, RequestId, RunId, TimestampMillis, TokenUsage, ToolCallId,
};
