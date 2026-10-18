//! `spb harness`: the other coding-agent harnesses (dsh, opencode, pi,
//! ZCode, WorkBuddy). `list` / `status` read; `apply` saves the given
//! settings (provider, protocol, models, route, options), then plans,
//! confirms and writes the entries Superba owns in the harness's files;
//! `restore` puts back what was there before Superba (or one whole file
//! from a backup); `endpoints` declares a provider's Chat Completions /
//! Anthropic endpoints; `token` stores a harness's router token.
//!
//! Same shape as `spb claude` (Claude Code branch): plan table with masked
//! keys and drift marks, `--yes` / `--take-over`, exit 4 when confirmation is
//! needed and nobody can answer.

use superba_core::harness::{
    DshTelemetry, HarnessApplyReport, HarnessId, HarnessPlan, HarnessPlanKind, HarnessProtocol,
    HarnessRoute, HarnessView, OpencodeMajor, ProviderProtocols, WorkBuddyEdition,
    base_url_problem, check_base_url,
};
use superba_core::{ProfileId, Secret};

use super::{Result, stdin_secret, usage};
use crate::text::text;
use crate::{HarnessApplyArgs, HarnessCommand, Output, Runtime, exit};

fn harness(out: &Output, name: &str) -> Result<HarnessId> {
    HarnessId::parse(name).ok_or_else(|| {
        usage(
            out,
            text("cli.harness.unknown").arg("name", name).arg(
                "list",
                HarnessId::ALL
                    .iter()
                    .map(|h| h.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        )
    })
}

fn url(out: &Output, s: &str) -> Result<url::Url> {
    check_base_url(s).map_err(|k| usage(out, base_url_problem(k)))
}

pub(super) async fn run(command: &HarnessCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        HarnessCommand::List => list(rt, out).await,
        HarnessCommand::Status { harness: h } => {
            let h = harness(out, h)?;
            let view = rt.service.harness_view(h).await?;
            print_status(&view, out)?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        HarnessCommand::Apply(args) => apply(args, rt, out).await,
        HarnessCommand::Restore {
            harness: h,
            yes,
            take_over,
            backup,
        } => {
            let h = harness(out, h)?;
            match backup {
                Some(id) => restore_backup(h, id, *yes, rt, out).await,
                None => execute(h, HarnessPlanKind::Restore, *yes, *take_over, rt, out).await,
            }
        }
        HarnessCommand::Backups { harness: h } => {
            let h = harness(out, h)?;
            let backups = rt.service.harness_backups(h).await?;
            if backups.is_empty() {
                out.line(text("cli.harness.no_backups"))?;
            }
            let rows = backups
                .iter()
                .map(|b| {
                    vec![
                        b.id.clone(),
                        out.time(b.at),
                        b.file.clone(),
                        out.render(&b.source.text()),
                        b.size.to_string(),
                    ]
                })
                .collect::<Vec<_>>();
            if !rows.is_empty() {
                out.table(
                    &[
                        "cli.harness.col.id",
                        "cli.harness.col.at",
                        "cli.harness.col.file",
                        "cli.harness.col.source",
                        "cli.harness.col.size",
                    ],
                    rows,
                )?;
            }
            out.value(&backups)?;
            Ok(exit::OK)
        }
        HarnessCommand::Endpoints {
            provider,
            chat,
            anthropic,
            clear,
        } => {
            let protocols = if *clear {
                ProviderProtocols::default()
            } else {
                ProviderProtocols {
                    chat: chat.as_deref().map(|u| url(out, u)).transpose()?,
                    anthropic: anthropic.as_deref().map(|u| url(out, u)).transpose()?,
                }
            };
            rt.service
                .save_provider_protocols(ProfileId::new(provider), protocols.clone())
                .await?;
            out.line(
                text(if protocols.is_empty() {
                    "cli.harness.endpoints_cleared"
                } else {
                    "cli.harness.endpoints_saved"
                })
                .arg("provider", provider),
            )?;
            out.value(&protocols)?;
            Ok(exit::OK)
        }
        HarnessCommand::Token {
            harness: h,
            stdin,
            key_env,
            clear,
        } => {
            let h = harness(out, h)?;
            let token = if *clear {
                None
            } else if let Some(var) = key_env {
                let v = rt
                    .env
                    .var_non_empty(var)
                    .ok_or_else(|| usage(out, text("cli.harness.env_empty").arg("var", var)))?;
                Some(Secret::new(v))
            } else if *stdin {
                Some(stdin_secret(rt, out)?)
            } else {
                return Err(usage(out, text("cli.harness.token_source")));
            };
            let cleared = token.is_none();
            let view = rt.service.set_harness_router_token(h, token).await?;
            out.line(text(if cleared {
                "cli.harness.token_cleared"
            } else {
                "cli.harness.token_saved"
            }))?;
            out.value(&view)?;
            Ok(exit::OK)
        }
    }
}

async fn list(rt: &Runtime, out: &mut Output) -> Result {
    let all = rt.service.harness_list().await?;
    let rows = all
        .iter()
        .map(|s| {
            let version = s
                .install
                .as_ref()
                .and_then(|i| i.version.clone())
                .unwrap_or_else(|| out.none());
            let applied = match (&s.applied, &s.applied_provider) {
                (true, Some(p)) => p.to_string(),
                (true, None) => out.boolean(true),
                (false, _) => out.none(),
            };
            vec![
                s.harness.as_str().to_owned(),
                s.harness.display_name().to_owned(),
                out.boolean(s.installed),
                version,
                applied,
                if s.drift > 0 {
                    s.drift.to_string()
                } else {
                    String::new()
                },
            ]
        })
        .collect::<Vec<_>>();
    out.table(
        &[
            "cli.harness.col.id",
            "cli.harness.col.name",
            "cli.harness.col.installed",
            "cli.harness.col.version",
            "cli.harness.col.applied",
            "cli.harness.col.drift",
        ],
        rows,
    )?;
    out.value(&all)?;
    Ok(exit::OK)
}

fn print_status(view: &HarnessView, out: &mut Output) -> superba_core::Result<()> {
    out.field("cli.harness.field.harness", view.harness.display_name())?;
    match &view.install {
        Some(i) => {
            let path = i
                .binary
                .as_ref()
                .map(|b| superba_core::paths::display_path(b))
                .unwrap_or_default();
            let major = i
                .opencode_major
                .map(|m| format!(" · OpenCode {}", m.as_str()))
                .unwrap_or_default();
            out.field(
                "cli.harness.field.program",
                format!("{}{major} {path}", i.version.as_deref().unwrap_or("?")),
            )?
        }
        None => out.field(
            "cli.harness.field.program",
            out.label("cli.harness.not_found"),
        )?,
    }
    let s = &view.settings;
    out.field(
        "cli.harness.field.provider",
        s.provider
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| out.label("cli.harness.follows_codex")),
    )?;
    out.field(
        "cli.harness.field.protocol",
        s.protocol
            .map(|p| p.label().to_owned())
            .unwrap_or_else(|| out.label("cli.harness.auto")),
    )?;
    out.field("cli.harness.field.route", s.route.as_str())?;
    if let Some(e) = &view.endpoint {
        out.field(
            "cli.harness.field.endpoint",
            format!("{} ({})", e.base_text(), e.protocol.label()),
        )?;
    }
    if !view.protocols.is_empty() {
        out.field(
            "cli.harness.field.offered",
            view.protocols
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )?;
    }
    out.field(
        "cli.harness.field.models",
        if s.models.is_empty() {
            out.label("cli.harness.all_models")
        } else {
            s.models.join(", ")
        },
    )?;
    if let Some(m) = &s.default_model {
        out.field("cli.harness.field.default_model", m)?;
    }
    for f in &view.files {
        let state = match &f.status {
            superba_core::harness::HarnessFileStatus::Absent => {
                out.label("cli.harness.file.absent")
            }
            superba_core::harness::HarnessFileStatus::Valid => out.label("cli.harness.file.valid"),
            _ => out.label("cli.harness.file.invalid"),
        };
        out.data_line(&format!(
            "  {} ({state})",
            superba_core::paths::display_path(&f.path)
        ))?;
    }
    match &view.applied {
        Some(a) => {
            out.line(
                text("cli.harness.applied")
                    .arg("generation", a.generation)
                    .arg("at", out.time(a.at))
                    .arg("n", a.owned.len()),
            )?;
            if !a.drift.is_empty() {
                out.line(text("cli.harness.drift").arg("keys", a.drift.join(", ")))?;
            }
        }
        None => out.line(text("cli.harness.not_applied"))?,
    }
    if !view.pending.is_empty() {
        out.line(text("cli.harness.pending").arg("n", view.pending.len()))?;
    }
    for n in &view.notes {
        out.notice(n)?;
    }
    Ok(())
}

/// Save the settings the flags give, then plan, confirm and apply.
async fn apply(args: &HarnessApplyArgs, rt: &Runtime, out: &mut Output) -> Result {
    let h = harness(out, &args.harness)?;
    let view = rt.service.harness_view(h).await?;
    let mut s = view.settings.clone();
    let mut changed = false;
    if let Some(p) = &args.provider {
        s.provider = Some(ProfileId::new(p));
        changed = true;
    }
    if args.follow_codex {
        s.provider = None;
        changed = true;
    }
    if let Some(p) = &args.protocol {
        s.protocol =
            match p.as_str() {
                "auto" => None,
                other => Some(HarnessProtocol::parse(other).ok_or_else(|| {
                    usage(out, text("cli.harness.bad_protocol").arg("value", other))
                })?),
            };
        changed = true;
    }
    if !args.model.is_empty() {
        s.models = args
            .model
            .iter()
            .flat_map(|m| m.split(','))
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .collect();
        changed = true;
    }
    if args.all_models {
        s.models.clear();
        changed = true;
    }
    if let Some(m) = &args.default_model {
        s.default_model = (!m.trim().is_empty()).then(|| m.trim().to_owned());
        changed = true;
    }
    if args.routed {
        s.route = HarnessRoute::Routed;
        changed = true;
    }
    if args.direct {
        s.route = HarnessRoute::Direct;
        changed = true;
    }
    if let Some(u) = &args.router_url {
        s.router_url = Some(url(out, u)?);
        changed = true;
    }
    if let Some(t) = &args.telemetry {
        s.options.dsh_telemetry = match t.as_str() {
            "off" => DshTelemetry::Off,
            _ => DshTelemetry::Unmanaged,
        };
        changed = true;
    }
    if let Some(m) = &args.opencode_major {
        s.options.opencode_major = match m.as_str() {
            "auto" => None,
            other => Some(
                OpencodeMajor::parse(other)
                    .ok_or_else(|| usage(out, text("cli.harness.bad_major").arg("value", other)))?,
            ),
        };
        changed = true;
    }
    if !args.edition.is_empty() {
        let mut e = Vec::new();
        for x in &args.edition {
            e.push(
                WorkBuddyEdition::parse(x)
                    .ok_or_else(|| usage(out, text("cli.harness.bad_edition").arg("value", x)))?,
            );
        }
        s.options.workbuddy_editions = e;
        changed = true;
    }
    if changed {
        rt.service.save_harness_settings(h, s).await?;
    }
    execute(h, HarnessPlanKind::Apply, args.yes, args.take_over, rt, out).await
}

fn print_plan(plan: &HarnessPlan, out: &mut Output) -> superba_core::Result<()> {
    if plan.changes.is_empty() {
        out.line(text("cli.harness.plan_empty"))?;
    }
    let none = out.none();
    let short = |s: &Option<String>| {
        let t = s.clone().unwrap_or_else(|| none.clone());
        if t.chars().count() > 72 {
            format!("{}…", t.chars().take(71).collect::<String>())
        } else {
            t
        }
    };
    let rows = plan
        .changes
        .iter()
        .map(|c| {
            vec![
                c.file.clone(),
                c.slot.clone(),
                short(&c.before),
                short(&c.after),
                if c.drift {
                    out.label("cli.harness.drift_mark")
                } else {
                    String::new()
                },
            ]
        })
        .collect::<Vec<_>>();
    if !rows.is_empty() {
        out.table(
            &[
                "cli.harness.col.file",
                "cli.harness.col.entry",
                "cli.harness.col.before",
                "cli.harness.col.after",
                "cli.harness.col.note",
            ],
            rows,
        )?;
    }
    for n in &plan.notes {
        out.notice(n)?;
    }
    Ok(())
}

fn confirm(
    yes: bool,
    rt: &Runtime,
    out: &mut Output,
    value: &impl serde::Serialize,
) -> Result<Option<u8>> {
    if yes {
        return Ok(None);
    }
    let interactive = rt.interactive && !out.json;
    if !interactive {
        out.line(text("cli.harness.confirm_needed"))?;
        out.value(value)?;
        return Ok(Some(exit::NEEDS_CONFIRMATION));
    }
    out.line(text("cli.harness.confirm"))?;
    let answer = (rt.read_stdin_line)()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !matches!(answer.as_str(), "y" | "yes" | "是") {
        out.line(text("cli.harness.cancelled"))?;
        return Ok(Some(exit::CANCELLED));
    }
    Ok(None)
}

fn print_report(report: &HarnessApplyReport, out: &mut Output) -> superba_core::Result<()> {
    out.line(
        text(match report.kind {
            HarnessPlanKind::Restore => "cli.harness.restored",
            HarnessPlanKind::Apply => "cli.harness.applied_now",
        })
        .arg("n", report.changed.len())
        .arg("files", report.files.len()),
    )?;
    for f in &report.files {
        let what = if f.sha256.is_none() {
            out.label("cli.harness.removed")
        } else {
            out.label("cli.harness.written")
        };
        let backup = f
            .backup
            .as_ref()
            .map(|b| out.render(&text("cli.harness.backup").arg("id", &b.id)))
            .unwrap_or_default();
        out.data_line(&format!(
            "  {what} {} {backup}",
            superba_core::paths::display_path(&f.path)
        ))?;
    }
    if !report.kept.is_empty() {
        out.line(text("cli.harness.kept").arg("keys", report.kept.join(", ")))?;
    }
    for n in &report.notes {
        out.notice(n)?;
    }
    Ok(())
}

async fn execute(
    h: HarnessId,
    kind: HarnessPlanKind,
    yes: bool,
    take_over: bool,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let plan = rt.service.harness_plan(h, kind).await?;
    print_plan(&plan, out)?;
    if plan.is_empty() {
        out.value(&plan)?;
        return Ok(exit::OK);
    }
    let drift = plan.drift().count();
    if drift > 0 && !take_over {
        out.line(text("cli.harness.drift_kept").arg("n", drift))?;
    }
    if let Some(code) = confirm(yes, rt, out, &plan)? {
        return Ok(code);
    }
    let report = rt.service.harness_apply(plan, take_over).await?;
    print_report(&report, out)?;
    out.value(&report)?;
    Ok(exit::OK)
}

async fn restore_backup(
    h: HarnessId,
    id: &str,
    yes: bool,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let backups = rt.service.harness_backups(h).await?;
    let Some(b) = backups.iter().find(|b| b.id == id) else {
        return Err(usage(out, text("cli.harness.backup_missing").arg("id", id)));
    };
    out.line(
        text("cli.harness.restore_backup")
            .arg("path", superba_core::paths::display_path(&b.path))
            .arg("at", out.time(b.at)),
    )?;
    if let Some(code) = confirm(yes, rt, out, b)? {
        return Ok(code);
    }
    let report = rt.service.harness_restore_backup(h, id.to_owned()).await?;
    print_report(&report, out)?;
    out.value(&report)?;
    Ok(exit::OK)
}
