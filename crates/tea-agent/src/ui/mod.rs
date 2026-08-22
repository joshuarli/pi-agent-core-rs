//! Tea-local presentation primitives.
//!
//! These modules deliberately stop at the terminal projection boundary. Core events, provider
//! selection, sessions, accounting, and compaction remain owned by their existing modules.

pub mod footer;
pub mod frame_layout;
pub mod menus;
pub mod theme;
pub mod transcript;
pub mod visual_layout;
