//! `spb models` and `spb speed`: a profile's model list (from the provider, the
//! feed or the local Codex), its default model and effort, the all-models
//! context window and compaction, per-model overrides, the picker mode and the
//! speed tier. Edits are stored in the profile; when the profile is the one
//! applied to this computer, the switch to it runs right away (a hot restart),
//! else it takes effect at the next switch. `--dry-run` stores nothing and shows
//! what would change.
//!
//! Context windows and compaction thresholds are shown as Codex uses them (the
//! model's own value, the all-models value or the model's default, with the
//! source), the same numbers the Models page shows, and checked with the same
//! bounds (`superba_core::check_*`) before anything is stored. PAT and API-key
//! profiles both apply per-model values through a generated catalog.

use superba_core::{
    CompactLimit, LimitError, LimitSource, ModelDefaults, ModelLimits, ModelOverride, ModelPicker,
    ModelSpec, ModelsEdit, ModelsView, ModelsViewRequest, Profile, ProfileId, SpeedTier,
    SwitchIntent, check_all_models, check_model_override, model_limits, parse_token_count,
};
use superba_switch::models::{CatalogReason, catalog_reasons, override_needs_catalog};

use super::{Result, execute, request, usage};
use crate::{
    ExecArgs, ModelsCommand, ModelsDefaultArgs, ModelsResetArgs, ModelsSetArgs, OnOff, Output,
    Runtime, SpeedArgs, exit, output::grouped, text::text,
};

