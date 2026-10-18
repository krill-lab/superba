//! `spb account`: identity, balance and (with `--usage`) usage of a profile's key,
//! from the standard billing / organization usage endpoints. Read-only; every part
//! is optional and fails on its own.

use std::time::Duration;

use superba_core::{
    AccountReport, AccountRequest, AccountSummary, Error, LineId, ProfileId, UsageReport, Usd,
};

use super::Result;
use crate::{
    AccountArgs, Output, Runtime, exit,
    text::{line_label, name_id, text},
};

pub(super) async fn run(args: &AccountArgs, rt: &Runtime, out: &mut Output) -> Result {
    let request = AccountRequest {
        profile_id: args.profile.as_deref().map(ProfileId::new),
        line_id: args.line.as_deref().map(LineId::new),
        usage: args.usage_query(),
    };
    let asked_usage = request.usage.is_some();
    let report = rt.service.account(request).await?;
    let failed = failed(report.summary_error.as_ref())
        || (asked_usage && failed(report.usage_error.as_ref()));
    if out.json {
        out.value(&report)?;
        return Ok(if failed { exit::ERROR } else { exit::OK });
    }
    let sites = rt.service.sites().await.unwrap_or_default();
    out.field(
        "cli.account.profile",
        out.render(&name_id(
            superba_core::LocalizedText::new("", &report.profile_name),
            &report.profile_id,
        )),
    )?;
    let site_name = sites
        .iter()
        .find(|s| s.id == report.site_id)
        .map(|s| out.render(&name_id(s.name.clone(), &s.id)))
        .unwrap_or_else(|| report.site_id.to_string());
    out.field("cli.label.site", site_name)?;
    out.field(
        "cli.label.line",
        out.render(&line_label(&sites, Some(&report.site_id), &report.line_id)),
    )?;
    summary(&report, out)?;
    if asked_usage {
        usage(&report, out)?;
    }
    Ok(if failed { exit::ERROR } else { exit::OK })
}

/// A part failed in a way worth a non-zero exit (a site without the endpoint is
/// not a failure).
fn failed(error: Option<&Error>) -> bool {
    error.is_some_and(|e| !matches!(e, Error::Unsupported { .. }))
}

fn summary(report: &AccountReport, out: &mut Output) -> Result<()> {
    let Some(s) = &report.summary else {
        match &report.summary_error {
            Some(e) if failed(Some(e)) => {
                out.line(text("cli.account.failed").arg_text("error", e.to_text()))?
            }
            _ => out.line(text("cli.account.none"))?,
        }
        return Ok(());
    };
    identity(s, out)?;
    let Some(b) = &s.billing else {
        out.line(text("cli.account.no_billing"))?;
        return Ok(());
    };
    if let Some(plan) = &b.plan_title {
        out.field("cli.account.plan", plan)?;
    }
    if b.unlimited {
        out.field("cli.account.limit", out.label("cli.account.unlimited"))?;
    } else if let Some(limit) = b.limit {
        out.field("cli.account.limit", limit)?;
    }
    if let Some(used) = b.used {
        let label = match (&b.period_start, &b.period_end) {
            (Some(start), Some(end)) => out.render(
                &text("cli.account.used")
                    .arg("start", start)
                    .arg("end", last_day(end)),
            ),
            _ => out.label("cli.account.used_plain"),
        };
        out.line(
            text("cli.format.field")
                .arg("label", label)
                .arg("value", used),
        )?;
    }
    if let Some(remaining) = b.remaining {
        out.field("cli.account.remaining", remaining)?;
    }
    if let Some(until) = b.access_until {
        out.field("cli.account.expires", until.utc_date_string())?;
    }
    Ok(())
}

fn identity(s: &AccountSummary, out: &mut Output) -> Result<()> {
    if let Some(name) = &s.display_name {
        out.field("cli.label.name", name)?;
    }
    if let Some(email) = &s.email {
        out.field("cli.label.email", email)?;
    }
    if let Some(last4) = &s.key_last4 {
        out.field("cli.label.last4", last4)?;
    }
    Ok(())
}

/// `YYYY-MM-DD` of the day before an exclusive end date (display is inclusive).
fn last_day(end_exclusive: &str) -> String {
    let mut parts = end_exclusive.splitn(3, '-');
    let parsed = (|| {
        let y = parts.next()?.parse::<i64>().ok()?;
        let m = parts.next()?.parse::<u32>().ok()?;
        let d = parts.next()?.parse::<u32>().ok()?;
        superba_core::Timestamp::from_utc_date(y, m, d)
    })();
    parsed
        .map(|t| {
            t.saturating_sub(Duration::from_secs(86_400))
                .utc_date_string()
        })
        .unwrap_or_else(|| end_exclusive.to_owned())
}

fn usage(report: &AccountReport, out: &mut Output) -> Result<()> {
    let Some(u) = &report.usage else {
        match &report.usage_error {
            Some(e) if failed(Some(e)) => {
                out.line(text("cli.account.usage_failed").arg_text("error", e.to_text()))?
            }
            _ => out.line(text("cli.account.usage_unsupported"))?,
        }
        return Ok(());
    };
    usage_report(u, out)
}

fn count(value: Option<u64>, out: &Output) -> String {
    out.optional(value.map(crate::output::grouped))
}

fn money(value: Option<Usd>, out: &Output) -> String {
    out.optional(value)
}

fn usage_report(u: &UsageReport, out: &mut Output) -> Result<()> {
    let last = u.end.saturating_sub(Duration::from_secs(86_400));
    out.line(
        text("cli.account.usage_title")
            .arg("start", u.start.utc_date_string())
            .arg("end", last.utc_date_string())
            .arg("days", u.days),
    )?;
    match u.cost_source {
        Some(source) => {
            out.field("cli.account.cost_source", out.render(&source.text()))?;
            out.field("cli.account.total", money(u.total_cost, out))?;
        }
        None => out.line(text("cli.account.no_cost"))?,
    }
    let rows = u
        .daily
        .iter()
        .map(|d| {
            vec![
                d.start.utc_date_string(),
                money(d.cost, out),
                count(d.requests, out),
                count(d.input_tokens, out),
                count(d.output_tokens, out),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.account.daily",
        &[
            "cli.account.day",
            "cli.account.cost",
            "cli.account.requests",
            "cli.account.input_tokens",
            "cli.account.output_tokens",
        ],
        rows,
    )?;
    if u.usage_available {
        let rows = u
            .models
            .iter()
            .map(|m| {
                vec![
                    m.model.clone(),
                    money(m.cost, out),
                    crate::output::grouped(m.requests),
                    crate::output::grouped(m.input_tokens),
                    crate::output::grouped(m.cached_input_tokens),
                    crate::output::grouped(m.output_tokens),
                ]
            })
            .collect::<Vec<_>>();
        out.section_table(
            "cli.account.models",
            &[
                "cli.account.model",
                "cli.account.cost",
                "cli.account.requests",
                "cli.account.input_tokens",
                "cli.account.cached_tokens",
                "cli.account.output_tokens",
            ],
            rows,
        )?;
    } else {
        out.line(text("cli.account.no_models"))?;
    }
    if let Some(notice) = &u.notice {
        out.notice(&superba_core::Notice::warning(notice.text()))?;
    }
    if u.from_cache || u.notice.is_some() {
        out.line(text("cli.account.saved").arg("time", out.time(u.fetched_at)))?;
    }
    Ok(())
}
