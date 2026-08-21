use crate::editor::EditorError;
use crate::terminal::TerminalError;
use tea_core::provider::RegistryError;
use tea_core::CoreError;
use std::fmt;

use super::cli::CliError;
use super::phi::PhiLoadError;
use super::session::SessionError;

/// Local application failures. Provider and core failures retain their typed source.
#[derive(Debug)]
pub enum AppError {
    /// Command-line parsing failed.
    Cli(CliError),
    /// Terminal setup, input, output, or restoration failed.
    Terminal(TerminalError),
    /// `$EDITOR` integration failed before it could replace the composer.
    Editor(EditorError),
    /// The explicit workspace or startup selection was invalid.
    Setup(String),
    /// Registry model resolution or adapter construction failed.
    Registry(RegistryError),
    /// Phi extension discovery or authoring boundary failed.
    Phi(PhiLoadError),
    /// Versioned linear-session persistence failed at the TUI-owned boundary.
    Session(SessionError),
    /// A core state-machine operation failed.
    Core(CoreError),
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli(error) => error.fmt(formatter),
            Self::Terminal(error) => error.fmt(formatter),
            Self::Editor(error) => error.fmt(formatter),
            Self::Setup(message) => formatter.write_str(message),
            Self::Registry(error) => error.fmt(formatter),
            Self::Phi(error) => error.fmt(formatter),
            Self::Session(error) => error.fmt(formatter),
            Self::Core(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cli(error) => Some(error),
            Self::Terminal(error) => Some(error),
            Self::Editor(error) => Some(error),
            Self::Registry(error) => Some(error),
            Self::Phi(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::Setup(_) => None,
        }
    }
}

impl From<CliError> for AppError {
    fn from(error: CliError) -> Self {
        Self::Cli(error)
    }
}

impl From<TerminalError> for AppError {
    fn from(error: TerminalError) -> Self {
        Self::Terminal(error)
    }
}

impl From<EditorError> for AppError {
    fn from(error: EditorError) -> Self {
        Self::Editor(error)
    }
}

impl From<RegistryError> for AppError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<PhiLoadError> for AppError {
    fn from(error: PhiLoadError) -> Self {
        Self::Phi(error)
    }
}

impl From<SessionError> for AppError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<CoreError> for AppError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}