pub(super) async fn run(
    command: Option<&ModelsCommand>,
    profile: Option<&str>,
    refresh: bool,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let profile_id = profile.map(ProfileId::new);
    match command {
        None | Some(ModelsCommand::List) => {
            let view = rt
                .service
                .models_view(ModelsViewRequest {
                    profile_id,
                    refresh,
                })
                .await?;
            render(&view, out)?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        Some(ModelsCommand::Default(args)) => default(args, profile_id, rt, out).await,
        Some(ModelsCommand::Set(args)) => set(args, profile_id, rt, out).await,
        Some(ModelsCommand::Reset(args)) => reset(args, profile_id, rt, out).await,
        Some(ModelsCommand::Picker { state, exec }) => {
            let edit = ModelsEdit {
                profile_id,
                picker: Some(match state {
                    OnOff::On => ModelPicker::Provider,
                    OnOff::Off => ModelPicker::CodexDefault,
                }),
                ..ModelsEdit::default()
            };
            save(edit, exec, rt, out).await
        }
        Some(ModelsCommand::Mode { mode, dry_run }) => {
            let mut settings = rt.service.profile_set().await?.settings.router.catalog;
            let before = settings.mode;
            if let Some(mode) = mode {
                settings.mode = (*mode).into();
                if !dry_run && settings.mode != before {
                    rt.service.save_router_catalog(settings.clone()).await?;
                }
            }
            out.field("cli.models.mode", settings.mode.as_str())?;
            out.value(&serde_json::json!({
                "mode": settings.mode,
                "changed": settings.mode != before,
                "dry_run": dry_run,
            }))?;
            Ok(exit::OK)
        }
    }
}

/// A token count (`400k`, `1m`, `272000`).
fn tokens(out: &Output, value: &str) -> Result<u64> {
    parse_token_count(value)
        .ok_or_else(|| usage(out, text("cli.models.bad_tokens").arg("value", value)))
}

/// A compaction threshold: tokens, or a percent of `window`.
fn compact(out: &Output, value: &str, window: Option<u64>) -> Result<u64> {
    match CompactLimit::parse(value) {
        Some(CompactLimit::Tokens(n)) => Ok(n),
        Some(CompactLimit::Percent(p)) => match window {
            Some(w) => Ok(CompactLimit::Percent(p).tokens(w)),
            None => Err(usage(out, text("cli.models.percent_needs_window"))),
        },
        None => Err(usage(
            out,
            text("cli.models.bad_tokens").arg("value", value),
        )),
    }
}

async fn view_for(
    profile_id: Option<ProfileId>,
    rt: &Runtime,
) -> std::result::Result<ModelsView, superba_core::Error> {
    rt.service
        .models_view(ModelsViewRequest {
            profile_id,
            refresh: false,
        })
        .await
}

/// A window / compaction error for `model` (the all-models row when `None`).
fn limit_error(out: &Output, model: Option<&str>, e: LimitError) -> super::Failure {
    let model = model.map_or_else(|| out.label("cli.models.all_models"), str::to_owned);
    let t = match e {
        LimitError::WindowTooSmall { min } => text("cli.models.too_small").arg("min", grouped(min)),
        LimitError::WindowOverMax { max } => text("cli.models.over_max").arg("max", grouped(max)),
        LimitError::CompactTooSmall { min } => {
            text("cli.models.compact_too_small").arg("min", grouped(min))
        }
        LimitError::CompactNotBelowWindow { window } => {
            text("cli.models.compact_not_below").arg("window", grouped(window))
        }
    };
    usage(out, t.arg("model", model))
}

/// The window and compaction Codex uses for `spec` with the view's values (a
/// model's own values only where they reach Codex).
fn limits(view: &ModelsView, spec: &ModelSpec) -> ModelLimits {
    let o = view.overrides.get(&spec.slug).filter(|_| view.per_model);
    model_limits(spec, o, &view.defaults)
}

async fn default(
    args: &ModelsDefaultArgs,
    profile_id: Option<ProfileId>,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let view = view_for(profile_id, rt).await?;
    let mut defaults = view.defaults.clone();
    if let Some(slug) = &args.slug {
        defaults.model = Some(slug.clone());
    }
    if let Some(e) = &args.effort {
        defaults.reasoning_effort = Some(e.clone());
    }
    if args.clear {
        defaults.context_window = None;
        defaults.auto_compact = None;
    }
    if let Some(c) = &args.context {
        defaults.context_window = Some(tokens(out, c)?);
    }
    if let Some(c) = &args.compact {
        // A share of the all-models window (as on the Models page): without one
        // each model has its own window, so a percentage has no single value.
        defaults.auto_compact = Some(compact(out, c, defaults.context_window)?);
    }
    check_all_models(&defaults).map_err(|e| limit_error(out, None, e))?;
    let edit = ModelsEdit {
        profile_id: Some(view.profile_id.clone()),
        defaults: Some(defaults),
        ..ModelsEdit::default()
    };
    save(edit, &args.exec, rt, out).await
}

async fn set(
    args: &ModelsSetArgs,
    profile_id: Option<ProfileId>,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let view = view_for(profile_id, rt).await?;
    if !view.per_model
        && (args.context.is_some()
            || args.compact.is_some()
            || args.name.is_some()
            || args.hide
            || args.show)
    {
        return Err(usage(out, text("cli.models.per_model_unavailable")));
    }
    let spec = view.list.model(&args.slug);
    let mut o: ModelOverride = view.overrides.get(&args.slug).cloned().unwrap_or_default();
    if let Some(c) = &args.context {
        o.context_window = Some(tokens(out, c)?);
    }
    if let Some(c) = &args.compact {
        // A share of the window the model ends up with (its own, else all
        // models', else its default), as on the Models page.
        let window = match spec {
            Some(m) => model_limits(m, Some(&o), &view.defaults).window,
            None => o.context_window.or(view.defaults.context_window),
        };
        o.auto_compact = Some(compact(out, c, window)?);
    }
    check_model_override(spec, &o, &view.defaults)
        .map_err(|e| limit_error(out, Some(&args.slug), e))?;
    if let Some(n) = &args.name {
        o.display_name = (!n.trim().is_empty()).then(|| n.clone());
    }
    if args.hide {
        o.hidden = true;
    }
    if args.show {
        o.hidden = false;
    }
    if let Some(u) = &args.upstream_name {
        o.upstream_name = Some(u.clone());
    }
    let edit = ModelsEdit {
        profile_id: Some(view.profile_id.clone()),
        set: [(args.slug.clone(), o)].into(),
        ..ModelsEdit::default()
    };
    save(edit, &args.exec, rt, out).await
}

async fn reset(
    args: &ModelsResetArgs,
    profile_id: Option<ProfileId>,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let view = view_for(profile_id, rt).await?;
    let edit = ModelsEdit {
        profile_id: Some(view.profile_id.clone()),
        reset: args.slug.iter().cloned().collect(),
        reset_all: args.all,
        ..ModelsEdit::default()
    };
    save(edit, &args.exec, rt, out).await
}

pub(super) async fn speed(args: &SpeedArgs, rt: &Runtime, out: &mut Output) -> Result {
    let profile_id = args.profile.as_deref().map(ProfileId::new);
    let view = view_for(profile_id, rt).await?;
    let Some(tier) = args.tier else {
        // Show the profile's tier and where it applies.
        let current = view.defaults.speed.unwrap_or_default();
        out.field("cli.speed.profile", &view.profile_id)?;
        out.field("cli.speed.tier", out.render(&current.text()))?;
        render_speed(&view, out)?;
        out.value(&serde_json::json!({
            "profile_id": view.profile_id,
            "speed": current,
            "service_tier": current.config_value(),
            "support": view.speed,
        }))?;
        return Ok(exit::OK);
    };
    let tier: SpeedTier = tier.into();
    if view.read_only {
        return Err(usage(out, text("cli.speed.official")));
    }
    if !view.speed.offered.contains(&tier) {
        out.progress(text("cli.speed.not_offered").arg_text("tier", tier.text()))?;
    }
    let defaults = ModelDefaults {
        speed: Some(tier),
        ..view.defaults.clone()
    };
    let edit = ModelsEdit {
        profile_id: Some(view.profile_id.clone()),
        defaults: Some(defaults),
        ..ModelsEdit::default()
    };
    save(edit, &args.exec, rt, out).await
}

/// The profile after `edit` (pure; the service validates on save).
fn edited(mut p: Profile, edit: &ModelsEdit) -> Profile {
    if let Some(d) = &edit.defaults {
        p.model = d.clone();
    }
    if edit.reset_all {
        p.model_overrides.clear();
    }
    for slug in &edit.reset {
        p.model_overrides.remove(slug);
    }
    for (slug, o) in &edit.set {
        let mut stored = o.clone();
        stored.upstream_name = None;
        if stored.is_empty() {
            p.model_overrides.remove(slug);
        } else {
            p.model_overrides.insert(slug.clone(), stored);
        }
    }
    if let Some(picker) = edit.picker {
        p.model_picker = picker;
    }
    p
}

fn reason_key(r: CatalogReason) -> &'static str {
    match r {
        CatalogReason::Overrides => "cli.models.reason.overrides",
        CatalogReason::ProviderPicker => "cli.models.reason.picker",
        CatalogReason::Ultrafast => "cli.models.reason.ultrafast",
        CatalogReason::TierSuffix => "cli.models.reason.suffix",
    }
}

