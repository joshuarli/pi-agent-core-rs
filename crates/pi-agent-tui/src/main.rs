//! `pi-agent` command-line entry point.

use pi_agent_tui::{App, AppError, CliOptions};

fn main() {
    if let Err(error) = run() {
        eprintln!("pi-agent: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), AppError> {
    // Keep startup inputs explicit and local.  In particular, this does not inspect a Pi
    // installation or discover configuration from the environment.
    let options = CliOptions::parse(std::env::args_os())?;
    App::new(options).run()
}
