use superba_core::{
    KeyResolution, LegacySessionChoice, LineId, ProfileId, SwitchIntent, SwitchRequest,
};

use super::{Result, execute, request, usage};
use crate::{ExecArgs, Output, Runtime, SwitchAnswers, SwitchArgs, text::text};

pub(super) async fn resolve_profile(
    explicit: Option<&str>,
    site: Option<&str>,
    rt: &Runtime,
    out: &Output,
) -> Result<ProfileId> {
    if let Some(id) = explicit {
        return Ok(ProfileId::new(id));
    }
    let active = rt
        .service
        .overview()
        .await?
        .active
        .and_then(|a| a.profile_id);
    if site.is_none() {
        return active.ok_or_else(|| usage(out, text("cli.error.choose_profile")));
    }
    let profiles = rt.service.profile_set().await?;
    let matches_site = |p: &&superba_core::Profile| Some(p.site_id.as_str()) == site;
    if let Some(id) = active
        && profiles.profile(&id).as_ref().is_some_and(matches_site)
    {
        return Ok(id);
    }
    profiles
        .profiles
        .iter()
        .filter(matches_site)
        .find(|p| !p.is_builtin())
        .map(|p| p.id.clone())
        .ok_or_else(|| usage(out, text("cli.error.choose_profile")))
}

pub(super) fn build(
    profile_id: ProfileId,
    answers: &SwitchAnswers,
    exec: &ExecArgs,
    out: &Output,
) -> Result<SwitchRequest> {
    let mut request = request(
        SwitchIntent::Profile {
            profile_id,
            line_id: answers.line.as_deref().map(LineId::new),
            auth_mode: answers.auth.map(Into::into),
        },
        exec,
    );
    request.model_override = answers.model.clone();
    request.legacy_sessions = if answers.migrate_legacy_sessions {
        Some(LegacySessionChoice::Migrate)
    } else if answers.skip_legacy_sessions {
        Some(LegacySessionChoice::Skip)
    } else {
        None
    };
    request.take_over_foreign_env = if answers.take_over_env {
        Some(true)
    } else if answers.keep_env {
        Some(false)
    } else {
        None
    };
    request.accept_unknown_versions = answers.accept_unknown_version;
    request.accept_shared_home = answers.accept_shared_home;
    request.migration_exclude = answers.exclude_thread.clone();
    for path in &answers.keep_key {
        request
            .key_resolutions
            .insert(path.clone(), KeyResolution::KeepCurrent);
    }
    for path in &answers.overwrite_key {
        if request.key_resolutions.contains_key(path) {
            return Err(usage(
                out,
                text("cli.error.key_resolution").arg("path", path),
            ));
        }
        request
            .key_resolutions
            .insert(path.clone(), KeyResolution::Overwrite);
    }
    Ok(request)
}

pub(super) async fn run(args: &SwitchArgs, rt: &Runtime, out: &mut Output) -> Result {
    let id = resolve_profile(args.profile.as_deref(), args.site.as_deref(), rt, out).await?;
    let request = build(id, &args.answers, &args.exec, out)?;
    execute::run(request, &args.exec, rt, out).await
}
