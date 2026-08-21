//! Model stream items and their ordering grammar.
//!
//! Providers differ in transport details, but the agent kernel needs one
//! invariant: a response starts once, emits deltas and balanced tool-call
//! fragments, then settles exactly once.  [`ModelStream`] is a small validator
//! for that invariant.  It does not buffer text, parse tool arguments, or
//! schedule work; those responsibilities stay with the provider adapter and
//! kernel respectively.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use crate::types::{ModelId, ModelResponseId, TokenUsage, ToolCallId};

/// Terminal reason reported by a model response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinishReason {
    /// The model reached a normal stop condition.
    Stop,
    /// The provider stopped generation at a token limit.
    Length,
    /// The response ended to request one or more tool calls.
    ToolCall,
    /// The owner cancelled the response.
    Cancelled,
    /// The provider reported a model-side error as the terminal reason.
    Error,
    /// A provider-specific reason preserved for diagnostics.
    Other(String),
}

/// One normalized item in a model response stream.
#[derive(Clone, Debug, PartialEq)]
pub enum ModelStreamItem {
    /// Begin a response stream.
    Started {
        /// Provider/model response identity.
        response_id: ModelResponseId,
        /// Model selected for this response.
        model: ModelId,
    },
    /// Append assistant-visible text.
    TextDelta(String),
    /// Append separately classified reasoning content.
    ReasoningDelta(String),
    /// Begin a tool call whose arguments may arrive in multiple deltas.
    ToolCallStarted {
        /// Invocation identity.
        call_id: ToolCallId,
        /// Registered tool name.
        name: String,
    },
    /// Append a provider-normalized JSON argument fragment.
    ToolCallArgumentsDelta {
        /// Invocation identity.
        call_id: ToolCallId,
        /// Fragment to append.  Parsing is an adapter responsibility.
        delta: String,
    },
    /// Close one tool call's argument stream.
    ToolCallFinished {
        /// Invocation identity.
        call_id: ToolCallId,
    },
    /// Settle the response stream.
    Finished {
        /// Why the model stopped.
        reason: FinishReason,
        /// Provider-reported accounting, when available.
        usage: Option<TokenUsage>,
    },
    /// Settle the stream as failed before a normal finish.
    Error {
        /// Redacted provider-safe diagnostic.
        message: String,
    },
}

impl ModelStreamItem {
    /// Return the grammar token name used in diagnostics.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::TextDelta(_) => "text_delta",
            Self::ReasoningDelta(_) => "reasoning_delta",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::ToolCallArgumentsDelta { .. } => "tool_call_arguments_delta",
            Self::ToolCallFinished { .. } => "tool_call_finished",
            Self::Finished { .. } => "finished",
            Self::Error { .. } => "error",
        }
    }

    /// Whether this item settles the stream.
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. } | Self::Error { .. })
    }
}

/// Coarse state of a [`ModelStream`] validator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelStreamPhase {
    /// No start item has been accepted yet.
    #[default]
    Idle,
    /// A start item was accepted and the response is producing output.
    Active,
    /// A normal finish item was accepted.
    Finished,
    /// An error item was accepted.
    Failed,
}

/// Stateful validator for the normalized model stream grammar.
#[derive(Clone, Debug, Default)]
pub struct ModelStream {
    phase: ModelStreamPhase,
    response_id: Option<ModelResponseId>,
    model: Option<ModelId>,
    open_tool_calls: BTreeSet<ToolCallId>,
}

impl ModelStream {
    /// Create an empty validator in [`ModelStreamPhase::Idle`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the current stream phase.
    pub const fn phase(&self) -> ModelStreamPhase {
        self.phase
    }

    /// Return the response identity after a start item has been accepted.
    pub fn response_id(&self) -> Option<&ModelResponseId> {
        self.response_id.as_ref()
    }

    /// Return the selected model after a start item has been accepted.
    pub fn model(&self) -> Option<&ModelId> {
        self.model.as_ref()
    }

    /// Return currently open tool calls in deterministic order.
    pub fn open_tool_calls(&self) -> impl Iterator<Item = &ToolCallId> {
        self.open_tool_calls.iter()
    }

    /// Validate and accept one item.
    ///
    /// Invalid items leave the validator unchanged.  A valid terminal item
    /// moves the validator permanently to `Finished` or `Failed`; subsequent
    /// items are rejected.  The caller remains responsible for buffering
    /// deltas and dispatching completed tool calls.
    pub fn accept(&mut self, item: &ModelStreamItem) -> Result<(), ModelStreamError> {
        match self.phase {
            ModelStreamPhase::Idle => self.accept_idle(item),
            ModelStreamPhase::Active => self.accept_active(item),
            ModelStreamPhase::Finished | ModelStreamPhase::Failed => {
                Err(ModelStreamError::AfterTerminal {
                    phase: self.phase,
                    item: item.kind(),
                })
            }
        }
    }

