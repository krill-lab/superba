//! `superba`: the Superba desktop GUI. `superba <subcommand>` behaves like `spb`.

// Release builds on Windows use the GUI subsystem (no console window flashes up
// when the app is started from the Start menu). Subcommands then attach to the
// parent console, see `attach_parent_console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    if superba_cli::is_cli_invocation(&args) {
        attach_parent_console();
        return superba_cli::main_with_args(args);
    }
    if args.get(1).is_some_and(|a| a == "--gui-help") {
        attach_parent_console();
        println!("{}", superba_gui::args::USAGE);
        return ExitCode::SUCCESS;
    }
    let gui_args = match superba_gui::GuiArgs::parse(&args) {
        Ok(a) => a,
        Err(e) => {
            attach_parent_console();
            eprintln!("error: {e}\n\n{}", superba_gui::args::USAGE);
            return ExitCode::from(superba_cli::exit::USAGE);
        }
    };
    match superba_gui::run(gui_args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(superba_cli::exit::ERROR)
        }
    }
}

/// Windows GUI-subsystem binaries have no console: attach to the console of the
/// shell that started us so `superba status` prints like `spb status`. A no-op when
/// there is no parent console (started from Explorer) and on other platforms.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: AttachConsole takes a process id constant and has no memory-safety
    // preconditions; failure (no parent console) is harmless and ignored.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}
