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

const REPETITION_CHECK_MIN_BYTES: usize = 64 * 1024;
const REPETITION_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const REPETITION_WINDOW_BYTES: usize = 4 * 1024;
const REPETITION_MAX_DISTANCE_BYTES: usize = 128 * 1024;

static TRANSPORT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) struct TransportResponse {
    pub(super) body: Vec<u8>,
    pub(super) status_code: Option<u16>,
    pub(super) partial: bool,
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
    let (status, cancelled, stalled_bytes, repetitive) = wait_for_child_or_cancellation(
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
    if repetitive {
        return Err("OpenRouter HTTP transport stopped after repetitive prose".into());
    }
    if let Some(stalled_bytes) = stalled_bytes {
        if stalled_bytes > 0 && output.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return Ok(TransportResponse {
                body: output,
                status_code: http_status_code(&header_output),
                partial: true,
            });
        }
        return Err(format!(
            "OpenRouter HTTP transport stalled after {stalled_bytes} response bytes without meaningful progress"
        ));
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&error_output).trim().to_owned();
        if output.iter().any(|byte| !byte.is_ascii_whitespace()) {
            let status_code = http_status_code(&header_output);
            if status_code.is_none() {
                return Ok(TransportResponse {
                    body: output,
                    status_code,
                    partial: true,
                });
            }
            if detail.is_empty() {
                return Err(format!(
                    "OpenRouter HTTP transport failed after {} response bytes",
                    output.len()
                ));
            }
            return Err(format!(
                "OpenRouter HTTP transport failed after {} response bytes: {detail}",
                output.len()
            ));
        }
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
        partial: false,
    })
}

/// Retry only failures that occurred before the provider emitted response bytes.
///
/// Once a completion has produced any body bytes, replaying the request can charge the
/// provider twice and repeats a potentially pathological generation. A zero-byte stall and
/// connection failure remain safe bounded retry cases.
pub(super) fn retryable_transport_error(message: &str) -> bool {
    if message.contains("before a provider response") {
        return true;
    }
    message
        .strip_prefix("OpenRouter HTTP transport stalled after ")
        .and_then(|rest| rest.split_once(" response bytes"))
        .and_then(|(bytes, _)| bytes.parse::<u64>().ok())
        == Some(0)
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
) -> Result<(ExitStatus, bool, Option<u64>, bool), String> {
    let started = std::time::Instant::now();
    let mut last_meaningful_progress = started;
    let mut last_repetition_check = started;
    let mut observed_bytes = 0_u64;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("OpenRouter transport status could not be read: {error}"))?
        {
            return Ok((status, false, None, false));
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
            return Ok((status, true, None, false));
        }
        let (new_observed_bytes, meaningful_progress) =
            response_progress(stdout_path, observed_bytes);
        observed_bytes = new_observed_bytes;
        if meaningful_progress {
            last_meaningful_progress = std::time::Instant::now();
        }
        if observed_bytes >= REPETITION_CHECK_MIN_BYTES as u64
            && last_repetition_check.elapsed() >= REPETITION_CHECK_INTERVAL
        {
            last_repetition_check = std::time::Instant::now();
            if response_repetition_stall(stdout_path) {
                match child.kill() {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
                    Err(error) => {
                        return Err(format!(
                            "repetitive OpenRouter transport could not be killed: {error}"
                        ));
                    }
                }
                let status = child.wait().map_err(|error| {
                    format!("repetitive OpenRouter transport could not be reaped: {error}")
                })?;
                return Ok((status, false, Some(observed_bytes), true));
            }
        }
        // A finite Chat Completions response may take a short time to begin, but
        // an absent response beyond the stall bound is indistinguishable from a
        // provider connection that has wedged.  Apply the same bounded retryable
        // stall policy before and after the first response byte; the request's
        // longer curl max-time must not turn a dead provider into a wall-clock
        // session hang.
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
            return Ok((status, false, Some(observed_bytes), false));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn response_repetition_stall(stdout_path: &Path) -> bool {
    let Ok(bytes) = fs::read(stdout_path) else {
        return false;
    };
    response_bytes_repetition_stall(&bytes)
}

