//! Local curl process and capture-file custody.

use crate::scheduler::CancellationToken;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;
pub(super) fn split_curl_status(bytes: &[u8]) -> Result<(&[u8], u16), String> {
    let output = std::str::from_utf8(bytes)
        .map_err(|_| "local transport returned non-UTF-8 output".to_owned())?;
    let (body, status) = output
        .rsplit_once('\n')
        .ok_or_else(|| "local transport did not report an HTTP status".to_owned())?;
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|_| "local transport reported an invalid HTTP status".to_owned())?;
    Ok((body.as_bytes(), status))
}

pub(super) fn process_capture_file(stream: &str) -> Result<(PathBuf, File), String> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pi-agent-local-{}-{sequence}-{stream}",
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
            Err(error) => return Err(format!("cannot create local transport capture: {error}")),
        }
    }
    Err("cannot allocate a unique local transport capture".to_owned())
}

pub(super) fn wait_for_child_or_cancellation(
    child: &mut std::process::Child,
    cancellation: &CancellationToken,
) -> Result<std::process::ExitStatus, String> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("local transport status could not be read: {error}"))?
        {
            return Ok(status);
        }
        if cancellation.is_cancelled() {
            let _ = child.kill();
            return child.wait().map_err(|error| {
                format!("cancelled local transport could not be reaped: {error}")
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}
