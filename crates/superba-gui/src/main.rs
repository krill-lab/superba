//! `superba`: the Superba desktop GUI. `superba <subcommand>` behaves like `spb`.

// Release builds on Windows use the GUI subsystem (no console window flashes up
// when the app is started from the Start menu). Subcommands then attach to the
// parent console, see `attach_parent_console`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    // The second stage of an update: swap files, restart the router, start the
    // new Superba, once the process that downloaded it has exited.
    if superba_versions::selfupdate::finish::is_finish_invocation(&args) {
        return match superba_versions::selfupdate::finish::FinishArgs::parse(&args)
            .and_then(|a| superba_versions::selfupdate::finish::run(&a))
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(superba_cli::exit::ERROR),
        };
    }
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
    // macOS starts apps with a soft limit of 256 open files.
    superba_cli::raise_open_files_limit();
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
