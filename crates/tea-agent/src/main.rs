//! `tea` command-line entry point.

use tea_agent::{App, AppError, CliCommand, CliOptions};

fn main() {
    if let Err(error) = run() {
        eprintln!("tea: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), AppError> {
    // Keep startup inputs explicit and local.  In particular, this does not inspect a Pi
    // installation or discover configuration from the environment.
    match CliOptions::parse_command(std::env::args_os())? {
        CliCommand::Help => {
            print!("{}", CliOptions::help_text());
            Ok(())
        }
        CliCommand::Options(options) => {
            let prompt = options.prompt().map(std::ffi::OsStr::to_owned);
            let mut app = App::new(options);
            match prompt {
                Some(prompt) => {
                    let prompt = prompt
                        .to_str()
                        .ok_or_else(|| AppError::Setup("-p/--prompt must be valid UTF-8".into()))?;
                    app.run_prompt(prompt.to_owned())
                }
                None => app.run(),
            }
        }
    }
}
