//! Headless `spb` command engine, shared with the Superba GUI binary.
//!
//! Commands only build requests, display service results, and answer questions
//! from explicit flags. All switching is delegated to `SwitchService`. Embedders
//! and tests inject a [`Runtime`] and [`Output`] into [`run`].

#![forbid(unsafe_code)]

pub mod args;
mod commands;
pub mod decider;
mod help;
pub mod output;
pub mod runtime;
pub mod text;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use clap::{
    CommandFactory, Parser,
    error::{ContextKind, ContextValue, ErrorKind},
};
use superba_core::{EnvSource, Error, Locale};

pub use args::*;
pub use decider::{CliEvents, FlagDecider};
pub use output::{Capture, Output};
pub use runtime::Runtime;

/// Stable process exit codes for `spb` and `superba <subcommand>`.
pub mod exit {
    /// Success.
    pub const OK: u8 = 0;
    /// Service or execution failure.
    pub const ERROR: u8 = 1;
    /// Invalid command arguments or missing input.
    pub const USAGE: u8 = 2;
    /// The requested plan is blocked.
    pub const BLOCKED: u8 = 3;
    /// The plan requires answers or confirmation.
    pub const NEEDS_CONFIRMATION: u8 = 4;
    /// Cancelled.
    pub const CANCELLED: u8 = 5;
}

/// Visible command names and aliases, used by the GUI to route invocations.
pub fn subcommand_names() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .flat_map(|command| {
            std::iter::once(command.get_name())
                .chain(command.get_visible_aliases())
                .map(str::to_owned)
        })
        .collect()
}

/// Whether arguments including argv[0] select the command engine.
/// Global options may precede the command; unrelated GUI flags remain GUI input.
pub fn is_cli_invocation(args: &[OsString]) -> bool {
    let names = subcommand_names();
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next().and_then(|arg| arg.to_str()) {
        if matches!(arg, "help" | "--help" | "-h" | "--version" | "-V")
            || names.iter().any(|name| name == arg)
        {
            return true;
        }
        match arg {
            "--json" | "--verbose" => continue,
            "--locale" => {
                let _ = args.next();
            }
            _ if arg.starts_with("--locale=") => {}
            _ => return false,
        }
    }
    false
}

/// Select CLI flag, injected environment, system locale, then English.
pub fn resolve_locale(explicit: Option<&str>, env: &dyn EnvSource) -> Locale {
    explicit
        .and_then(Locale::parse)
        .or_else(|| env.var("SUPERBA_LOCALE").as_deref().and_then(Locale::parse))
        .or_else(|| sys_locale::get_locale().as_deref().and_then(Locale::parse))
        .unwrap_or(Locale::En)
}

/// Run a parsed command against an injectable service and output sinks.
pub async fn run(cli: &Cli, rt: &Runtime, out: &mut Output) -> u8 {
    out.configure(
        cli.json,
        cli.verbose,
        resolve_locale(cli.locale.as_deref(), rt.env.as_ref()),
    );
    match commands::dispatch(&cli.command, rt, out).await {
        Ok(code) => code,
        Err(failure) => {
            out.report_error(&failure.error);
            failure.code
        }
    }
}

/// Parse, initialize logging and the async runtime, and run with production services.
/// Help, version and parse failures do not construct the production service.
pub fn main_with_args<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<OsString>>();
    let (json, locale) = early_options(&args);
    let binary = invoked_binary(args.first());
    let mut out = Output::new(
        json,
        locale,
        Box::new(std::io::stdout()),
        Box::new(std::io::stderr()),
    )
    .with_terminals(
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal(),
    );
    out.binary = binary;
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => {
            return ExitCode::from(match error.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                | ErrorKind::MissingSubcommand => {
                    let usage_error = error.kind() != ErrorKind::DisplayHelp;
                    let command =
                        help::localized(Cli::command().name(binary).bin_name(binary), &out.catalog);
                    let mut help_args = args.clone();
                    if error.kind() == ErrorKind::MissingSubcommand {
                        help_args.push("--help".into());
                    }
                    let help = match command.try_get_matches_from(&help_args) {
                        Err(error) => error.to_string(),
                        Ok(_) => String::new(),
                    };
                    if out.help(&help, usage_error).is_ok() {
                        if usage_error { exit::USAGE } else { exit::OK }
                    } else {
                        exit::ERROR
                    }
                }
                ErrorKind::DisplayVersion => {
                    let version = env!("CARGO_PKG_VERSION");
                    let result = if json {
                        out.value(&serde_json::json!({ "version": version }))
                    } else {
                        out.line(
                            text::text("cli.version")
                                .arg("binary", binary)
                                .arg("version", version),
                        )
                    };
                    if result.is_ok() {
                        exit::OK
                    } else {
                        exit::ERROR
                    }
                }
                _ => {
                    let message = argument_error(&error, &args, binary, &out);
                    out.report_error(&Error::invalid(out.render(&message)));
                    exit::USAGE
                }
            });
        }
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
    let executor = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(executor) => executor,
        Err(_) => {
            out.report_error(&Error::other(out.label("cli.error.runtime")));
            return ExitCode::from(exit::ERROR);
        }
    };
    ExitCode::from(executor.block_on(async {
        match Runtime::production() {
            Ok(rt) => run(&cli, &rt, &mut out).await,
            Err(error) => {
                let failure = commands::Failure::from(error);
                out.report_error(&failure.error);
                failure.code
            }
        }
    }))
}

