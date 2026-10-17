use superba_core::{AuthApiHealth, AuthMode, KeyInput, KeyRef, LineId, ProfileId, Secret, catalog};

use super::{Result, missing, profile, site, stdin_secret, usage};
use crate::{KeyCheckArgs, KeyCommand, Output, Runtime, exit, text::text};

pub(super) async fn run(command: &KeyCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        KeyCommand::Set(args) => {
            let id = if let Some(id) = &args.profile {
                ProfileId::new(id)
            } else {
                rt.service
                    .overview()
                    .await?
                    .active
                    .and_then(|active| active.profile_id)
                    .ok_or_else(|| usage(out, text("cli.error.key_set_profile")))?
            };
            let set = rt.service.profile_set().await?;
            let mut profile = set
                .profile(&id)
                .cloned()
                .ok_or_else(|| missing(out, text("cli.error.profile_missing").arg("id", &id)))?;
            let secret = if let Some(var) = &args.key_env {
                profile.key_ref = Some(KeyRef::Env { var: var.clone() });
                None
            } else {
                Some(stdin_secret(rt, out)?)
            };
            let masked = secret.as_ref().map(Secret::masked);
            let saved = rt.service.save_profile(profile, secret).await?;
            profile::render_saved(&saved, masked.as_deref(), out)?;
            out.value(&saved)?;
            Ok(exit::OK)
        }
        KeyCommand::Check(args) => check(args, rt, out).await,
    }
}

async fn check(args: &KeyCheckArgs, rt: &Runtime, out: &mut Output) -> Result {
    let supplied = if let Some(var) = &args.key_env {
        let value = rt
            .env
            .var_non_empty(var)
            .ok_or_else(|| usage(out, text("cli.error.key_env").arg("var", var)))?;
        Some(KeyInput::Secret(Secret::new(value)))
    } else if args.stdin {
        Some(KeyInput::Secret(stdin_secret(rt, out)?))
    } else {
        None
    };
    let profile_id = if let Some(id) = &args.profile {
        Some(ProfileId::new(id))
    } else if supplied.is_none() {
        rt.service
            .overview()
            .await?
            .active
            .and_then(|a| a.profile_id)
    } else {
        None
    };
    let key = match (supplied, &profile_id) {
        (Some(key), _) => key,
        (None, Some(id)) => KeyInput::Profile(id.clone()),
        (None, None) => return Err(usage(out, text("cli.error.key_profile"))),
    };
    let profile = if let Some(id) = &profile_id {
        Some(
            rt.service
                .profile_set()
                .await?
                .profile(id)
                .cloned()
                .ok_or_else(|| missing(out, text("cli.error.profile_missing").arg("id", id)))?,
        )
    } else {
        None
    };
    let site_id = args
        .site
        .as_deref()
        .or_else(|| profile.as_ref().map(|p| p.site_id.as_str()))
        .unwrap_or(catalog::KRILL_SITE_ID);
    let site = site(rt, out, site_id).await?;
    let line = args
        .line
        .as_deref()
        .map(LineId::new)
        .or_else(|| profile.as_ref().and_then(|p| p.line_id.clone()))
        .or_else(|| site.default_line().map(|l| l.id.clone()))
        .ok_or_else(|| usage(out, text("cli.error.no_default_line").arg("site", &site.id)))?;
    let mode = args
        .auth
        .map(Into::into)
        .or_else(|| profile.as_ref().map(|p| p.auth_mode))
        .unwrap_or(AuthMode::ApiKey);
    let check = rt.service.check_key(&site.id, &line, mode, key).await?;
    out.field("cli.label.access", out.render(&check.access.text()))?;
    let health = match &check.whoami {
        Some(AuthApiHealth::Healthy) => out.label("cli.key.healthy"),
        Some(AuthApiHealth::Unhealthy { reason }) => {
            out.render(&text("cli.key.unhealthy").arg("reason", reason))
        }
        None => out.none(),
    };
    out.field("cli.label.whoami", health)?;
    if let Some(identity) = &check.identity {
        out.field("cli.label.email", out.optional(identity.email.as_ref()))?;
        out.field("cli.label.name", out.optional(identity.name.as_ref()))?;
    }
    out.field(
        "cli.label.codex_path",
        out.optional(check.codex_path.map(|p| p.suffix())),
    )?;
    out.field("cli.label.last4", &check.key_last4)?;
    out.value(&check)?;
    Ok(if check.access.is_ok() {
        exit::OK
    } else {
        exit::ERROR
    })
}
