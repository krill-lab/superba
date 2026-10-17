//! `spb`: the headless Superba CLI.

use std::process::ExitCode;

fn main() -> ExitCode {
    superba_cli::main_with_args(std::env::args_os())
}
