//! `spb claude` (user bug 79): Claude Code's provider and Anthropic endpoint,
//! its models (slots, capabilities, the `/model` rows), applying and
//! restoring the keys Superba owns in `settings.json`, diagnostics, the
//! endpoint check and the `settings.json` editor.

use std::collections::BTreeSet;

use superba_core::claude::{
    AnthropicPreset, anthropic_preset, anthropic_presets, capabilities_text, parse_capabilities,
};
use superba_core::{
    AnthropicAuth, AnthropicEndpoint, ClaudeConfig, ClaudeModels, ClaudeModelsRequest, ClaudePlan,
    ClaudePlanKind, ClaudeSlot, ClaudeView, Error, PickerRow, ProfileId, SlotModel,
};

use super::{Result, usage};
use crate::text::text;
use crate::{
    ClaudeCommand, ClaudeConfigCommand, CodexFileArg, ConfigCommand, Output, Runtime, exit,
};

pub(super) async fn run(command: &ClaudeCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        ClaudeCommand::Status => status(rt, out).await,
        ClaudeCommand::Provider { id, follow_codex } => {
            provider(id.as_deref(), *follow_codex, rt, out).await
        }
        ClaudeCommand::Route { route } => set_route(route.as_deref(), rt, out).await,
        ClaudeCommand::Endpoint {
            value,
            provider,
            auth,
            reset,
        } => {
            endpoint(
                value.as_deref(),
                provider.as_deref(),
                auth.as_deref(),
                *reset,
                rt,
                out,
            )
            .await
        }
        ClaudeCommand::Presets => presets(out),
        ClaudeCommand::Models { pull, add, remove } => models(*pull, add, remove, rt, out).await,
        ClaudeCommand::Slot {
            slot,
            model,
            name,
            description,
            clear,
        } => {
            set_slot(
                slot,
                model.as_deref(),
                name.as_deref(),
                description.as_deref(),
                *clear,
                rt,
                out,
            )
            .await
        }
        ClaudeCommand::Caps { model, list } => caps(model, list.as_deref(), rt, out).await,
        ClaudeCommand::Picker {
            action,
            model,
            label,
            description,
            behaves_as,
        } => {
            picker(
                action,
                model.as_deref(),
                PickerRow {
                    model: String::new(),
                    label: label.clone(),
                    description: description.clone(),
                    behaves_as: behaves_as.clone(),
                },
                rt,
                out,
            )
            .await
        }
        ClaudeCommand::Apply { yes, take_over } => {
            apply(ClaudePlanKind::Apply, *yes, *take_over, rt, out).await
        }
        ClaudeCommand::Restore { yes, take_over } => {
            apply(ClaudePlanKind::Restore, *yes, *take_over, rt, out).await
        }
        ClaudeCommand::Probe { model } => probe(model.clone(), rt, out).await,
        ClaudeCommand::Doctor => doctor(rt, out).await,
        ClaudeCommand::Config { command } => {
            let config = match command {
                ClaudeConfigCommand::Edit {
                    yes,
                    force,
                    show_secrets,
                } => ConfigCommand::Edit {
                    file: CodexFileArg::Claude,
                    host: None,
                    yes: *yes,
                    force: *force,
                    show_secrets: *show_secrets,
                },
                ClaudeConfigCommand::Check => ConfigCommand::Check {
                    file: CodexFileArg::Claude,
                    host: None,
                },
                ClaudeConfigCommand::Backups => ConfigCommand::Backups {
                    file: Some(CodexFileArg::Claude),
                    host: None,
                },
                ClaudeConfigCommand::Restore {
                    id,
                    yes,
                    force,
                    show_secrets,
                } => ConfigCommand::Restore {
                    id: id.clone(),
                    yes: *yes,
                    force: *force,
                    show_secrets: *show_secrets,
                },
            };
            super::config::run(&config, rt, out).await
        }
    }
}

fn preset_label(out: &Output, p: &AnthropicPreset) -> String {
    out.render(&p.label())
}

fn provider_name(view: &ClaudeView, set: &superba_core::ProfileSet) -> Option<String> {
    let id = view.provider.as_ref()?;
    Some(
        set.profile(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string()),
    )
}

