//! `superba-router`: Superba's local router service.
//!
//! Started by the per-user service manager (systemd user unit, LaunchAgent, or
//! the self-supervisor of the XDG autostart / Windows Run key backends); Superba
//! writes its configuration and manages the service. Exit code 78 means the
//! configuration is missing or invalid (the supervisor then stops restarting).
//!
//! On Windows the binary uses the GUI subsystem so the Run-key start at login opens
//! no console window; it attaches to the parent console when there is one, so
//! `status` and `--help` still print in a terminal (not verified on Windows yet).

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand};
use superba_core::DataDir;
use superba_router::client::AdminClient;
use superba_router::manager::{RouterManager, service_spec};
use superba_router::service::supervise::{EXIT_CONFIG, SuperviseEnd, supervise};
use superba_router::{RouterPaths, RouterServer};

#[derive(Parser)]
#[command(name = "superba-router", version, about = "Superba local router")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve on 127.0.0.1 until stopped.
    Run {
        /// Superba data dir (the router reads `<dir>/router/`).
        #[arg(long, env = "SUPERBA_DATA_DIR")]
        data_dir: PathBuf,
    },
    /// Keep `run` alive, restarting it when it exits (service backends without a
    /// supervising service manager).
    Supervise {
        /// Superba data dir.
        #[arg(long, env = "SUPERBA_DATA_DIR")]
        data_dir: PathBuf,
    },
    /// Print the running router's status as JSON.
    Status {
        /// Superba data dir.
        #[arg(long, env = "SUPERBA_DATA_DIR")]
        data_dir: PathBuf,
    },
    /// Stop and remove the per-user service. Superba normally does this when the
    /// router is turned off, after pointing Codex back at a direct line; run it by
    /// hand only to clean up, then let Superba repair Codex's configuration.
    Uninstall {
        /// Superba data dir.
        #[arg(long, env = "SUPERBA_DATA_DIR")]
        data_dir: PathBuf,
    },
}

fn main() -> ExitCode {
    attach_parent_console();
    let cli = Cli::parse();
    init_logging();
    raise_open_files_limit();
    match cli.command {
        Command::Run { data_dir } => run(data_dir),
        Command::Supervise { data_dir } => supervise_cmd(data_dir),
        Command::Status { data_dir } => status(data_dir),
        Command::Uninstall { data_dir } => uninstall(data_dir),
    }
}

#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: a plain Win32 call without pointers; it fails harmlessly when the
    // parent has no console (service-manager starts).
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}

/// launchd starts agents with a soft limit of 256 open files, and every proxied
/// request holds two sockets: raise the soft limit towards
/// [`superba_core::fdlimit::OPEN_FILES_TARGET`] (never above the hard limit).
/// Never fails; the values go to the debug log.
fn raise_open_files_limit() {
    use superba_core::fdlimit;
    if let Some(limit) = fdlimit::raise_open_files_limit(fdlimit::OPEN_FILES_TARGET) {
        tracing::debug!(
            before = ?limit.before,
            after = ?limit.after,
            hard = ?limit.hard,
            "open-file limit (RLIMIT_NOFILE soft)"
        );
    }
}

fn runtime() -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => Some(rt),
        Err(e) => {
            eprintln!("superba-router: cannot start the async runtime: {e}");
            None
        }
    }
}

fn run(data_dir: PathBuf) -> ExitCode {
    let paths = RouterPaths::new(&DataDir::new(data_dir));
    let Some(rt) = runtime() else {
        return ExitCode::FAILURE;
    };
    rt.block_on(async move {
        let server = match RouterServer::bind_from_files(paths).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "cannot start");
                return match e.code() {
                    "io" | "parse" | "invalid" | "secret" => exit(EXIT_CONFIG),
                    _ => ExitCode::FAILURE,
                };
            }
        };
        let handle = server.shutdown_handle();
        tokio::spawn(async move {
            wait_for_signal().await;
            handle.shutdown();
        });
        match server.serve().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!(error = %e, "router failed");
                ExitCode::FAILURE
            }
        }
    })
}

fn exit(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn current_exe() -> Option<PathBuf> {
    match std::env::current_exe() {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("superba-router: cannot locate its own executable: {e}");
            None
        }
    }
}

fn supervise_cmd(data_dir: PathBuf) -> ExitCode {
    let Some(exe) = current_exe() else {
        return ExitCode::FAILURE;
    };
    let spec = service_spec(&DataDir::new(data_dir), exe);
    let stop = Arc::new(AtomicBool::new(false));
    let Some(rt) = runtime() else {
        return ExitCode::FAILURE;
    };
    let flag = stop.clone();
    rt.spawn(async move {
        wait_for_signal().await;
        flag.store(true, Ordering::SeqCst);
    });
    let result = supervise(&spec, &stop);
    rt.shutdown_background();
    match result {
        Ok(SuperviseEnd::Stopped) => ExitCode::SUCCESS,
        Ok(SuperviseEnd::ConfigInvalid) => exit(EXIT_CONFIG),
        Ok(SuperviseEnd::SpawnFailed(why)) => {
            tracing::error!(reason = %why, "cannot start the router");
            ExitCode::FAILURE
        }
        Err(e) => {
            tracing::error!(error = %e, "supervisor failed");
            ExitCode::FAILURE
        }
    }
}

fn status(data_dir: PathBuf) -> ExitCode {
    let paths = RouterPaths::new(&DataDir::new(data_dir));
    let Some(rt) = runtime() else {
        return ExitCode::FAILURE;
    };
    rt.block_on(async move {
        let client = match AdminClient::from_runtime(&paths) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("superba-router: not running");
                return ExitCode::FAILURE;
            }
        };
        match client.status().await {
            Ok(s) => {
                match serde_json::to_string_pretty(&s) {
                    Ok(text) => println!("{text}"),
                    Err(e) => eprintln!("superba-router: {e}"),
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("superba-router: {e}");
                ExitCode::FAILURE
            }
        }
    })
}

fn uninstall(data_dir: PathBuf) -> ExitCode {
    let Some(exe) = current_exe() else {
        return ExitCode::FAILURE;
    };
    #[allow(deprecated)]
    let Some(home) = std::env::home_dir() else {
        eprintln!("superba-router: no home directory");
        return ExitCode::FAILURE;
    };
    let manager = RouterManager::system(&DataDir::new(data_dir), home, exe);
    match manager.uninstall_service() {
        Ok(()) => {
            println!(
                "The router service was removed. Open Superba to point Codex back at a direct line."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("superba-router: {e}");
            ExitCode::FAILURE
        }
    }
}