/// A localized diagnostic for a parse failure. Clap's own text can echo rejected
/// argv values, including secrets, so it is never shown: only names that come
/// from the schema (missing arguments, suggestions, the command path) and the
/// name of an unknown option (the part before any `=`) or a short unknown
/// subcommand word are quoted, never a value.
fn argument_error(
    error: &clap::Error,
    args: &[OsString],
    binary: &'static str,
    out: &Output,
) -> superba_core::LocalizedText {
    let command = command_path(args);
    let help = if command.is_empty() {
        format!("{binary} help")
    } else {
        format!("{binary} help {command}")
    };
    let strings = |kind: ContextKind| match error.get(kind) {
        Some(ContextValue::String(s)) => vec![s.clone()],
        Some(ContextValue::Strings(v)) => v.clone(),
        _ => Vec::new(),
    };
    let suggestion = |kind: ContextKind| {
        strings(kind)
            .first()
            .map(|s| {
                out.render(&text::text("cli.error.did_you_mean").arg("suggestion", s.as_str()))
            })
            .map(|s| format!(" ({s})"))
            .unwrap_or_default()
    };
    match error.kind() {
        ErrorKind::UnknownArgument => {
            let flag = strings(ContextKind::InvalidArg)
                .into_iter()
                .next()
                .filter(|a| a.starts_with('-'))
                .map(|a| a.split('=').next().unwrap_or_default().to_owned());
            match flag {
                Some(flag) => text::text("cli.error.unknown_option")
                    .arg("option", flag)
                    .arg("suggestion", suggestion(ContextKind::SuggestedArg))
                    .arg("help", help),
                // A stray positional value may be a secret: never quote it.
                None => text::text("cli.error.unexpected_value").arg("help", help),
            }
        }
        ErrorKind::InvalidSubcommand => {
            let word = strings(ContextKind::InvalidSubcommand)
                .into_iter()
                .next()
                .filter(|w| {
                    w.len() <= 20
                        && w.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                });
            text::text("cli.error.unknown_command")
                .arg("command", word.unwrap_or_else(|| "…".into()))
                .arg("suggestion", suggestion(ContextKind::SuggestedSubcommand))
                .arg("help", help)
        }
        ErrorKind::MissingRequiredArgument => {
            // Missing-argument signatures are generated from clap's schema.
            let arguments = strings(ContextKind::InvalidArg);
            if arguments.is_empty() {
                text::text("cli.error.arguments_missing").arg("help", help)
            } else {
                text::text("cli.error.arguments_required")
                    .arg("arguments", arguments.join(", "))
                    .arg("help", help)
            }
        }
        ErrorKind::MissingSubcommand => text::text("cli.error.arguments_missing").arg("help", help),
        ErrorKind::ArgumentConflict => text::text("cli.error.arguments_conflict").arg("help", help),
        _ => text::text("cli.error.arguments").arg("help", help),
    }
}

/// The subcommand words of `args` that name commands (`site add`), from the
/// schema, so `help <command>` can be suggested without quoting user input.
fn command_path(args: &[OsString]) -> String {
    let mut command = Cli::command();
    let mut path: Vec<String> = Vec::new();
    let mut words = args.iter().skip(1).filter_map(|a| a.to_str());
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if word == "--locale" {
            words.next();
            continue;
        }
        if word.starts_with('-') {
            continue;
        }
        let Some(sub) = command
            .get_subcommands()
            .find(|c| c.get_name() == word || c.get_all_aliases().any(|a| a == word))
            .cloned()
        else {
            break;
        };
        path.push(sub.get_name().to_owned());
        command = sub;
    }
    path.join(" ")
}

fn invoked_binary(arg0: Option<&OsString>) -> &'static str {
    match arg0
        .and_then(|arg| Path::new(arg).file_stem())
        .and_then(|name| name.to_str())
    {
        Some("superba") => "superba",
        _ => "spb",
    }
}

fn early_options(args: &[OsString]) -> (bool, Locale) {
    let mut json = false;
    let mut locale = None;
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next().and_then(|s| s.to_str()) {
        if arg == "--" {
            break;
        }
        if arg == "--json" {
            json = true;
        } else if arg == "--locale" {
            locale = args.next().and_then(|s| s.to_str());
        } else if let Some(value) = arg.strip_prefix("--locale=") {
            locale = Some(value);
        }
    }
    (json, runtime::initial_locale(locale))
}
