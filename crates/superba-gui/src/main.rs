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
    // Windows: never as another account than the signed-in user (SYSTEM,
    // `runas`, a scheduled task): its files would land in that account's
    // profile. Checked before anything is written (user bug 44).
    if superba_gui::startup::refuse_other_account(
        gui_args
            .locale
            .unwrap_or_else(superba_gui::i18n::system_locale),
    ) {
        return ExitCode::from(superba_cli::exit::BLOCKED);
    }
    // macOS starts apps with a soft limit of 256 open files.
    superba_cli::raise_open_files_limit();
    // GH#7: the "ChatGPT (Superba)" shortcut. A running Superba window takes
    // the request; otherwise ChatGPT is opened here without a window, which
    // shows only when a reopen must be confirmed or opening failed.
    if gui_args.launch_chatgpt
        && superba_gui::launch::forward_or_launch() != superba_gui::launch::Handled::ShowWindow
    {
        return ExitCode::SUCCESS;
    }
    // Item 70: one Superba per user. Starting it again shows the window that
    // runs (also when it hides in the tray) instead of opening a second one.
    if !gui_args.launch_chatgpt && superba_gui::instance::show_running() {
        return ExitCode::SUCCESS;
    }
    // From here on the GUI never ends silently: a failed start or a panic is
    // logged and, on Windows (no console), shown in a message box (user bug 43).
    let report = superba_gui::startup::Reporter::new(
        gui_args
            .locale
            .unwrap_or_else(superba_gui::i18n::system_locale),
    );
    report.begin();
    report.install_wsl_diagnostics();
    report.install_panic_hook();
    match superba_gui::run(gui_args, &report) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report.report_failure(&e);
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