/// Store the edit (or show it with `--dry-run`), then switch to the profile
/// when it is the applied one.
async fn save(edit: ModelsEdit, exec: &ExecArgs, rt: &Runtime, out: &mut Output) -> Result {
    let set = rt.service.profile_set().await?;
    let id = edit
        .profile_id
        .clone()
        .ok_or_else(|| usage(out, text("cli.error.choose_profile")))?;
    let profile = set
        .profile(&id)
        .cloned()
        .ok_or_else(|| super::missing(out, text("cli.error.profile_missing").arg("id", &id)))?;
    let site = rt
        .service
        .sites()
        .await?
        .into_iter()
        .find(|s| s.id == profile.site_id);
    let overview = rt.service.overview().await?;
    let applied = overview.active.as_ref().and_then(|a| a.profile_id.as_ref()) == Some(&id);
    let auth = overview
        .active
        .as_ref()
        .filter(|_| applied)
        .and_then(|a| a.auth_mode)
        .unwrap_or(superba_core::AuthMode::ApiKey);
    if exec.dry_run {
        let after = edited(profile.clone(), &edit);
        let speed = after.model.speed.unwrap_or_default();
        out.line(text("cli.models.dry_run").arg("profile", &id))?;
        out.field("cli.models.service_tier", speed.config_value())?;
        let reasons = site
            .as_ref()
            .map(|s| catalog_reasons(&after, s, auth))
            .unwrap_or_default();
        if reasons.is_empty() {
            out.line(text("cli.models.no_catalog"))?;
        } else {
            let why: Vec<String> = reasons.iter().map(|r| out.label(reason_key(*r))).collect();
            out.line(text("cli.models.catalog_needed").arg("reasons", why.join(", ")))?;
        }
        for (slug, o) in &after.model_overrides {
            if override_needs_catalog(o) {
                out.line(override_line(slug, o))?;
            }
        }
        out.value(&serde_json::json!({
            "outcome": "dry_run",
            "profile": after,
            "service_tier": speed.config_value(),
            "catalog_reasons": reasons,
            "applied": applied,
        }))?;
        return Ok(exit::OK);
    }
    let report = rt.service.save_models(edit).await?;
    for n in &report.notes {
        out.notice(n)?;
    }
    if applied {
        out.line(text("cli.models.applying"))?;
        let request = request(
            SwitchIntent::Profile {
                profile_id: id,
                line_id: None,
                auth_mode: None,
            },
            exec,
        );
        return execute::run(request, exec, rt, out).await;
    }
    out.line(text("cli.models.next_switch"))?;
    out.value(&report)?;
    Ok(exit::OK)
}