/// Detect a response that repeats the same prose span instead of making progress.
///
/// This deliberately does not reject long or tool-less prose. It requires two adjacent
/// 4 KiB windows to recur at the same distance, and ignores responses that contain a tool
/// call or terminal finish marker. Those constraints preserve ordinary analysis while
/// cutting off the model behavior that can stream the same paragraph indefinitely.
fn response_bytes_repetition_stall(bytes: &[u8]) -> bool {
    if bytes.len() < REPETITION_CHECK_MIN_BYTES
        || bytes.windows(b"tool_calls".len()).any(|window| window == b"tool_calls")
        || bytes
            .windows(br#"finish_reason":"stop"#.len())
            .any(|window| window == br#"finish_reason":"stop"#)
        || bytes
            .windows(br#"finish_reason":"length"#.len())
            .any(|window| window == br#"finish_reason":"length"#)
    {
        return false;
    }
    let tail_start = bytes.len().saturating_sub(REPETITION_WINDOW_BYTES * 2);
    let search_start = tail_start.saturating_sub(REPETITION_MAX_DISTANCE_BYTES);
    let tail = &bytes[tail_start..];
    let prefix = &tail[..16];
    let suffix = &tail[REPETITION_WINDOW_BYTES - 16..REPETITION_WINDOW_BYTES];
    for previous_start in search_start..tail_start {
        let distance = tail_start.saturating_sub(previous_start);
        if distance < REPETITION_WINDOW_BYTES
            || previous_start + REPETITION_WINDOW_BYTES * 2 > bytes.len()
        {
            continue;
        }
        let previous = &bytes[previous_start..previous_start + REPETITION_WINDOW_BYTES * 2];
        if &previous[..16] != prefix
            || &previous[REPETITION_WINDOW_BYTES - 16..REPETITION_WINDOW_BYTES] != suffix
        {
            continue;
        }
        if previous == tail {
            return true;
        }
    }
    false
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
        meaningful |= response_bytes_meaningful(&buffer[..read]);
    }
    (observed_bytes.saturating_add(total_read), meaningful)
}

fn response_bytes_meaningful(bytes: &[u8]) -> bool {
    bytes.split(|byte| *byte == b'\n').any(|line| {
        let line = line.trim_ascii();
        !line.is_empty() && !line.starts_with(b":")
    })
}

#[cfg(test)]
mod tests {
    use super::{
        http_status_code, response_bytes_meaningful, response_bytes_repetition_stall,
        retryable_transport_error, run_curl,
    };
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
        command.args(["-c", "printf '   '; sleep 1"]);
        let result = run_curl(
            &mut command,
            b"{}",
            &cancellation,
            std::time::Duration::from_millis(200),
        );
        let error = result.expect_err("whitespace-only response should stall");
        assert!(error.contains("stalled"), "unexpected error: {error}");
        assert!(error.contains("3 response bytes"), "unexpected error: {error}");
    }

    #[test]
    fn detects_pre_response_stall_without_body_bytes() {
        let cancellation = CancellationToken::new();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 1"]);
        let error = run_curl(
            &mut command,
            b"{}",
            &cancellation,
            std::time::Duration::from_millis(200),
        )
        .expect_err("a response that never emits bytes should stall");
        assert!(error.contains("stalled"), "unexpected error: {error}");
        assert!(error.contains("0 response bytes"), "unexpected error: {error}");
    }

    #[test]
    fn preserves_meaningful_partial_body_from_failed_curl() {
        let cancellation = CancellationToken::new();
        let mut command = Command::new("sh");
        command.args(["-c", "printf 'data: partial'; exit 1"]);
        let result = run_curl(
            &mut command,
            b"{}",
            &cancellation,
            std::time::Duration::from_millis(200),
        )
        .expect("meaningful partial body should be retained");
        assert!(result.partial);
        assert_eq!(result.body, b"data: partial");
    }

    #[test]
    fn permits_finite_response_without_body_before_stall_bound() {
        let cancellation = CancellationToken::new();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 0.01"]);
        let result = run_curl(
            &mut command,
            b"{}",
            &cancellation,
            std::time::Duration::from_millis(200),
        )
        .expect("a finite response may have no body bytes before exit");
        assert!(result.body.is_empty());
    }

    #[test]
    fn detects_repeated_prose_without_rejecting_distinct_prose() {
        let unit = b"data: {\"choices\":[{\"delta\":{\"content\":\"A useful analysis paragraph. \\n";
        let repeated = unit.repeat(2_000);
        assert!(response_bytes_repetition_stall(&repeated));

        let distinct = (0..2_000)
            .map(|index| format!("data: distinct analysis paragraph {index}.\\n"))
            .collect::<String>();
        assert!(!response_bytes_repetition_stall(distinct.as_bytes()));
    }

    #[test]
    fn ignores_openrouter_processing_heartbeats_as_progress() {
        assert!(!response_bytes_meaningful(
            b": OPENROUTER PROCESSING\n\n: OPENROUTER PROCESSING\n"
        ));
        assert!(response_bytes_meaningful(
            b": OPENROUTER PROCESSING\n\ndata: {\"choices\":[]}\n"
        ));
    }

    #[test]
    fn cuts_off_a_repetitive_stream_before_request_timeout() {
        let cancellation = CancellationToken::new();
        let mut command = Command::new("sh");
        command.args(["-c", "yes x | head -c 131072; sleep 1"]);
        let error = run_curl(
            &mut command,
            b"{}",
            &cancellation,
            std::time::Duration::from_secs(5),
        )
        .expect_err("repetitive response should be rejected");
        assert!(error.contains("repetitive"), "unexpected error: {error}");
    }

    #[test]
    fn retries_only_transport_failures_before_response_bytes() {
        assert!(retryable_transport_error(
            "OpenRouter HTTP transport failed before a provider response"
        ));
        assert!(retryable_transport_error(
            "OpenRouter HTTP transport stalled after 0 response bytes without meaningful progress"
        ));
        assert!(!retryable_transport_error(
            "OpenRouter HTTP transport failed after 32768 response bytes: curl: (28) timeout"
        ));
        assert!(!retryable_transport_error(
            "OpenRouter HTTP transport stalled after 32768 response bytes without meaningful progress"
        ));
    }

}
