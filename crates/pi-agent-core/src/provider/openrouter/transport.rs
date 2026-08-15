//! OpenRouter subprocess and private capture-file custody.

use super::config::OpenRouterConfigError;
use crate::scheduler::CancellationToken;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

pub(super) const COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(super) const GENERATION_URL: &str = "https://openrouter.ai/api/v1/generation";

static TRANSPORT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn run_curl(
    command: &mut Command,
    payload: &[u8],
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let (stdout_path, stdout) = capture_file("stdout")?;
    let (stderr_path, stderr) = match capture_file("stderr") {
        Ok(capture) => capture,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return Err(error);
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let mut child = command.spawn().map_err(|error| {
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        format!("could not start the OpenRouter HTTP transport: {error}")
    })?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "OpenRouter HTTP transport has no request pipe".to_owned())
        .and_then(|mut stdin| {
            stdin
                .write_all(payload)
                .map_err(|_| "could not send OpenRouter request".to_owned())
        });
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        return Err(error);
    }
    let (status, cancelled) = wait_for_child_or_cancellation(&mut child, cancellation)?;
    let output = fs::read(&stdout_path)
        .map_err(|error| format!("could not read OpenRouter response capture: {error}"));
    let error_output = fs::read(&stderr_path).unwrap_or_default();
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    let output = output?;
    if cancelled {
        return Err("OpenRouter HTTP transport cancelled".into());
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&error_output).trim().to_owned();
        if detail.is_empty() {
            return Err("OpenRouter HTTP transport failed before a provider response".into());
        }
        return Err(format!(
            "OpenRouter HTTP transport failed before a provider response: {detail}"
        ));
    }
    Ok(output)
}

fn capture_file(stream: &str) -> Result<(PathBuf, File), String> {
    for _ in 0..16 {
        let sequence = TRANSPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pi-agent-core-openrouter-{}-{sequence}-{stream}",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not create private transport capture: {error}"
                ));
            }
        }
    }
    Err("could not allocate a private OpenRouter transport capture".into())
}

pub(super) fn write_curl_config(api_key: &str) -> Result<PathBuf, String> {
    if api_key.contains(['\n', '\r']) {
        return Err(OpenRouterConfigError::ApiKeyContainsLineBreak.to_string());
    }
    let escaped = api_key.replace('\\', "\\\\").replace('"', "\\\"");
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pi-agent-core-openrouter-{}-{}.curl",
        std::process::id(),
        TRANSPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| {
        format!("could not create private OpenRouter transport config: {error}")
    })?;
    writeln!(file, "header = \"Authorization: Bearer {escaped}\"")
        .and_then(|_| writeln!(file, "header = \"Content-Type: application/json\""))
        .map_err(|error| format!("could not write private OpenRouter transport config: {error}"))?;
    Ok(path)
}

fn wait_for_child_or_cancellation(
    child: &mut Child,
    cancellation: &CancellationToken,
) -> Result<(ExitStatus, bool), String> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("OpenRouter transport status could not be read: {error}"))?
        {
            return Ok((status, false));
        }
        if cancellation.is_cancelled() {
            match child.kill() {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
                Err(error) => {
                    return Err(format!(
                        "cancelled OpenRouter transport could not be killed: {error}"
                    ));
                }
            }
            let status = child.wait().map_err(|error| {
                format!("cancelled OpenRouter transport could not be reaped: {error}")
            })?;
            return Ok((status, true));
        }
        thread::sleep(Duration::from_millis(10));
    }
}
