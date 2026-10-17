use superba_core::{
    AuthChoice, AuthMode, CodexSettings, KeyRef, LineId, ModelDefaults, Profile, ProfileId,
    SiteKind, catalog,
};

use super::{Result, execute, missing, stdin_secret, switch, usage};
use crate::{Output, ProfileAddArgs, ProfileCommand, ProfileEditArgs, Runtime, exit, text::text};

pub(super) async fn run(command: &ProfileCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        ProfileCommand::List => {
            let set = rt.service.profile_set().await?;
            let rows = set
                .profiles
                .iter()
                .map(|p| row(p, set.last_selected.as_ref() == Some(&p.id), out))
                .collect::<Vec<_>>();
            out.table(&HEADERS, rows)?;
            out.value(&serde_json::json!({ "profiles": set.profiles, "last_selected": set.last_selected, "settings": set.settings }))?;
            Ok(exit::OK)
        }
        ProfileCommand::Add(args) => add(args, rt, out).await,
        ProfileCommand::Edit(args) => edit(args, rt, out).await,
        ProfileCommand::Remove { id } => {
            rt.service.delete_profile(&ProfileId::new(id)).await?;
            out.line(text("cli.profile.removed").arg("id", id))?;
            out.value(&serde_json::json!({ "removed": id }))?;
            Ok(exit::OK)
        }
        ProfileCommand::Use { id, answers, exec } => {
            let request = switch::build(ProfileId::new(id), answers, exec, out)?;
            execute::run(request, exec, rt, out).await
        }
    }
}

fn parse_base_url(value: &str, out: &Output) -> Result<url::Url> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| usage(out, text("cli.error.base_url")))
}

async fn add(args: &ProfileAddArgs, rt: &Runtime, out: &mut Output) -> Result {
    if !valid_id(&args.id)
        || [catalog::PROFILE_ORIGINAL, catalog::PROFILE_OFFICIAL].contains(&args.id.as_str())
    {
        return Err(usage(out, text("cli.error.profile_id")));
    }
    let set = rt.service.profile_set().await?;
    let id = ProfileId::new(&args.id);
    if set.profile(&id).is_some() && !args.replace {
        return Err(superba_core::Error::Conflict {
            message: out.render(&text("cli.error.profile_exists").arg("id", &id)),
        }
        .into());
    }
    let site = super::site(rt, out, &args.site).await?;
    // The switching service treats a new profile under a preset ID (`krill`,
    // `custom`) as a setup-wizard re-run and merges it into the provider's
    // existing profile on that site, possibly the applied one: adding never
    // changes another profile, so that case is refused.
    if set.profile(&id).is_none()
        && catalog::presets()
            .iter()
            .any(|p| p.id == args.id && p.id != catalog::PRESET_OFFICIAL)
        && let Some(existing) = set
            .profiles
            .iter()
            .find(|p| !p.is_builtin() && p.site_id == site.id)
    {
        return Err(superba_core::Error::Conflict {
            message: out.render(
                &text("cli.error.profile_preset_id")
                    .arg("id", &id)
                    .arg("site", &site.id)
                    .arg("existing", &existing.id),
            ),
        }
        .into());
    }
    let choice: AuthChoice = args.auth.map(Into::into).unwrap_or_default();
    // Krill profiles start with Krill's recommended Codex settings (they are the
    // profile's own settings and can be changed with `spb settings`).
    let codex_settings = if site.kind == SiteKind::Krill && !args.no_recommended_keys {
        CodexSettings::krill_recommended()
    } else {
        CodexSettings::new()
    };
    let mut profile = Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        id,
        name: args.name.clone().unwrap_or_else(|| args.id.clone()),
        builtin: None,
        site_id: site.id.clone(),
        line_id: None,
        backup_lines: Vec::new(),
        auth_mode: AuthMode::ApiKey,
        auth: AuthChoice::ApiKey,
        key_ref: args
            .key_env
            .as_ref()
            .map(|var| KeyRef::Env { var: var.clone() }),
        base_url_override: None,
        model: ModelDefaults {
            auto_compact: None,
            context_window: None,
            speed: None,
            model: args.model.clone(),
            reasoning_effort: args.effort.clone(),
        },
        codex_settings,
    };
    profile.set_auth(choice);
    profile.set_line_priority(args.lines.iter().map(LineId::new));
    if let Some(url) = &args.base_url {
        profile.base_url_override = Some(parse_base_url(url, out)?);
    }
    let secret = args.key_stdin.then(|| stdin_secret(rt, out)).transpose()?;
    let masked = secret.as_ref().map(superba_core::Secret::masked);
    let saved = rt.service.save_profile(profile, secret).await?;
    render_saved(&saved, masked.as_deref(), out)?;
    out.value(&saved)?;
    Ok(exit::OK)
}

