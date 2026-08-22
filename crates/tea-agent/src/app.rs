//! Terminal application façade.
//!
//! The public module remains stable while the implementation is organized by
//! responsibility: command-line input, application errors, projected state,
//! host assembly, runtime/input handling, picker behavior, and presentation
//! helpers.

mod cli;
mod commands;
mod compaction;
mod error;
mod host;
mod input;
mod picker;
mod preferences;
mod runtime;
mod session;
mod state;
mod support;
mod tea;

#[cfg(test)]
mod tests;

pub use cli::{CliCommand, CliError, CliOptions};
pub use error::AppError;
pub use host::build_host_agent;
pub use runtime::App;
pub use state::{
    AppState, NoticeSeverity, ToolProjection, ToolState, TranscriptEntry, UiStatus, UiSurface,
};
pub use support::format_usage;
pub use tea::{load_tea_extensions, resolve_tea_home, TeaExtension, TeaExtensions, TeaLoadError};
