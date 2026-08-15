//! Command Code subprocess transport custody.

use std::process::Command;

pub(super) const API_URL: &str = "https://api.commandcode.ai/alpha/generate";

pub(super) fn run_curl(command: &mut Command, payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = command
        .spawn()
        .map_err(|_| "could not start the Command Code HTTP transport".to_owned())?;
    {
        use std::io::Write;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "Command Code HTTP transport has no request pipe".to_owned())?;
        stdin
            .write_all(payload)
            .map_err(|_| "could not send Command Code request".to_owned())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| "Command Code HTTP transport did not settle".to_owned())?;
    if !output.status.success() {
        return Err("Command Code HTTP transport failed before a provider response".into());
    }
    Ok(output.stdout)
}
