//! The small terminal host for [`pi_agent_core`].
//!
//! Core owns the conversation and execution state; these modules own the
//! terminal projection and input surface. The binary is intentionally small:
//! it consumes lossless typed core events, paints a local cell grid directly
//! through Crossterm, and drives core futures on Smol.
#![forbid(unsafe_code)]

pub mod app;
pub mod composer;
pub mod editor;
pub mod grid;
pub mod render;
pub mod terminal;

pub use app::{
    build_host_agent, App, AppError, AppState, CliCommand, CliOptions, ToolState, TranscriptKind,
    TranscriptLine,
};
