//! Canonical agent and run state.
//!
//! State is split into durable conversation data and transient execution data. The public
//! state module is a compatibility facade over focused contract modules.

mod accounting;
mod identifiers;
mod lifecycle;
mod messages;

pub use accounting::*;
pub use identifiers::*;
pub use lifecycle::*;
pub use messages::*;
