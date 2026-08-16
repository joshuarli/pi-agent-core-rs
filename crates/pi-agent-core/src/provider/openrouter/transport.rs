//! OpenRouter subprocess and private capture-file custody.

use super::config::OpenRouterConfigError;
use crate::scheduler::CancellationToken;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

pub(super) const COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
pub(super) const GENERATION_URL: &str = "https://openrouter.ai/api/v1/generation";

static TRANSPORT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) struct TransportResponse {
    pub(super) body: Vec<u8>,
    pub(super) status_code: Option<u16>,
}

pub(super) fn run_curl(
    command: &mut Command,
    payload: &[u8],
    cancellation: &CancellationToken,
    stall_timeout: Duration,
) -> Result<TransportResponse, String> {
    let (stdout_path, stdout) = capture_file("stdout")?;
    let (stderr_path, stderr) = match capture_file("stderr") {
        Ok(capture) => capture,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return Err(error);
        }
    };
    let (headers_path, _headers) = match capture_file("headers") {
        Ok(capture) => capture,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(error);
        }
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .arg("--dump-header")
        .arg(&headers_path);
    let mut child = command.spawn().map_err(|error| {
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        let _ = fs::remove_file(&headers_path);
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
        let _ = fs::remove_file(&headers_path);
        return Err(error);
    }
    let (status, cancelled, stalled_bytes) = wait_for_child_or_cancellation(
        &mut child,
        cancellation,
        &stdout_path,
        stall_timeout,
    )?;
    let output = fs::read(&stdout_path)
        .map_err(|error| format!("could not read OpenRouter response capture: {error}"));
    let error_output = fs::read(&stderr_path).unwrap_or_default();
    let header_output = fs::read(&headers_path).unwrap_or_default();
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    let _ = fs::remove_file(&headers_path);
    let output = output?;
    if cancelled {
        return Err("OpenRouter HTTP transport cancelled".into());
    }
    if let Some(stalled_bytes) = stalled_bytes {
        return Err(format!(
            "OpenRouter HTTP transport stalled after {stalled_bytes} response bytes without meaningful progress"
        ));
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
    Ok(TransportResponse {
        body: output,
        status_code: http_status_code(&header_output),
    })
}

pub(super) fn http_status_code(headers: &[u8]) -> Option<u16> {
    headers
        .split(|byte| *byte == b'\n')
        .rev()
        .filter_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let mut fields = line.split(|byte| *byte == b' ' || *byte == b'\t');
            let version = fields.next()?;
            if !version.starts_with(b"HTTP/") {
                return None;
            }
            std::str::from_utf8(fields.next()?).ok()?.parse().ok()
        })
        .next()
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
    stdout_path: &Path,
    stall_timeout: Duration,
) -> Result<(ExitStatus, bool, Option<u64>), String> {
    let started = std::time::Instant::now();
    let mut last_meaningful_progress = started;
    let mut observed_bytes = 0_u64;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("OpenRouter transport status could not be read: {error}"))?
        {
            return Ok((status, false, None));
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
            return Ok((status, true, None));
        }
        let (new_observed_bytes, meaningful_progress) =
            response_progress(stdout_path, observed_bytes);
        observed_bytes = new_observed_bytes;
        if meaningful_progress {
            last_meaningful_progress = std::time::Instant::now();
        }
        if last_meaningful_progress.elapsed() >= stall_timeout {
            match child.kill() {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
                Err(error) => {
                    return Err(format!(
                        "stalled OpenRouter transport could not be killed: {error}"
                    ));
                }
            }
            let status = child.wait().map_err(|error| {
                format!("stalled OpenRouter transport could not be reaped: {error}")
            })?;
            return Ok((status, false, Some(observed_bytes)));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn response_progress(stdout_path: &Path, observed_bytes: u64) -> (u64, bool) {
    let Ok(mut file) = File::open(stdout_path) else {
        return (observed_bytes, false);
    };
    if file.seek(SeekFrom::Start(observed_bytes)).is_err() {
        return (observed_bytes, false);
    }
    let mut buffer = [0_u8; 8 * 1024];
    let mut total_read = 0_u64;
    let mut meaningful = false;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        total_read = total_read.saturating_add(read as u64);
        meaningful |= buffer[..read].iter().any(|byte| !byte.is_ascii_whitespace());
    }
    (observed_bytes.saturating_add(total_read), meaningful)
}

#[cfg(test)]
mod tests {
    use super::{http_status_code, run_curl};
    use crate::scheduler::CancellationToken;
    use std::process::Command;

    #[test]
    fn reads_the_last_http_status_from_captured_headers() {
        assert_eq!(
            http_status_code(b"HTTP/2 502 Bad Gateway\r\ncontent-type: text/html\r\n\r\n"),
            Some(502)
        );
    }

    #[test]
    fn ignores_non_http_capture_lines() {
        assert_eq!(http_status_code(b"curl: (28) timed out\n"), None);
    }

    #[test]
    fn detects_whitespace_only_response_stall() {
        let cancellation = CancellationToken::new();
        let mut command = Command::new("sh");
        command.args(["-c", "printf '   '; sleep 30"]);
        let result = run_curl(
            &mut command,
            b"{}",
            &cancellation,
            std::time::Duration::from_millis(25),
        );
        let error = result.expect_err("whitespace-only response should stall");
        assert!(error.contains("stalled"), "unexpected error: {error}");
        assert!(error.contains("3 response bytes"), "unexpected error: {error}");
    }
}
