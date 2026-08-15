//! Command Code subprocess transport custody.

use crate::scheduler::CancellationToken;
use std::io::{Read, Write};
use std::process::Command;
use std::thread;
use std::time::Duration;

pub(super) const API_URL: &str = "https://api.commandcode.ai/alpha/generate";

pub(super) fn run_curl(
    command: &mut Command,
    payload: &[u8],
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let mut child = command
        .spawn()
        .map_err(|_| "could not start the Command Code HTTP transport".to_owned())?;
    if cancellation.is_cancelled() {
        let _ = child.kill();
        child
            .wait()
            .map_err(|_| "cancelled Command Code transport could not be reaped".to_owned())?;
        return Err("Command Code request cancelled".into());
    }
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "Command Code HTTP transport has no request pipe".to_owned())?;
        stdin
            .write_all(payload)
            .map_err(|_| "could not send Command Code request".to_owned())?;
    }
    // Close the request pipe before waiting. This also lets a cancellation
    // kill/reap the process without retaining a second write handle.
    drop(child.stdin.take());
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "Command Code HTTP transport did not settle".to_owned())?
        {
            break status;
        }
        if cancellation.is_cancelled() {
            let _ = child.kill();
            child
                .wait()
                .map_err(|_| "cancelled Command Code transport could not be reaped".to_owned())?;
            return Err("Command Code request cancelled".into());
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| "Command Code HTTP transport has no response pipe".to_owned())?
        .read_to_end(&mut stdout)
        .map_err(|_| "could not read Command Code response".to_owned())?;
    if !status.success() {
        return Err("Command Code HTTP transport failed before a provider response".into());
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_and_reaps_an_in_flight_transport_child() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 5")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let error = run_curl(&mut command, b"{}", &cancellation)
            .expect_err("cancelled transport must not wait for curl timeout");
        assert_eq!(error, "Command Code request cancelled");
    }
}