/// The provider a model command changes: the one Claude Code uses.
fn current_provider(view: &ClaudeView, out: &Output) -> Result<ProfileId> {
    view.provider
        .clone()
        .ok_or_else(|| usage(out, text("cli.claude.no_provider")))
}

async fn edit_models(
    rt: &Runtime,
    out: &mut Output,
    change: impl FnOnce(&mut ClaudeModels) -> std::result::Result<(), Error>,
) -> Result<ClaudeView> {
    let view = rt.service.claude_view().await?;
    let id = current_provider(&view, out)?;
    let mut config = view.config.clone();
    let mut models = config.models_of(&id);
    change(&mut models)?;
    config.models.insert(id, models);
    Ok(rt.service.save_claude_config(config).await?)
}

async fn status(rt: &Runtime, out: &mut Output) -> Result {
    let view = rt.service.claude_view().await?;
    let set = rt.service.profile_set().await?;
    print_status(&view, &set, out)?;
    print_router(&view, rt, out).await?;
    out.value(&view)?;
    Ok(exit::OK)
}

/// The route, and through the router the providers it uses for Claude Code
/// with why one cannot serve it.
async fn print_router(
    view: &ClaudeView,
    rt: &Runtime,
    out: &mut Output,
) -> superba_core::Result<()> {
    let route = match (view.config.route, &view.router_base_url) {
        (superba_core::ClaudeRoute::Router, Some(url)) => {
            out.render(&text("cli.claude.route.router_at").arg("url", url.as_str()))
        }
        (superba_core::ClaudeRoute::Router, None) => out.label("cli.claude.route.router_missing"),
        _ => out.label("cli.claude.route.direct"),
    };
    out.field("cli.claude.field.route", route)?;
    if view.config.route != superba_core::ClaudeRoute::Router {
        return Ok(());
    }
    let Some(claude) = rt.service.router_status().await.ok().and_then(|r| r.claude) else {
        return Ok(());
    };
    for m in &claude.members {
        let state = match (m.ineligible, &m.endpoint) {
            (Some(reason), _) => out.render(&reason.text()),
            (None, Some(e)) => e.clone(),
            (None, None) => String::new(),
        };
        let mark = if m.ineligible.is_some() { "✗" } else { "✓" };
        let role = if m.preferred {
            format!(" ({})", out.label("cli.claude.route.preferred"))
        } else {
            String::new()
        };
        out.data_line(&format!("  {mark} {}{role}  {state}", m.name))?;
    }
    Ok(())
}

async fn set_route(route: Option<&str>, rt: &Runtime, out: &mut Output) -> Result {
    let view = rt.service.claude_view().await?;
    let set = rt.service.profile_set().await?;
    let Some(route) = route else {
        print_router(&view, rt, out).await?;
        out.value(&view)?;
        return Ok(exit::OK);
    };
    let route = if route == "router" {
        superba_core::ClaudeRoute::Router
    } else {
        superba_core::ClaudeRoute::Direct
    };
    if route == superba_core::ClaudeRoute::Router && view.router_base_url.is_none() {
        return Err(usage(out, text("cli.claude.route.router_missing")));
    }
    let mut config = view.config.clone();
    config.route = route;
    let view = rt.service.save_claude_config(config).await?;
    print_status(&view, &set, out)?;
    print_router(&view, rt, out).await?;
    out.line(text("cli.claude.saved_apply"))?;
    out.value(&view)?;
    Ok(exit::OK)
}

