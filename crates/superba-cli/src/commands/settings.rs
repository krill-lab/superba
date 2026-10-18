//! `spb settings`: Codex settings, driven by the settings catalog of the
//! installed Codex. Without `--profile` they are this computer's `config.toml`
//! (like the settings page; no profile needed to look) and edits go to the
//! key-based profile applied on this computer; `--profile` shows or edits one
//! profile's own settings. Changes are stored in the profile and written into
//! `config.toml` when it is applied (`spb settings diff` previews that switch).

use superba_core::{
    BuiltinProfile, Locale, LocalizedText, Profile, ProfileId, ProfileSet, SettingEntry,
    SettingRow, SettingValue, SettingsEdit, SettingsEditReport, SettingsScope, SettingsView,
    SettingsViewRequest, SwitchIntent,
};

use super::{Result, request, usage};
use crate::{ExecArgs, Output, Runtime, SettingsCommand, exit, text::text};

pub(super) async fn run(command: &SettingsCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        SettingsCommand::List {
            paths,
            profile,
            advanced,
        } => {
            let view = rt
                .service
                .settings_view(SettingsViewRequest {
                    profile_id: profile.as_deref().map(ProfileId::new),
                    advanced: *advanced,
                    paths: paths.clone(),
                })
                .await?;
            render_list(&view, *advanced, out)?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        SettingsCommand::Get { path, profile } => {
            let view = rt
                .service
                .settings_view(SettingsViewRequest {
                    profile_id: profile.as_deref().map(ProfileId::new),
                    advanced: true,
                    paths: vec![path.clone()],
                })
                .await?;
            // A built-in profile: say why it has no settings of its own.
            if view.builtin.is_some() {
                render_scope(&view, out)?;
            }
            let Some(row) = view.rows.iter().find(|r| &r.path == path) else {
                return Err(
                    super::missing(out, text("cli.settings.unknown").arg("path", path)).into(),
                );
            };
            render_row(row, out)?;
            out.value(row)?;
            Ok(exit::OK)
        }
        SettingsCommand::Set {
            path,
            value,
            profile,
            force,
        } => {
            let value = parse_value(value);
            edit(
                SettingsEdit {
                    profile_id: profile.as_deref().map(ProfileId::new),
                    set: vec![(path.clone(), value)],
                    force: *force,
                    ..SettingsEdit::default()
                },
                rt,
                out,
            )
            .await
        }
        SettingsCommand::Reset { paths, profile } => {
            edit(
                SettingsEdit {
                    profile_id: profile.as_deref().map(ProfileId::new),
                    reset: paths.clone(),
                    ..SettingsEdit::default()
                },
                rt,
                out,
            )
            .await
        }
        SettingsCommand::Forget { paths, profile } => {
            edit(
                SettingsEdit {
                    profile_id: profile.as_deref().map(ProfileId::new),
                    forget: paths.clone(),
                    ..SettingsEdit::default()
                },
                rt,
                out,
            )
            .await
        }
        SettingsCommand::Migrate { legacy, profile } => {
            edit(
                SettingsEdit {
                    profile_id: profile.as_deref().map(ProfileId::new),
                    accept_migrations: legacy.clone(),
                    ..SettingsEdit::default()
                },
                rt,
                out,
            )
            .await
        }
        SettingsCommand::Diff { profile, remotes } => {
            diff(profile.as_deref(), remotes, rt, out).await
        }
    }
}

/// A TOML literal (`true`, `3`, `"x"`, `["a"]`), else the text as a string.
fn parse_value(raw: &str) -> SettingValue {
    SettingValue::from_toml_repr(raw.trim()).unwrap_or_else(|| SettingValue::from(raw))
}

