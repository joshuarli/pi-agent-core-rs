//! The Pi agent execution kernel.
//!
//! This crate is the deliberately small boundary between a caller-owned executor and the
//! agent state machine.  It does not create an executor, discover configuration, parse a
//! workspace, or own a model provider.  The modules below are scaffolding for the V0 loop;
//! each transition is represented by a typed operation so an implementation can be checked
//! against the pinned upstream SDK without leaking policy into the scheduler.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod agent;
pub mod default_tools;
pub mod error;
pub mod event;
pub mod hooks;
pub mod profile;
pub mod queue;
pub mod run;
pub mod scheduler;
mod schema_validation;
pub mod state;
pub mod tool;
#[cfg(feature = "trace")]
pub mod trace;

#[cfg(test)]
mod tests;

pub use agent::{Agent, AgentBuilder, EventSubscription, ObserverSubscription};
pub use default_tools::{
    CodingOperations, DefaultCodingTools, LocalCodingOperations, WorkspaceRoot,
};
pub use error::CoreError;
pub use event::{AgentEvent, AgentEventKind, EventObserver, EventSequence, ObserverFuture};
pub use run::RunHandle;
pub use state::{AgentSnapshot, Message, MessageId, RunId, RunSnapshot, ThinkingLevel};