fn print_status(
    view: &ClaudeView,
    set: &superba_core::ProfileSet,
    out: &mut Output,
) -> superba_core::Result<()> {
    match &view.install {
        Some(i) => out.field(
            "cli.claude.field.program",
            format!(
                "{} ({})",
                i.version.as_deref().unwrap_or("?"),
                superba_core::paths::display_path(&i.binary)
            ),
        )?,
        None => out.field(
            "cli.claude.field.program",
            out.label("cli.claude.not_found"),
        )?,
    }
    let file = match &view.file {
        superba_core::ClaudeFileStatus::Absent => out.label("cli.claude.file.absent"),
        superba_core::ClaudeFileStatus::Valid => out.label("cli.claude.file.valid"),
        _ => out.label("cli.claude.file.invalid"),
    };
    out.field(
        "cli.claude.field.settings",
        format!(
            "{} ({file})",
            superba_core::paths::display_path(&view.settings_path)
        ),
    )?;
    let provider = match provider_name(view, set) {
        Some(n) if view.follows_codex => out.render(&text("cli.claude.follows").arg("name", n)),
        Some(n) => n,
        None => out.label("cli.claude.no_provider_short"),
    };
    out.field("cli.claude.field.provider", provider)?;
    if view.endpoint.is_none() && view.provider.is_some() {
        out.line(superba_core::ClaudeIneligible::NoAnthropicEndpoint.text())?;
    }
    match &view.endpoint {
        Some(e) => {
            let preset = e
                .preset
                .as_deref()
                .and_then(anthropic_preset)
                .map(|p| format!(" · {}", preset_label(out, &p)))
                .unwrap_or_default();
            out.field(
                "cli.claude.field.endpoint",
                format!("{}{preset} ({})", e.base_text(), e.auth.as_str()),
            )?;
        }
        None => out.field(
            "cli.claude.field.endpoint",
            out.label("cli.claude.no_endpoint"),
        )?,
    }
    if let Some(id) = &view.provider {
        let m = view.config.models_of(id);
        for slot in ClaudeSlot::ALL {
            if let Some(s) = m.slots.get(&slot) {
                let caps = m.capabilities_of(&s.model, None);
                out.data_line(&format!(
                    "  {:<8} {}  [{}]",
                    slot.as_str(),
                    s.model,
                    capabilities_text(&caps)
                ))?;
            }
        }
        for (key, value) in [
            ("cli.claude.field.main", &m.main),
            ("cli.claude.field.subagent", &m.subagent),
            ("cli.claude.field.background", &m.small_fast),
        ] {
            if let Some(v) = value {
                out.field(key, v)?;
            }
        }
        if !m.picker.is_empty() {
            out.field(
                "cli.claude.field.picker",
                m.picker
                    .iter()
                    .map(|r| r.model.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )?;
        }
    }
    match &view.applied {
        Some(a) => {
            out.line(
                text("cli.claude.applied")
                    .arg("generation", a.generation)
                    .arg("at", out.time(a.at))
                    .arg("n", a.owned.len()),
            )?;
            if !a.drift.is_empty() {
                out.line(text("cli.claude.drift").arg("keys", a.drift.join(", ")))?;
            }
        }
        None => out.line(text("cli.claude.not_applied"))?,
    }
    if !view.pending.is_empty() {
        out.line(text("cli.claude.pending").arg("n", view.pending.len()))?;
    }
    for n in &view.notes {
        out.line(n.text.clone())?;
    }
    Ok(())
}

async fn provider(id: Option<&str>, follow: bool, rt: &Runtime, out: &mut Output) -> Result {
    let view = rt.service.claude_view().await?;
    let set = rt.service.profile_set().await?;
    let Some(id) = id.filter(|_| !follow) else {
        if follow {
            let mut config = view.config.clone();
            config.provider = None;
            let view = rt.service.save_claude_config(config).await?;
            print_status(&view, &set, out)?;
            out.value(&view)?;
            return Ok(exit::OK);
        }
        // List the providers and whether Claude Code can use them.
        let sites = rt.service.sites().await?;
        let rows = set
            .profiles
            .iter()
            .filter(|p| p.builtin.is_none())
            .map(|p| {
                let site = sites.iter().find(|s| s.id == p.site_id);
                let endpoint = site.and_then(|s| superba_core::claude::effective_endpoint(s, p));
                vec![
                    p.id.to_string(),
                    p.name.clone(),
                    endpoint
                        .map(|e| e.base_text())
                        .unwrap_or_else(|| out.label("cli.claude.no_anthropic")),
                    out.boolean(view.provider.as_ref() == Some(&p.id)),
                ]
            })
            .collect::<Vec<_>>();
        out.table(
            &[
                "cli.claude.col.id",
                "cli.claude.col.name",
                "cli.claude.col.endpoint",
                "cli.claude.col.current",
            ],
            rows,
        )?;
        out.value(&view)?;
        return Ok(exit::OK);
    };
    let pid = ProfileId::new(id);
    let profile = set
        .profile(&pid)
        .ok_or_else(|| usage(out, text("cli.claude.unknown_provider").arg("id", id)))?;
    let sites = rt.service.sites().await?;
    let usable = sites
        .iter()
        .find(|s| s.id == profile.site_id)
        .and_then(|s| superba_core::claude::effective_endpoint(s, profile))
        .is_some();
    if !usable {
        return Err(usage(out, text("cli.claude.no_anthropic_reason")));
    }
    let mut config: ClaudeConfig = view.config.clone();
    config.provider = Some(pid);
    let view = rt.service.save_claude_config(config).await?;
    print_status(&view, &set, out)?;
    out.value(&view)?;
    Ok(exit::OK)
}

async fn endpoint(
    value: Option<&str>,
    provider: Option<&str>,
    auth: Option<&str>,
    reset: bool,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let view = rt.service.claude_view().await?;
    let set = rt.service.profile_set().await?;
    let id = match provider {
        Some(p) => ProfileId::new(p),
        None => current_provider(&view, out)?,
    };
    let mut profile = set
        .profile(&id)
        .cloned()
        .ok_or_else(|| usage(out, text("cli.claude.unknown_provider").arg("id", &id)))?;
    if reset {
        profile.anthropic = None;
    } else if let Some(v) = value {
        let sites = rt.service.sites().await?;
        let site = sites.iter().find(|s| s.id == profile.site_id);
        let origin = site.and_then(|s| superba_core::claude::profile_origin(s, &profile));
        let mut e = match anthropic_preset(v) {
            Some(p) => p
                .endpoint(origin.as_ref())
                .ok_or_else(|| usage(out, text("cli.claude.preset_needs_line")))?,
            None => {
                let url = AnthropicEndpoint::parse_base(v).map_err(|key| {
                    usage(
                        out,
                        superba_core::LocalizedText::new(key, "invalid endpoint"),
                    )
                })?;
                AnthropicEndpoint::new(url)
            }
        };
        if let Some(a) = auth.and_then(AnthropicAuth::parse) {
            e.auth = a;
        }
        profile.anthropic = Some(e);
    } else if let Some(a) = auth.and_then(AnthropicAuth::parse) {
        let mut e = view
            .endpoint
            .clone()
            .ok_or_else(|| usage(out, text("cli.claude.no_endpoint")))?;
        e.auth = a;
        profile.anthropic = Some(e);
    } else {
        // Show it.
        match &view.endpoint {
            Some(e) => out.field("cli.claude.field.endpoint", e.base_text())?,
            None => out.line(text("cli.claude.no_endpoint"))?,
        }
        out.value(&view.endpoint)?;
        return Ok(exit::OK);
    }
    rt.service.save_profile(profile, None).await?;
    let view = rt.service.claude_view().await?;
    print_status(&view, &set, out)?;
    out.value(&view)?;
    Ok(exit::OK)
}

fn presets(out: &mut Output) -> Result {
    let list = anthropic_presets();
    let rows = list
        .iter()
        .map(|p| {
            vec![
                p.id.to_owned(),
                preset_label(out, p),
                p.base_url
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("<line>{}", p.path)),
                p.doc_url.to_owned(),
            ]
        })
        .collect::<Vec<_>>();
    out.table(
        &[
            "cli.claude.col.id",
            "cli.claude.col.name",
            "cli.claude.col.endpoint",
            "cli.claude.col.docs",
        ],
        rows,
    )?;
    let ids: Vec<_> = list.iter().map(|p| p.id).collect();
    out.value(&ids)?;
    Ok(exit::OK)
}

