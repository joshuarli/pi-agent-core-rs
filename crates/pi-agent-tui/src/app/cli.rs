use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;

/// Explicit command-line inputs accepted by the v0 terminal host.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CliOptions {
    provider: Option<OsString>,
    model: Option<OsString>,
    cwd: Option<PathBuf>,
}

impl CliOptions {
    /// Parse `pi-agent [--provider <id>] [--model <id>] [--cwd <path>]`.
    pub fn parse<I>(args: I) -> Result<Self, CliError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut arguments = args.into_iter();
        let _program = arguments.next();
        let mut options = Self::default();
        while let Some(argument) = arguments.next() {
            let slot = match argument.to_string_lossy().as_ref() {
                "--provider" => OptionSlot::Provider,
                "--model" => OptionSlot::Model,
                "--cwd" => OptionSlot::Cwd,
                _ if argument.as_os_str().to_string_lossy().starts_with('-') => {
                    return Err(CliError::UnknownOption(argument));
                }
                _ => return Err(CliError::UnexpectedArgument(argument)),
            };
            let value = arguments
                .next()
                .ok_or_else(|| CliError::MissingValue(slot.name()))?;
            if value.is_empty() {
                return Err(CliError::EmptyValue(slot.name()));
            }
            options.set(slot, value)?;
        }
        Ok(options)
    }

    /// Borrow the explicitly selected provider, if supplied.
    pub fn provider(&self) -> Option<&OsStr> {
        self.provider.as_deref()
    }

    /// Borrow the explicitly selected model, if supplied.
    pub fn model(&self) -> Option<&OsStr> {
        self.model.as_deref()
    }

    /// Borrow the explicit workspace authority, if supplied.
    pub fn cwd(&self) -> Option<&std::path::Path> {
        self.cwd.as_deref()
    }

    fn set(&mut self, slot: OptionSlot, value: OsString) -> Result<(), CliError> {
        let destination = match slot {
            OptionSlot::Provider => &mut self.provider,
            OptionSlot::Model => &mut self.model,
            OptionSlot::Cwd => {
                if self.cwd.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::DuplicateOption(slot.name()));
                }
                return Ok(());
            }
        };
        if destination.replace(value).is_some() {
            Err(CliError::DuplicateOption(slot.name()))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OptionSlot {
    Provider,
    Model,
    Cwd,
}

impl OptionSlot {
    const fn name(self) -> &'static str {
        match self {
            Self::Provider => "--provider",
            Self::Model => "--model",
            Self::Cwd => "--cwd",
        }
    }
}

/// Errors produced by direct command-line parsing.
#[derive(Debug, Eq, PartialEq)]
pub enum CliError {
    /// An option had no following value.
    MissingValue(&'static str),
    /// An option was supplied more than once.
    DuplicateOption(&'static str),
    /// An option was supplied with an empty value.
    EmptyValue(&'static str),
    /// The option is not part of v0.
    UnknownOption(OsString),
    /// Positional arguments are not part of v0.
    UnexpectedArgument(OsString),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(flag) => write!(formatter, "missing value for {flag}"),
            Self::DuplicateOption(flag) => write!(formatter, "duplicate option {flag}"),
            Self::EmptyValue(flag) => write!(formatter, "empty value for {flag}"),
            Self::UnknownOption(option) => write!(formatter, "unknown option {option:?}"),
            Self::UnexpectedArgument(argument) => {
                write!(formatter, "unexpected argument {argument:?}")
            }
        }
    }
}

impl std::error::Error for CliError {}