fn override_line(slug: &str, o: &ModelOverride) -> superba_core::LocalizedText {
    text("cli.models.override")
        .arg("model", slug)
        .arg(
            "context",
            o.context_window.map(grouped).unwrap_or_else(|| "-".into()),
        )
        .arg(
            "compact",
            o.auto_compact.map(grouped).unwrap_or_else(|| "-".into()),
        )
}

fn source_label(out: &Output, source: LimitSource) -> String {
    out.label(match source {
        LimitSource::Custom => "cli.models.source_custom",
        LimitSource::AllModels => "cli.models.source_all",
        LimitSource::Model => "cli.models.source_default",
    })
}

/// `200,000 (custom)`.
fn value_text(out: &Output, value: u64, source: LimitSource) -> String {
    out.render(
        &text("cli.models.value")
            .arg("value", grouped(value))
            .arg("source", source_label(out, source)),
    )
}

/// Where a model's values stand: applied, saved but not applied yet (the
/// profile is applied; a switch applies them), next switch (another profile),
/// not used (profiles without catalog support).
fn state_key(view: &ModelsView, m: &ModelSpec, o: Option<&ModelOverride>) -> &'static str {
    let custom = o.is_some_and(|o| o.context_window.is_some() || o.auto_compact.is_some());
    if custom && !view.per_model {
        return "cli.models.state.not_used";
    }
    let Some(applied) = &view.applied else {
        return if custom {
            "cli.models.state.next_switch"
        } else {
            "cli.models.state.none"
        };
    };
    if applied.limits(m).pair() != limits(view, m).pair() {
        "cli.models.state.not_applied"
    } else if custom {
        "cli.models.state.applied"
    } else {
        "cli.models.state.none"
    }
}

fn source_text(view: &ModelsView) -> superba_core::LocalizedText {
    use superba_core::ModelListSource as S;
    match &view.list.source {
        S::Upstream { path, .. } => text("cli.models.source.upstream").arg("path", path),
        S::Feed { mirror, serial, .. } => text("cli.models.source.feed")
            .arg("mirror", mirror)
            .arg("serial", serial),
        S::Snapshot { codex_version } => {
            text("cli.models.source.snapshot").arg("version", codex_version)
        }
        S::Bundled { codex_version } => {
            text("cli.models.source.bundled").arg("version", codex_version)
        }
        S::OfficialReadOnly => text("cli.models.source.official"),
        _ => text("cli.models.source.other"),
    }
}