async fn models(
    pull: bool,
    add: &[String],
    remove: &[String],
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    if !add.is_empty() || !remove.is_empty() {
        let (add, remove) = (add.to_vec(), remove.to_vec());
        edit_models(rt, out, |m| {
            for a in add {
                let a = a.trim().to_owned();
                if !a.is_empty() && !m.extra_models.contains(&a) {
                    m.extra_models.push(a);
                }
            }
            m.extra_models.retain(|x| !remove.contains(x));
            Ok(())
        })
        .await?;
    }
    let view = rt
        .service
        .claude_models(ClaudeModelsRequest {
            provider: None,
            refresh: pull,
        })
        .await?;
    for s in &view.sources {
        match &s.error {
            Some(e) => out.line(
                text("cli.claude.source_failed")
                    .arg("list", s.kind.as_str())
                    .arg("error", e.to_string()),
            )?,
            None => out.line(
                text("cli.claude.source_ok")
                    .arg("list", s.kind.as_str())
                    .arg("n", s.count)
                    .arg("path", s.path.as_deref().unwrap_or("")),
            )?,
        }
    }
    let rows = view
        .rows
        .iter()
        .map(|r| {
            let lists = if r.manual {
                out.label("cli.claude.list.manual")
            } else {
                r.lists
                    .iter()
                    .map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join("+")
            };
            vec![
                r.id.clone(),
                lists,
                if r.usable {
                    String::new()
                } else {
                    out.label("cli.claude.codex_only")
                },
                capabilities_text(&r.suggested),
            ]
        })
        .collect::<Vec<_>>();
    out.table(
        &[
            "cli.claude.col.model",
            "cli.claude.col.lists",
            "cli.claude.col.usable",
            "cli.claude.col.suggested",
        ],
        rows,
    )?;
    out.value(&view)?;
    Ok(exit::OK)
}

