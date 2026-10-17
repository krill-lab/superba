//! `spb site`: list sites; add, edit, remove and probe user-defined
//! OpenAI-compatible sites.

use superba_core::{
    KeyInput, LineId, ProfileId, Secret, Site, SiteId, SiteKind, SiteProbeReport, SiteProbeRequest,
};

use super::{Result, missing, stdin_secret, usage};
use crate::{
    Output, Runtime, SiteCommand, SiteEditArgs, SiteProbeArgs, exit,
    text::{name_id, text},
};

pub(super) async fn run(command: &SiteCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        SiteCommand::List => {
            let sites = rt.service.sites().await?;
            let rows = sites.iter().map(|site| row(site, out)).collect::<Vec<_>>();
            out.table(
                &[
                    "cli.label.id",
                    "cli.label.name",
                    "cli.label.kind",
                    "cli.label.lines",
                    "cli.label.model_map",
                ],
                rows,
            )?;
            out.value(&sites)?;
            Ok(exit::OK)
        }
        SiteCommand::Add {
            id,
            name,
            base_url,
            map,
        } => {
            let url = parse_url(base_url, out)?;
            let mut site = superba_switch::new_compatible_site(
                SiteId::new(id),
                name.as_deref().unwrap_or(id),
                &url,
            )?;
            site.model_map.extend(map.iter().cloned());
            let saved = rt.service.save_site(site).await?;
            out.line(text("cli.site.added").arg("id", &saved.id))?;
            render_map(&saved, out)?;
            out.line(text("cli.site.hint").arg("id", &saved.id))?;
            out.value(&saved)?;
            Ok(exit::OK)
        }
        SiteCommand::Edit(args) => edit(args, rt, out).await,
        SiteCommand::Remove { id } => {
            rt.service.delete_site(&SiteId::new(id)).await?;
            out.line(text("cli.site.removed").arg("id", id))?;
            out.value(&serde_json::json!({ "removed": id }))?;
            Ok(exit::OK)
        }
        SiteCommand::Probe(args) => probe(args, rt, out).await,
    }
}

fn parse_url(value: &str, out: &Output) -> Result<url::Url> {
    url::Url::parse(value.trim()).map_err(|_| usage(out, text("cli.error.base_url")))
}

async fn edit(args: &SiteEditArgs, rt: &Runtime, out: &mut Output) -> Result {
    if args.name.is_none()
        && args.base_url.is_none()
        && args.map.is_empty()
        && args.unmap.is_empty()
        && !args.clear_map
    {
        return Err(usage(out, text("cli.error.site_edit_nothing")));
    }
    let id = SiteId::new(&args.id);
    let current = super::site(rt, out, &args.id).await?;
    if current.kind != SiteKind::Compatible || superba_net_builtin(&current) {
        return Err(usage(
            out,
            text("cli.error.site_not_custom").arg("id", &args.id),
        ));
    }
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| out.render(&current.name));
    let mut site = match &args.base_url {
        Some(base_url) => {
            let url = parse_url(base_url, out)?;
            let mut site = superba_switch::new_compatible_site(id.clone(), &name, &url)?;
            site.model_map = current.model_map.clone();
            site.homepage = current.homepage.clone();
            site
        }
        None => {
            let mut site = current.clone();
            if args.name.is_some() {
                site.name = superba_core::LocalizedText::new("", name.trim());
            }
            site
        }
    };
    if args.clear_map {
        site.model_map.clear();
    }
    for codex in &args.unmap {
        site.model_map.remove(codex);
    }
    site.model_map.extend(args.map.iter().cloned());
    let saved = rt.service.save_site(site).await?;
    out.line(text("cli.site.updated").arg("id", &saved.id))?;
    render_map(&saved, out)?;
    out.line(text("cli.site.applies_next"))?;
    out.value(&saved)?;
    Ok(exit::OK)
}

/// Built-in sites are never compatible today; kept explicit so a future
/// built-in compatible site stays read-only here.
fn superba_net_builtin(site: &Site) -> bool {
    matches!(
        site.id.as_str(),
        superba_core::catalog::KRILL_SITE_ID | superba_core::catalog::OFFICIAL_SITE_ID
    )
}

fn render_map(site: &Site, out: &mut Output) -> Result<()> {
    if site.model_map.is_empty() {
        return Ok(());
    }
    out.field("cli.label.model_map", map_text(site, out))?;
    Ok(())
}

