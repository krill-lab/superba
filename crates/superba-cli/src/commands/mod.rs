//! Commands build requests and display service results; switching belongs to the service.

mod account;
mod autostart;
mod claude;
mod config;
mod execute;
mod handoff;
mod harness;
mod import;
mod key;
mod lines;
mod models;
mod notify;
mod preview;
mod profile;
mod remote;
mod requests;
mod restore;
mod router;
mod runtime;
mod selfupdate;
mod sessions;
mod settings;
mod site;
mod status;
mod switch;
mod versions;
mod wsl;

use superba_core::{Error, LocalizedText, Secret, Site, SiteId, SwitchIntent, SwitchRequest};

use crate::{Command, ExecArgs, Output, Runtime, exit, text::text};

pub(crate) struct Failure {
    pub error: Error,
    pub code: u8,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        let code = match &error {
            Error::Blocked { .. } => exit::BLOCKED,
            Error::Cancelled => exit::CANCELLED,
            _ => exit::ERROR,
        };
        Self { error, code }
    }
}

pub(crate) type Result<T = u8> = std::result::Result<T, Failure>;

pub(crate) fn usage(out: &Output, message: LocalizedText) -> Failure {
    Failure {
        error: Error::invalid(out.render(&message)),
        code: exit::USAGE,
    }
}

pub(crate) fn missing(out: &Output, message: LocalizedText) -> Error {
    Error::NotFound {
        what: out.render(&message),
    }
}

pub(crate) fn stdin_secret(rt: &Runtime, out: &Output) -> Result<Secret> {
    let line = (rt.read_stdin_line)().map_err(|_| Error::other(out.label("cli.error.stdin")))?;
    let secret = Secret::new(line);
    if secret.is_empty() {
        return Err(usage(out, text("cli.error.empty_key")));
    }
    Ok(secret)
}

pub(crate) async fn site(rt: &Runtime, out: &Output, id: &str) -> Result<Site> {
    rt.service
        .sites()
        .await?
        .into_iter()
        .find(|site| site.id == SiteId::new(id))
        .ok_or_else(|| missing(out, text("cli.error.site_missing").arg("id", id)).into())
}

pub(crate) fn request(intent: SwitchIntent, exec: &ExecArgs) -> SwitchRequest {
    SwitchRequest {
        intent,
        targets: exec.targets(),
        model_override: None,
        legacy_sessions: None,
        take_over_foreign_env: None,
        accept_unknown_versions: false,
        key_resolutions: Default::default(),
        accept_shared_home: false,
        migration_exclude: Vec::new(),
    }
}

pub(crate) async fn dispatch(command: &Command, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        Command::Status => status::run(rt, out).await,
        Command::Autostart { command } => autostart::run(command, rt, out).await,
        Command::Lines { site, probe } => lines::run(site, *probe, rt, out).await,
        Command::Key { command } => key::run(command, rt, out).await,
        Command::Profile { command } => profile::run(command, rt, out).await,
        Command::Switch(args) => switch::run(args, rt, out).await,
        Command::Restore(args) => restore::run(args, rt, out).await,
        Command::Launch { restart, exec } => {
            if *restart {
                execute::run(
                    request(SwitchIntent::RestartApp { install_id: None }, exec),
                    exec,
                    rt,
                    out,
                )
                .await
            } else {
                if exec != &ExecArgs::default() {
                    return Err(usage(out, text("cli.error.launch_flags")));
                }
                let session = rt.service.open_app().await?;
                status::app_session(&session, out)?;
                out.value(&session)?;
                Ok(exit::OK)
            }
        }
        Command::Sessions { command, host } => {
            sessions::run(command.as_ref(), host.as_deref(), rt, out).await
        }
        Command::Models {
            command,
            profile,
            refresh,
        } => models::run(command.as_ref(), profile.as_deref(), *refresh, rt, out).await,
        Command::Speed(args) => models::speed(args, rt, out).await,
        Command::Harness { command } => harness::run(command, rt, out).await,
        Command::Codex { command } => versions::codex(command, rt, out).await,
        Command::App { command } => versions::app(command, rt, out).await,
        Command::SelfUpdate { command } => selfupdate::run(command, rt, out).await,
        Command::Feeds { command } => versions::feeds(command, rt, out).await,
        Command::Site { command } => site::run(command, rt, out).await,
        Command::Account(args) => account::run(args, rt, out).await,
        Command::Settings { command } => settings::run(command, rt, out).await,
        Command::Config { command } => config::run(command, rt, out).await,
        Command::Router { command } => router::run(command, rt, out).await,
        Command::Claude { command } => claude::run(command, rt, out).await,
        Command::Usage(args) => match &args.command {
            None => requests::log(&args.log, rt, out).await,
            Some(crate::UsageCommand::Log(log)) => requests::log(log, rt, out).await,
            Some(crate::UsageCommand::Stats { by, since, source }) => {
                requests::stats(*by, *since, source.map(Into::into), rt, out).await
            }
        },
        Command::Remote { command } => remote::run(command, rt, out).await,
        Command::Wsl { command } => wsl::run(command, rt, out).await,
        Command::Import(args) => match &args.what {
            Some(crate::ImportCommand::Usage(u)) => import::usage_run(u, rt, out).await,
            None => import::run(args, rt, out).await,
        },
        Command::Doctor => status::doctor(rt, out).await,
        Command::Recover => status::recover(rt, out).await,
        Command::Uninstall {
            delete_backups,
            provider,
            take_over,
            exec,
        } => {
            let mut request = request(
                SwitchIntent::Uninstall {
                    delete_backups: *delete_backups,
                    provider: provider.map(Into::into),
                },
                exec,
            );
            if *take_over {
                superba_switch::ownership::take_over(&mut request);
            }
            execute::run(request, exec, rt, out).await
        }
        Command::HandOff(args) => handoff::run(args, rt, out).await,
        Command::Notify { command } => notify::run(command, rt, out).await,
    }
}