async fn edit(args: &ProfileEditArgs, rt: &Runtime, out: &mut Output) -> Result {
    let nothing = args.name.is_none()
        && args.lines.is_empty()
        && !args.auto_line
        && args.auth.is_none()
        && args.model.is_none()
        && args.effort.is_none()
        && args.base_url.is_none()
        && !args.clear_base_url;
    if nothing {
        return Err(usage(out, text("cli.error.profile_edit_nothing")));
    }
    let set = rt.service.profile_set().await?;
    let id = ProfileId::new(&args.id);
    let mut profile = set
        .profile(&id)
        .cloned()
        .ok_or_else(|| missing(out, text("cli.error.profile_missing").arg("id", &id)))?;
    if profile.is_builtin() {
        return Err(usage(out, text("cli.error.profile_builtin").arg("id", &id)));
    }
    if let Some(name) = &args.name {
        profile.name = name.trim().to_owned();
    }
    if args.auto_line {
        profile.set_line_priority(std::iter::empty());
    } else if !args.lines.is_empty() {
        profile.set_line_priority(args.lines.iter().map(LineId::new));
    }
    if let Some(choice) = args.auth {
        profile.set_auth(choice.into());
    }
    if let Some(model) = &args.model {
        profile.model.model = (!model.trim().is_empty()).then(|| model.trim().to_owned());
    }
    if let Some(effort) = &args.effort {
        profile.model.reasoning_effort =
            (!effort.trim().is_empty()).then(|| effort.trim().to_owned());
    }
    if args.clear_base_url {
        profile.base_url_override = None;
    } else if let Some(url) = &args.base_url {
        profile.base_url_override = Some(parse_base_url(url, out)?);
    }
    let saved = rt.service.save_profile(profile, None).await?;
    render_saved(&saved, None, out)?;
    out.line(text("cli.profile.applies_next"))?;
    out.value(&saved)?;
    Ok(exit::OK)
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

const HEADERS: [&str; 8] = [
    "cli.label.selected",
    "cli.label.id",
    "cli.label.name",
    "cli.label.site",
    "cli.label.lines",
    "cli.label.auth",
    "cli.label.key_source",
    "cli.label.base_url",
];

fn row(profile: &Profile, selected: bool, out: &Output) -> Vec<String> {
    let lines = profile.line_priority();
    vec![
        if selected { "*".into() } else { out.none() },
        profile.id.to_string(),
        profile.name.clone(),
        profile.site_id.to_string(),
        if lines.is_empty() {
            out.label("cli.value.auto")
        } else {
            lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" > ")
        },
        if profile.is_builtin() {
            out.render(&profile.auth_mode.text())
        } else {
            out.render(&profile.auth_choice().text())
        },
        key_source(profile.key_ref.as_ref(), out),
        out.optional(profile.base_url_override.as_ref()),
    ]
}

pub(super) fn key_source(reference: Option<&KeyRef>, out: &Output) -> String {
    match reference {
        Some(KeyRef::Keyring { .. }) => out.label("cli.key.keyring"),
        Some(KeyRef::File { .. }) => out.label("cli.key.file"),
        Some(KeyRef::Env { var }) => out.render(&text("cli.key.env").arg("var", var)),
        None => out.none(),
        _ => out.label("cli.value.unknown"),
    }
}

pub(super) fn render_saved(
    profile: &Profile,
    masked: Option<&str>,
    out: &mut Output,
) -> superba_core::Result<()> {
    out.table(&HEADERS, [row(profile, false, out)])?;
    if let Some(masked) = masked {
        out.field("cli.label.key", masked)?;
    }
    Ok(())
}
