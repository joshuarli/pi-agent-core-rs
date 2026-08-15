//! Deterministic Rust adapter for the declarative Pi parity fixtures.
//!
//! The executable is deliberately outside the library runtime boundary: it
//! uses `smol::block_on` to drive one caller-owned fixture future. It accepts
//! one fixture path, has no network/provider capability, and supports the
//! closed V0 fixture subset implemented by the Rust core.

#[path = "pi-agent-parity/fixture.rs"]
mod fixture;
#[path = "pi-agent-parity/normalize.rs"]
mod normalize;
#[path = "pi-agent-parity/parser.rs"]
mod parser;
#[path = "pi-agent-parity/runner.rs"]
mod runner;

use fixture::Fixture;
use std::env;
use std::fs;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let fixture_path = single_fixture_path()?;
    let fixture =
        Fixture::parse(&fs::read_to_string(fixture_path).map_err(|error| error.to_string())?)?;
    let result = smol::block_on(runner::run_fixture(fixture))?;
    print!(
        "{}",
        result.to_json_string().map_err(|error| error.to_string())?
    );
    Ok(())
}

fn single_fixture_path() -> Result<String, String> {
    let mut arguments = env::args();
    let _program = arguments.next();
    let path = arguments
        .next()
        .ok_or_else(|| "expected exactly one declarative fixture path".to_owned())?;
    if arguments.next().is_some() {
        return Err("expected exactly one declarative fixture path".into());
    }
    Ok(path)
}
