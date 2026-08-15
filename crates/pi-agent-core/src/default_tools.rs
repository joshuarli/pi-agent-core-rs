//! Batteries-included, explicit coding tools for the pinned Pi profile.
//!
//! The implementation is organized under `tools/`; this module remains the
//! compatibility facade for the long-standing `pi_agent_core::default_tools`
//! paths.

pub use crate::tools::{
    CodingOperations, CommandEnvironment, CommandOutput, DefaultCodingTools, DirectoryEntry,
    EntryMetadata, GrepMatch, GrepOptions, LocalCodingOperations, OperationError, OperationFuture,
    WorkspaceRoot,
};
