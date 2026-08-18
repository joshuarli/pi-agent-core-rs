//! Terminal application façade.
//!
//! The public module remains stable while the implementation is organized by
//! responsibility: command-line input, application errors, projected state,
//! host assembly, runtime/input handling, picker behavior, and presentation
//! helpers.

mod cli;
mod compaction;
mod error;
mod host;
mod input;
mod picker;
mod runtime;
mod state;
mod support;

#[cfg(test)]
mod tests;

pub use cli::{CliCommand, CliError, CliOptions};
pub use error::AppError;
pub use host::build_host_agent;
pub use runtime::App;
pub use state::{AppState, TranscriptLine, UiStatus};
pub use support::format_usage;
