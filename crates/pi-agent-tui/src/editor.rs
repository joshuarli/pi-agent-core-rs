//! `$EDITOR` integration with an explicit terminal suspension boundary.

use crate::terminal::{TerminalError, TerminalGuard};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Typed errors from the external-editor operation.
#[derive(Debug)]
pub enum EditorError {
    /// `$EDITOR` was not set to a non-empty program.
    MissingEditor,
    /// `$EDITOR` contained an unsupported unmatched quote or escape.
    MalformedEditor,
    /// The secure temporary file could not be created, written, or read.
    Io(io::Error),
    /// The editor process could not be started.
    Spawn(io::Error),
    /// The editor exited unsuccessfully; the composer remains unchanged.
    Failed { status: Option<i32> },
    /// Terminal suspension or restoration failed.
    Terminal(TerminalError),
}

impl fmt::Display for EditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEditor => formatter.write_str("$EDITOR is not set"),
            Self::MalformedEditor => {
                formatter.write_str("$EDITOR has unmatched quoting or escaping")
            }
            Self::Io(error) => write!(formatter, "editor temporary file failed: {error}"),
            Self::Spawn(error) => write!(formatter, "could not start $EDITOR: {error}"),
            Self::Failed {
                status: Some(status),
            } => {
                write!(formatter, "$EDITOR exited with status {status}")
            }
            Self::Failed { status: None } => {
                formatter.write_str("$EDITOR ended without an exit status")
            }
            Self::Terminal(error) => {
                write!(formatter, "editor terminal transition failed: {error}")
            }
        }
    }
}

impl std::error::Error for EditorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::Spawn(error) => Some(error),
            Self::Terminal(error) => Some(error),
            Self::MissingEditor | Self::MalformedEditor | Self::Failed { .. } => None,
        }
    }
}

impl From<TerminalError> for EditorError {
    fn from(error: TerminalError) -> Self {
        Self::Terminal(error)
    }
}

/// `$EDITOR` process boundary.
#[derive(Debug, Default)]
pub struct Editor;

impl Editor {
    /// Edit `current` without a shell and return the replacement text on success.
    ///
    /// The temporary path is created with exclusive creation. On Unix it is
    /// opened with mode `0600`; elsewhere exclusive creation still prevents a
    /// symlink or pre-existing-path substitution. The file is deleted by its
    /// guard on all recoverable paths.
    pub fn open(terminal: &mut TerminalGuard, current: &str) -> Result<String, EditorError> {
        let editor = std::env::var_os("EDITOR").ok_or(EditorError::MissingEditor)?;
        let arguments = parse_editor(&editor)?;
        let (program, arguments) = arguments.split_first().ok_or(EditorError::MissingEditor)?;
        let temporary = TemporaryFile::new(current)?;

        terminal.suspend()?;
        let editor_result = Command::new(program)
            .args(arguments)
            .arg(temporary.path())
            .status()
            .map_err(EditorError::Spawn)
            .and_then(|status| {
                if status.success() {
                    fs::read_to_string(temporary.path()).map_err(EditorError::Io)
                } else {
                    Err(EditorError::Failed {
                        status: status.code(),
                    })
                }
            });
        let resume_result = terminal.resume().map_err(EditorError::Terminal);
        match (editor_result, resume_result) {
            (_, Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Ok(text), Ok(())) => Ok(text),
        }
    }
}

#[derive(Debug)]
struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    fn new(contents: &str) -> Result<Self, EditorError> {
        let directory = std::env::temp_dir();
        for _ in 0..16 {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = directory.join(format!(
                "pi-agent-editor-{}-{nanos}-{sequence}.txt",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(&path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(contents.as_bytes()) {
                        let _ = fs::remove_file(&path);
                        return Err(EditorError::Io(error));
                    }
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(EditorError::Io(error)),
            }
        }
        Err(EditorError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique editor temporary file",
        )))
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Parse the traditional small `$EDITOR` command syntax without invoking a shell.
///
/// Space separates words; single quotes, double quotes, and backslash escapes
/// can include spaces. Shell expansion, command substitution, variables, and
/// pipelines are intentionally unsupported because they would change the
/// authority boundary of this application.
fn parse_editor(value: &OsStr) -> Result<Vec<OsString>, EditorError> {
    let value = value.to_str().ok_or(EditorError::MalformedEditor)?;
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut active = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
            active = true;
            escaped = false;
            continue;
        }
        match (quote, character) {
            (_, '\\') => escaped = true,
            (Some(delimiter), character) if delimiter == character => quote = None,
            (None, '\'' | '"') => {
                quote = Some(character);
                active = true;
            }
            (None, character) if character.is_whitespace() => {
                if active {
                    arguments.push(OsString::from(std::mem::take(&mut current)));
                    active = false;
                }
            }
            _ => {
                current.push(character);
                active = true;
            }
        }
    }
    if escaped || quote.is_some() {
        return Err(EditorError::MalformedEditor);
    }
    if active {
        arguments.push(OsString::from(current));
    }
    if arguments
        .first()
        .is_some_and(|argument| argument.is_empty())
    {
        return Err(EditorError::MissingEditor);
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_parser_never_needs_a_shell() {
        assert_eq!(
            parse_editor(OsStr::new("nvim -f --cmd 'set number'")).expect("valid editor command"),
            vec!["nvim", "-f", "--cmd", "set number"]
        );
        assert!(matches!(
            parse_editor(OsStr::new("nvim 'unterminated")),
            Err(EditorError::MalformedEditor)
        ));
    }
}