async fn set_slot(
    slot: &str,
    model: Option<&str>,
    name: Option<&str>,
    description: Option<&str>,
    clear: bool,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    if model.is_none() && !clear {
        return Err(usage(out, text("cli.claude.slot_needs_model")));
    }
    let lower = slot.trim().to_ascii_lowercase();
    let pseudo = matches!(lower.as_str(), "main" | "subagent" | "background");
    let parsed = ClaudeSlot::parse(&lower);
    if parsed.is_none() && !pseudo {
        return Err(usage(
            out,
            text("cli.claude.unknown_slot").arg("slot", slot),
        ));
    }
    let (model, name, description) = (
        model.map(str::to_owned),
        name.map(str::to_owned),
        description.map(str::to_owned),
    );
    let view = edit_models(rt, out, move |m| {
        match (lower.as_str(), parsed) {
            ("main", _) => m.main = model,
            ("subagent", _) => m.subagent = model,
            ("background", _) => m.small_fast = model,
            (_, Some(slot)) => match model {
                Some(model) => {
                    m.slots.insert(
                        slot,
                        SlotModel {
                            model,
                            name,
                            description,
                        },
                    );
                }
                None => {
                    m.slots.remove(&slot);
                }
            },
            _ => {}
        }
        Ok(())
    })
    .await?;
    out.line(text("cli.claude.saved_apply"))?;
    out.value(&view)?;
    Ok(exit::OK)
}

async fn caps(model: &str, list: Option<&str>, rt: &Runtime, out: &mut Output) -> Result {
    let model = model.trim().to_owned();
    let Some(list) = list else {
        let view = rt.service.claude_view().await?;
        let id = current_provider(&view, out)?;
        let caps = view.config.models_of(&id).capabilities_of(&model, None);
        out.field("cli.claude.field.caps", capabilities_text(&caps))?;
        out.value(&caps)?;
        return Ok(exit::OK);
    };
    let set: Option<BTreeSet<_>> = match list.trim() {
        "suggested" | "auto" => None,
        "none" | "" => Some(BTreeSet::new()),
        l => {
            let (caps, unknown) = parse_capabilities(l);
            if !unknown.is_empty() {
                return Err(usage(
                    out,
                    text("cli.claude.unknown_caps").arg("names", unknown.join(", ")),
                ));
            }
            Some(caps)
        }
    };
    let view = edit_models(rt, out, move |m| {
        match set {
            Some(c) => {
                m.capabilities.insert(model, c);
            }
            None => {
                m.capabilities.remove(&model);
            }
        }
        Ok(())
    })
    .await?;
    out.line(text("cli.claude.saved_apply"))?;
    out.value(&view)?;
    Ok(exit::OK)
}

async fn picker(
    action: &str,
    model: Option<&str>,
    row: PickerRow,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let model = model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned);
    if matches!(action, "add" | "remove") && model.is_none() {
        return Err(usage(out, text("cli.claude.slot_needs_model")));
    }
    let action = action.to_owned();
    let view = edit_models(rt, out, move |m| {
        match action.as_str() {
            "add" => {
                let model = model.unwrap_or_default();
                m.add_to_picker(&model);
                if let Some(r) = m.picker.iter_mut().find(|r| r.model == model) {
                    if row.label.is_some() {
                        r.label = row.label;
                    }
                    if row.description.is_some() {
                        r.description = row.description;
                    }
                    if row.behaves_as.is_some() {
                        r.behaves_as = row.behaves_as;
                    }
                }
            }
            "remove" => {
                let model = model.unwrap_or_default();
                m.picker.retain(|r| r.model != model);
            }
            "clear" => m.picker.clear(),
            "replace-builtin" => m.replace_builtin = true,
            "keep-builtin" => m.replace_builtin = false,
            _ => {}
        }
        Ok(())
    })
    .await?;
    out.line(text("cli.claude.saved_apply"))?;
    out.value(&view)?;
    Ok(exit::OK)
}

