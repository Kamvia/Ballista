// File: ballista/src/main.rs
use std::process::ExitCode;

use ballista::scenario::Registry;

fn main() -> ExitCode {
    ballista::cli::main_with(Registry::new())
}