    fn accept_idle(&mut self, item: &ModelStreamItem) -> Result<(), ModelStreamError> {
        match item {
            ModelStreamItem::Started { response_id, model } => {
                self.response_id = Some(response_id.clone());
                self.model = Some(model.clone());
                self.phase = ModelStreamPhase::Active;
                Ok(())
            }
            ModelStreamItem::Error { .. } => {
                self.phase = ModelStreamPhase::Failed;
                Ok(())
            }
            _ => Err(ModelStreamError::ExpectedStart { item: item.kind() }),
        }
    }

    fn accept_active(&mut self, item: &ModelStreamItem) -> Result<(), ModelStreamError> {
        match item {
            ModelStreamItem::Started { .. } => Err(ModelStreamError::DuplicateStart),
            ModelStreamItem::TextDelta(_) | ModelStreamItem::ReasoningDelta(_) => Ok(()),
            ModelStreamItem::ToolCallStarted { call_id, name } => {
                if name.is_empty() {
                    return Err(ModelStreamError::EmptyToolName);
                }
                if !self.open_tool_calls.insert(call_id.clone()) {
                    return Err(ModelStreamError::DuplicateToolCall(call_id.clone()));
                }
                Ok(())
            }
            ModelStreamItem::ToolCallArgumentsDelta { call_id, .. } => {
                if self.open_tool_calls.contains(call_id) {
                    Ok(())
                } else {
                    Err(ModelStreamError::UnknownToolCall(call_id.clone()))
                }
            }
            ModelStreamItem::ToolCallFinished { call_id } => {
                if self.open_tool_calls.remove(call_id) {
                    Ok(())
                } else {
                    Err(ModelStreamError::UnknownToolCall(call_id.clone()))
                }
            }
            ModelStreamItem::Finished { .. } if !self.open_tool_calls.is_empty() => {
                Err(ModelStreamError::OpenToolCallsAtFinish {
                    count: self.open_tool_calls.len(),
                })
            }
            ModelStreamItem::Finished { .. } => {
                self.phase = ModelStreamPhase::Finished;
                Ok(())
            }
            ModelStreamItem::Error { .. } => {
                self.open_tool_calls.clear();
                self.phase = ModelStreamPhase::Failed;
                Ok(())
            }
        }
    }
}

/// Grammar violation reported by [`ModelStream::accept`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelStreamError {
    /// A non-start item was received before a stream start.
    ExpectedStart {
        /// Received grammar token.
        item: &'static str,
    },
    /// A second start item was received.
    DuplicateStart,
    /// An item was received after a terminal item.
    AfterTerminal {
        /// Terminal phase already reached.
        phase: ModelStreamPhase,
        /// Received grammar token.
        item: &'static str,
    },
    /// A tool name was empty.
    EmptyToolName,
    /// A tool call ID was started more than once.
    DuplicateToolCall(ToolCallId),
    /// A tool-call fragment referred to no open invocation.
    UnknownToolCall(ToolCallId),
    /// A finish item arrived while a tool invocation remained open.
    OpenToolCallsAtFinish {
        /// Number of unclosed calls.
        count: usize,
    },
}

impl fmt::Display for ModelStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedStart { item } => write!(formatter, "expected stream start, got {item}"),
            Self::DuplicateStart => formatter.write_str("model stream started more than once"),
            Self::AfterTerminal { phase, item } => {
                write!(
                    formatter,
                    "got {item} after terminal stream phase {phase:?}"
                )
            }
            Self::EmptyToolName => formatter.write_str("model stream tool name cannot be empty"),
            Self::DuplicateToolCall(call_id) => write!(formatter, "duplicate tool call {call_id}"),
            Self::UnknownToolCall(call_id) => write!(formatter, "unknown tool call {call_id}"),
            Self::OpenToolCallsAtFinish { count } => {
                write!(formatter, "cannot finish with {count} open tool call(s)")
            }
        }
    }
}

impl Error for ModelStreamError {}

#[cfg(test)]
mod tests {
    use super::{FinishReason, ModelStream, ModelStreamError, ModelStreamItem, ModelStreamPhase};
    use crate::types::{ModelId, ModelResponseId, ToolCallId};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("test IDs are non-empty")
    }

    #[test]
    fn stream_requires_balanced_tool_calls_before_finish() {
        let mut stream = ModelStream::new();
        let start = ModelStreamItem::Started {
            response_id: id::<ModelResponseId>("response"),
            model: id::<ModelId>("model"),
        };
        let call_id = id::<ToolCallId>("call");

        stream.accept(&start).unwrap();
        stream
            .accept(&ModelStreamItem::ToolCallStarted {
                call_id: call_id.clone(),
                name: "search".into(),
            })
            .unwrap();
        assert_eq!(
            stream.accept(&ModelStreamItem::Finished {
                reason: FinishReason::ToolCall,
                usage: None,
            }),
            Err(ModelStreamError::OpenToolCallsAtFinish { count: 1 })
        );
        stream
            .accept(&ModelStreamItem::ToolCallFinished { call_id })
            .unwrap();
        stream
            .accept(&ModelStreamItem::Finished {
                reason: FinishReason::Stop,
                usage: None,
            })
            .unwrap();
        assert_eq!(stream.phase(), ModelStreamPhase::Finished);
    }
}