fn print_plan(plan: &ClaudePlan, out: &mut Output) -> superba_core::Result<()> {
    if plan.changes.is_empty() {
        out.line(text("cli.claude.plan_empty"))?;
    }
    let none = out.none();
    let rows = plan
        .changes
        .iter()
        .map(|c| {
            vec![
                c.path.clone(),
                c.before.clone().unwrap_or_else(|| none.clone()),
                c.after.clone().unwrap_or_else(|| none.clone()),
                if c.drift {
                    out.label("cli.claude.drift_mark")
                } else {
                    String::new()
                },
            ]
        })
        .collect::<Vec<_>>();
    if !rows.is_empty() {
        out.table(
            &[
                "cli.claude.col.key",
                "cli.claude.col.before",
                "cli.claude.col.after",
                "cli.claude.col.note",
            ],
            rows,
        )?;
    }
    for n in &plan.notes {
        out.line(n.text.clone())?;
    }
    Ok(())
}

async fn apply(
    kind: ClaudePlanKind,
    yes: bool,
    take_over: bool,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let plan = rt.service.claude_plan(kind).await?;
    print_plan(&plan, out)?;
    if plan.is_empty() {
        out.value(&plan)?;
        return Ok(exit::OK);
    }
    let drift = plan.drift().count();
    if drift > 0 && !take_over {
        out.line(text("cli.claude.drift_kept").arg("n", drift))?;
    }
    if !yes {
        let interactive = rt.interactive && !out.json;
        if !interactive {
            out.line(text("cli.claude.confirm_needed"))?;
            out.value(&plan)?;
            return Ok(exit::NEEDS_CONFIRMATION);
        }
        out.line(text("cli.claude.confirm"))?;
        let answer = (rt.read_stdin_line)()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !matches!(answer.as_str(), "y" | "yes" | "是") {
            out.line(text("cli.claude.cancelled"))?;
            return Ok(exit::CANCELLED);
        }
    }
    let report = rt.service.claude_apply(plan, take_over).await?;
    out.line(
        text(match kind {
            ClaudePlanKind::Restore => "cli.claude.restored",
            _ => "cli.claude.applied_now",
        })
        .arg("n", report.changed.len())
        .arg(
            "path",
            superba_core::paths::display_path(&report.settings_path),
        ),
    )?;
    if let Some(b) = &report.backup {
        out.line(text("cli.claude.backup").arg("id", &b.id))?;
    }
    for n in &report.notes {
        out.line(n.text.clone())?;
    }
    out.value(&report)?;
    Ok(exit::OK)
}

async fn probe(model: Option<String>, rt: &Runtime, out: &mut Output) -> Result {
    let p = rt.service.claude_probe(model).await?;
    match (p.ok, p.status) {
        (true, _) => out.line(
            text("cli.claude.probe_ok")
                .arg("model", &p.model)
                .arg("ms", p.latency_ms)
                .arg("url", &p.url),
        )?,
        (false, status) => out.line(
            text("cli.claude.probe_failed")
                .arg("model", &p.model)
                .arg(
                    "status",
                    status.map_or_else(|| "-".to_owned(), |s| s.to_string()),
                )
                .arg("error", p.message.as_deref().unwrap_or("")),
        )?,
    }
    out.value(&p)?;
    Ok(if p.ok { exit::OK } else { exit::ERROR })
}

async fn doctor(rt: &Runtime, out: &mut Output) -> Result {
    let d = rt.service.claude_diagnostics().await?;
    for f in &d.findings {
        out.data_line(&format!(
            "[{}] {}",
            out.severity(f.severity),
            out.render(&f.title)
        ))?;
        if let Some(detail) = &f.detail {
            out.data_line(&format!("    {}", out.render(detail)))?;
        }
    }
    out.value(&d)?;
    Ok(exit::OK)
}