fn map_text(site: &Site, out: &Output) -> String {
    if site.model_map.is_empty() {
        return out.none();
    }
    site.model_map
        .iter()
        .map(|(codex, mapped)| {
            out.render(
                &text("cli.site.map_entry")
                    .arg("codex", codex)
                    .arg("site", mapped),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

async fn probe(args: &SiteProbeArgs, rt: &Runtime, out: &mut Output) -> Result {
    let site = super::site(rt, out, &args.id).await?;
    let key = if let Some(var) = &args.key_env {
        let value = rt
            .env
            .var_non_empty(var)
            .ok_or_else(|| usage(out, text("cli.error.key_env").arg("var", var)))?;
        Some(KeyInput::Secret(Secret::new(value)))
    } else if args.stdin {
        Some(KeyInput::Secret(stdin_secret(rt, out)?))
    } else if let Some(id) = &args.profile {
        let set = rt.service.profile_set().await?;
        if set.profile(&ProfileId::new(id)).is_none() {
            return Err(missing(out, text("cli.error.profile_missing").arg("id", id)).into());
        }
        Some(KeyInput::Profile(ProfileId::new(id)))
    } else {
        None
    };
    let has_key = key.is_some();
    let report = rt
        .service
        .probe_site(SiteProbeRequest {
            site_id: site.id.clone(),
            line_id: args.line.as_deref().map(LineId::new),
            key,
            model: args.model.clone(),
            responses: args.responses,
            site: None,
        })
        .await?;
    render_probe(&site, &report, has_key, out)?;
    out.value(&report)?;
    Ok(if probe_ok(&report, args.responses) {
        exit::OK
    } else {
        exit::ERROR
    })
}

/// Everything that was checked worked.
fn probe_ok(report: &SiteProbeReport, responses: bool) -> bool {
    report.caps.reachable
        && report.key_check.as_ref().is_none_or(|c| c.access.is_ok())
        && (!responses || report.responses.as_ref().is_some_and(|r| r.ok))
}

fn render_probe(
    site: &Site,
    report: &SiteProbeReport,
    has_key: bool,
    out: &mut Output,
) -> Result<()> {
    let line = site
        .line(&report.line_id)
        .map(|l| out.render(&name_id(l.name.clone(), &l.id)))
        .unwrap_or_else(|| report.line_id.to_string());
    out.field("cli.label.line", line)?;
    let caps = &report.caps;
    out.field(
        "cli.label.detail",
        out.label(if caps.reachable {
            "cli.site.probe_reachable"
        } else {
            "cli.site.probe_unreachable"
        }),
    )?;
    out.field("cli.label.latency", out.optional(caps.latency_ms))?;
    if let Some(detail) = &caps.detail {
        out.line(detail.clone())?;
    }
    if !has_key {
        out.line(text("cli.site.probe_no_key"))?;
        return Ok(());
    }
    if let Some(check) = &report.key_check {
        out.field("cli.label.access", out.render(&check.access.text()))?;
    }
    match (&report.models, &report.models_error) {
        (Some(models), _) => out.field(
            "cli.label.models",
            out.render(&text("cli.site.probe_models").arg("count", models.len())),
        )?,
        (None, Some(e)) => out.field("cli.label.models", out.render(&e.to_text()))?,
        (None, None) => {}
    }
    if !report.mapped.is_empty() {
        out.field("cli.label.model_map", map_text(site, out))?;
        for m in report.mapped.iter().filter(|m| m.available == Some(false)) {
            out.notice(&superba_core::Notice::warning(
                text("cli.site.probe_map_missing")
                    .arg("site", &m.site)
                    .arg("codex", &m.codex),
            ))?;
        }
    }
    match (&report.responses, &report.responses_error, &report.model) {
        (Some(r), _, Some(model)) if r.ok => out.line(
            text("cli.site.probe_responses_ok")
                .arg("model", model)
                .arg("ms", out.optional(r.latency_ms)),
        )?,
        (Some(r), _, Some(model)) => out.line(
            text("cli.site.probe_responses_failed")
                .arg("model", model)
                .arg(
                    "detail",
                    r.detail
                        .clone()
                        .or_else(|| r.status.map(|s| format!("HTTP {s}")))
                        .unwrap_or_else(|| out.none()),
                ),
        )?,
        (_, Some(e), _) => {
            out.line(text("cli.site.probe_responses_error").arg_text("error", e.to_text()))?
        }
        _ => {}
    }
    Ok(())
}

fn row(site: &Site, out: &Output) -> Vec<String> {
    let kind = match site.kind {
        SiteKind::Krill => "cli.site.kind.krill",
        SiteKind::Official => "cli.site.kind.official",
        _ => "cli.site.kind.compatible",
    };
    vec![
        site.id.to_string(),
        out.render(&site.name),
        out.label(kind),
        if site.lines.is_empty() {
            out.none()
        } else {
            site.lines
                .iter()
                .map(|l| format!("{} ({})", l.id, l.origin))
                .collect::<Vec<_>>()
                .join(", ")
        },
        map_text(site, out),
    ]
}