fn render_speed(view: &ModelsView, out: &mut Output) -> superba_core::Result<()> {
    let tiers: Vec<String> = view
        .speed
        .offered
        .iter()
        .map(|t| out.render(&t.text()))
        .collect();
    out.field("cli.speed.offered", tiers.join(", "))?;
    out.field("cli.speed.cli", out.render(&view.speed.cli.text()))?;
    out.field("cli.speed.app", out.render(&view.speed.app.text()))
}

fn render(view: &ModelsView, out: &mut Output) -> superba_core::Result<()> {
    out.field("cli.models.profile", &view.profile_id)?;
    out.field("cli.models.source_label", out.render(&source_text(view)))?;
    if let Some(reason) = &view.list.fallback_reason {
        out.field("cli.models.fallback", out.render(&reason.to_text()))?;
    }
    if let Some(catalog) = &view.router_catalog {
        out.field("cli.models.mode", catalog.settings.mode.as_str())?;
        for (alias, id) in &catalog.aliases {
            out.field("cli.models.alias", format!("{alias} → {id}"))?;
        }
    }
    let d = &view.defaults;
    out.field("cli.models.default", out.optional(d.model.as_ref()))?;
    out.field(
        "cli.models.effort",
        out.optional(d.reasoning_effort.as_ref()),
    )?;
    out.field(
        "cli.models.all_context",
        d.context_window
            .map(|w| value_text(out, w, LimitSource::Custom))
            .unwrap_or_else(|| out.none()),
    )?;
    out.field(
        "cli.models.all_compact",
        d.auto_compact
            .map(|c| value_text(out, c, LimitSource::Custom))
            .unwrap_or_else(|| out.none()),
    )?;
    out.field(
        "cli.speed.tier",
        out.render(&d.speed.unwrap_or_default().text()),
    )?;
    render_speed(view, out)?;
    if view.picker_available {
        out.field(
            "cli.models.picker",
            out.label(match view.picker {
                ModelPicker::Provider => "cli.models.picker_provider",
                _ => "cli.models.picker_codex",
            }),
        )?;
    }
    out.field("cli.models.catalog", out.render(&view.catalog_file.text()))?;
    let rows = view
        .list
        .models
        .iter()
        .map(|m| {
            let o = view.overrides.get(&m.slug);
            // What Codex uses (the same numbers as the Models page).
            let l = limits(view, m);
            let window = l
                .window
                .map(|w| value_text(out, w, l.window_source))
                .unwrap_or_else(|| out.none());
            let compact = match (l.compact, l.compact_limit) {
                (Some(c), Some(limit)) if l.clamped => out.render(
                    &text("cli.models.capped")
                        .arg("value", grouped(c))
                        .arg("source", source_label(out, l.compact_source))
                        .arg("limit", grouped(limit)),
                ),
                (Some(c), _) => value_text(out, c, l.compact_source),
                (None, _) => out.none(),
            };
            let tiers: Vec<String> = m
                .speed_tiers()
                .iter()
                .map(|t| out.render(&t.text()))
                .collect();
            let mut row = vec![
                m.slug.clone(),
                o.and_then(|o| o.display_name.clone())
                    .unwrap_or_else(|| m.label().to_owned()),
                window,
                compact,
                tiers.join(", "),
                out.label(state_key(view, m, o)),
            ];
            // Through the router: the providers of the model, in member order.
            if let Some(catalog) = &view.router_catalog {
                let names: Vec<&str> = catalog
                    .model(&m.slug)
                    .into_iter()
                    .flat_map(|r| &r.sources)
                    .map(|p| p.provider_name.as_str())
                    .collect();
                row.push(names.join(", "));
            }
            row
        })
        .collect::<Vec<_>>();
    let mut columns = vec![
        "cli.models.col.model",
        "cli.models.col.name",
        "cli.models.col.context",
        "cli.models.col.compact",
        "cli.models.col.tiers",
        "cli.models.col.state",
    ];
    if view.router_catalog.is_some() {
        columns.push("cli.models.col.providers");
    }
    out.section_table("cli.models.list", &columns, rows)?;
    for n in &view.notes {
        out.notice(n)?;
    }
    Ok(())
}