/// The key-based profiles, for hints (`krill, work`).
fn keyed_ids(set: &ProfileSet) -> String {
    set.profiles
        .iter()
        .filter(|p| !p.is_builtin())
        .map(|p| p.id.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where a setting can be stored instead: `--profile <id>` with the key-based
/// profiles, or "add one first".
fn store_hint(set: &ProfileSet, out: &Output) -> LocalizedText {
    let ids = keyed_ids(set);
    if ids.is_empty() {
        text("cli.settings.hint_add_profile").arg("binary", out.binary)
    } else {
        text("cli.settings.hint_profile").arg("profiles", ids)
    }
}

/// What a built-in profile is, in one sentence.
fn builtin_text(profile: &Profile) -> LocalizedText {
    let key = match profile.builtin {
        Some(BuiltinProfile::Original) => "cli.settings.builtin_original",
        _ => "cli.settings.builtin_official",
    };
    text(key).arg("profile", &profile.id)
}

/// The profile an edit goes to. Without `--profile` it is the key-based
/// profile applied on this computer; with the official login (or nothing)
/// applied there is none, and a built-in profile has no settings of its own:
/// both are explained, with the profiles that can hold the setting.
async fn edit_target(edit: &mut SettingsEdit, rt: &Runtime, out: &Output) -> Result<()> {
    let set = rt.service.profile_set().await?;
    if let Some(id) = &edit.profile_id {
        if let Some(p) = set.profile(id).filter(|p| p.is_builtin()) {
            return Err(usage(
                out,
                text("cli.settings.not_here")
                    .arg_text("reason", builtin_text(p))
                    .arg_text("hint", store_hint(&set, out)),
            ));
        }
        return Ok(());
    }
    let applied = rt
        .service
        .overview()
        .await?
        .active
        .and_then(|a| a.profile_id)
        .and_then(|id| set.profile(&id).cloned());
    let reason = match &applied {
        Some(p) if !p.is_builtin() => {
            edit.profile_id = Some(p.id.clone());
            return Ok(());
        }
        Some(p) if p.builtin == Some(BuiltinProfile::Original) => {
            text("cli.settings.no_target_original")
        }
        Some(_) => text("cli.settings.no_target_official"),
        None => text("cli.settings.no_target_none"),
    };
    Err(usage(
        out,
        text("cli.settings.not_here")
            .arg_text("reason", reason)
            .arg_text("hint", store_hint(&set, out)),
    ))
}

async fn edit(mut edit: SettingsEdit, rt: &Runtime, out: &mut Output) -> Result {
    edit_target(&mut edit, rt, out).await?;
    let report: SettingsEditReport = rt.service.edit_settings(edit).await?;
    out.line(text("cli.settings.saved").arg("profile", &report.profile.id))?;
    for note in &report.notes {
        out.notice(note)?;
    }
    out.line(
        text("cli.settings.applies_next")
            .arg("binary", out.binary)
            .arg("profile", &report.profile.id),
    )?;
    out.value(&report)?;
    Ok(exit::OK)
}

fn managed_text(entry: Option<&SettingEntry>, out: &Output) -> String {
    match entry {
        None => out.none(),
        Some(SettingEntry::Unset) => out.label("cli.settings.removed"),
        Some(e) => e.to_toml_repr().unwrap_or_else(|| out.none()),
    }
}

fn stage_text(row: &SettingRow, out: &Output) -> String {
    match &row.feature {
        Some(f) => out.render(&f.stage.text()),
        None if row.key.as_ref().is_some_and(|k| k.deprecated_hint) => {
            out.label("cli.settings.deprecated")
        }
        None if row.key.is_none() => out.label("cli.settings.not_in_catalog"),
        None => out.none(),
    }
}

fn default_text(row: &SettingRow, out: &Output) -> String {
    row.key
        .as_ref()
        .and_then(|k| k.default.as_ref())
        .map(SettingValue::to_toml_repr)
        .unwrap_or_else(|| out.none())
}

/// Where the rows come from: this computer's `config.toml` (and the applied
/// profile whose settings Superba writes into it) or one profile.
fn render_scope(view: &SettingsView, out: &mut Output) -> Result<()> {
    match (view.scope, &view.profile_id) {
        (SettingsScope::Profile, Some(id)) => {
            out.field("cli.label.profile_used", id)?;
            if view.builtin.is_none() {
                out.line(
                    text("cli.settings.scope_profile")
                        .arg("profile", id)
                        .arg("binary", out.binary),
                )?;
            }
        }
        _ => {
            out.field(
                "cli.settings.scope_computer",
                view.config_path
                    .as_deref()
                    .map(superba_core::paths::display_path)
                    .unwrap_or_else(|| "config.toml".into()),
            )?;
            if let Some(id) = &view.profile_id {
                out.line(text("cli.settings.scope_applied").arg("profile", id))?;
            }
        }
    }
    if let Some(v) = &view.catalog_version {
        out.field("cli.label.catalog_version", v)?;
    }
    for note in &view.notes {
        out.notice(note)?;
    }
    Ok(())
}

fn render_list(view: &SettingsView, advanced: bool, out: &mut Output) -> Result<()> {
    render_scope(view, out)?;
    let own = view.scope == SettingsScope::Profile;
    // This computer: the value in config.toml first, the applied profile's
    // value only when one is applied. A profile: its own value first.
    let with_managed = own || view.profile_id.is_some();
    let rows = view
        .rows
        .iter()
        .map(|r| {
            let current = r.current.clone().unwrap_or_else(|| out.none());
            let managed = managed_text(r.managed.as_ref(), out);
            let mut row = vec![r.path.clone()];
            match (own, with_managed) {
                (true, _) => row.extend([managed, current]),
                (false, true) => row.extend([current, managed]),
                (false, false) => row.push(current),
            }
            row.extend([default_text(r, out), stage_text(r, out)]);
            row
        })
        .collect::<Vec<_>>();
    let mut headers = vec!["cli.settings.path"];
    match (own, with_managed) {
        (true, _) => headers.extend(["cli.settings.managed", "cli.settings.current"]),
        (false, true) => headers.extend(["cli.settings.current", "cli.settings.managed"]),
        (false, false) => headers.push("cli.settings.current"),
    }
    headers.extend(["cli.settings.default", "cli.settings.stage"]);
    let empty = rows.is_empty();
    out.section_table("cli.settings.title", &headers, rows)?;
    if own {
        if let (true, None, Some(id)) = (empty, view.builtin, &view.profile_id) {
            out.line(
                text("cli.settings.profile_empty")
                    .arg("profile", id)
                    .arg("binary", out.binary),
            )?;
        }
    } else if !advanced {
        out.line(text("cli.settings.advanced_hint").arg("binary", out.binary))?;
    }
    Ok(())
}

fn render_row(row: &SettingRow, out: &mut Output) -> Result<()> {
    out.field("cli.settings.path", &row.path)?;
    let description = row
        .key
        .as_ref()
        .and_then(|k| k.description.clone())
        .or_else(|| {
            row.feature
                .as_ref()
                .and_then(|f| f.menu_description.clone())
        });
    // English is the official upstream text; Chinese is human-written only.
    out.field(
        "cli.settings.description",
        description.unwrap_or_else(|| out.label("cli.settings.no_description")),
    )?;
    if out.locale() == Locale::ZhCn {
        match &row.zh {
            Some(zh) => out.field("cli.settings.zh", zh)?,
            None => out.line(text("cli.settings.no_zh"))?,
        }
    }
    if let Some(key) = &row.key {
        out.field("cli.settings.kind", out.label(kind_label(key.kind)))?;
        if !key.enum_values.is_empty() {
            out.field(
                "cli.settings.values",
                key.enum_values
                    .iter()
                    .map(|v| v.value.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )?;
        }
        if let Some(since) = &key.since {
            out.field("cli.settings.since", since)?;
        }
    }
    out.field("cli.settings.stage", stage_text(row, out))?;
    out.field("cli.settings.default", default_text(row, out))?;
    out.field(
        "cli.settings.current",
        row.current.clone().unwrap_or_else(|| out.none()),
    )?;
    out.field(
        "cli.settings.managed",
        managed_text(row.managed.as_ref(), out),
    )?;
    for s in &row.support {
        out.line(
            text("cli.settings.support_row")
                .arg("consumer", &s.label)
                .arg(
                    "version",
                    s.version
                        .as_ref()
                        .map_or_else(|| out.label("cli.value.unknown"), ToString::to_string),
                )
                .arg_text("support", s.support.text()),
        )?;
    }
    Ok(())
}

/// Plan a switch to the profile (never executed) and print the settings preview.
async fn diff(profile: Option<&str>, remotes: &[String], rt: &Runtime, out: &mut Output) -> Result {
    let profile_id = super::switch::resolve_profile(profile, None, rt, out).await?;
    let exec = ExecArgs {
        remotes: remotes.to_vec(),
        dry_run: true,
        ..ExecArgs::default()
    };
    let plan = rt
        .service
        .plan(request(
            SwitchIntent::Profile {
                profile_id,
                line_id: None,
                auth_mode: None,
            },
            &exec,
        ))
        .await?;
    if let superba_core::RestartKind::Blocked { reasons } = &plan.primary {
        for reason in reasons {
            out.progress(reason.text())?;
        }
    }
    let settings = rt.service.plan_settings(&plan.id).await?;
    for target in &settings.targets {
        for skip in &target.skipped {
            out.notice(&superba_core::Notice::info(
                text("cli.settings.skipped")
                    .arg("path", &skip.path)
                    .arg_text("reason", skip.reason.text()),
            ))?;
        }
    }
    super::execute::render_changes(&settings.changes, out)?;
    out.value(&serde_json::json!({ "plan_id": plan.id, "settings": settings }))?;
    Ok(if plan.is_blocked() {
        exit::BLOCKED
    } else {
        exit::OK
    })
}

/// The catalog key of a setting kind's readable name.
fn kind_label(kind: superba_core::SettingKind) -> &'static str {
    use superba_core::SettingKind as K;
    match kind {
        K::Boolean => "cli.settings.kinds.boolean",
        K::String => "cli.settings.kinds.string",
        K::Integer => "cli.settings.kinds.integer",
        K::Number => "cli.settings.kinds.number",
        K::Array => "cli.settings.kinds.array",
        K::Table => "cli.settings.kinds.table",
        K::Map => "cli.settings.kinds.map",
        K::Enum => "cli.settings.kinds.enum",
        K::Union | K::Any | K::Recursive => "cli.settings.kinds.union",
        K::BoolOrTable => "cli.settings.kinds.bool_or_table",
        _ => "cli.settings.kinds.unknown",
    }
}
