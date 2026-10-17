//! `spb usage [log|stats]` and `spb router stats-line|record|clear`: the request
//! log of this computer and its statistics, the reply stats line and the
//! router's recording settings. The log merges the local router's database
//! (readable while the router is stopped; numbers only, never headers or
//! bodies) with the model requests
//! in Codex's session files, so it works without the router; rows from the
//! session files have no first token, TPS or line. Remote hosts are not in it.

use std::time::Duration;

use superba_core::{
    ClearRequest, ProfileId, RequestClient, RequestPage, RequestQuery, RequestRow,
    RequestStatusFilter, StatsBucket, StatsGroupBy, StatsLineClient, StatsLineSettings,
    StatsLineStyle, StatsQuery,
};

use super::Result;
use crate::{
    LogClientArg, OnOff, Output, RouterLogArgs, Runtime, StatsByArg, StatsClientArg, exit,
    output::grouped, text::text,
};

fn since(rt: &Runtime, secs: u64) -> superba_core::Timestamp {
    rt.clock.now().saturating_sub(Duration::from_secs(secs))
}

fn ms(v: Option<u64>) -> String {
    v.map(|m| format!("{}.{:02}s", m / 1000, (m % 1000) / 10))
        .unwrap_or_else(|| "-".into())
}

fn tps(v: Option<u64>) -> String {
    v.map(|t| format!("{}.{}", t / 1000, (t % 1000) / 100))
        .unwrap_or_else(|| "-".into())
}

fn tokens(v: Option<u64>) -> String {
    v.map(grouped).unwrap_or_else(|| "-".into())
}

fn usd(v: Option<superba_core::Usd>) -> String {
    v.map(|u| u.to_string()).unwrap_or_else(|| "-".into())
}

/// A summed cost estimate (OpenAI list prices x the provider's ratio, the same
/// numbers the GUI shows): `-` when nothing in it had a price, never `$0`.
fn total_cost(t: &superba_core::RequestTotals) -> String {
    if t.cost.is_zero() && t.unpriced > 0 {
        "-".into()
    } else {
        t.cost.to_string()
    }
}

pub(super) async fn log(args: &RouterLogArgs, rt: &Runtime, out: &mut Output) -> Result {
    let query = RequestQuery {
        from: Some(since(rt, args.since)),
        member: args.provider.as_deref().map(ProfileId::new),
        model: args.model.clone(),
        status: if args.errors {
            RequestStatusFilter::Errors
        } else if args.failover {
            RequestStatusFilter::Failover
        } else {
            RequestStatusFilter::All
        },
        client: args.client.map(|c| match c {
            LogClientArg::App => RequestClient::Desktop,
            LogClientArg::Cli => RequestClient::Tui,
            LogClientArg::Exec => RequestClient::Exec,
        }),
        limit: args.limit,
        ..RequestQuery::default()
    };
    let page = rt.service.router_requests(query).await?;
    render_log(&page, rt, out).await?;
    out.value(&page)?;
    Ok(exit::OK)
}

async fn render_log(
    page: &RequestPage,
    rt: &Runtime,
    out: &mut Output,
) -> superba_core::Result<()> {
    if out.json {
        return Ok(());
    }
    // Session names are looked up locally at display time (never stored).
    let keys: Vec<String> = page
        .rows
        .iter()
        .filter_map(|r| r.prompt_cache_key.clone().or_else(|| r.thread_id.clone()))
        .collect();
    let names = if keys.is_empty() {
        Default::default()
    } else {
        rt.service.session_titles(keys).await.unwrap_or_default()
    };
    let session = |r: &RequestRow| {
        r.prompt_cache_key
            .as_ref()
            .or(r.thread_id.as_ref())
            .and_then(|k| names.get(k))
            .map(|t| t.title.clone())
            .or_else(|| r.short_key())
            .unwrap_or_else(|| "-".into())
    };
    let rows = page
        .rows
        .iter()
        .map(|r| {
            vec![
                r.started_at.to_rfc3339(),
                session(r),
                out.render(&r.status.text()),
                match r.line_label() {
                    Some(line) => format!("{} · {line}", r.member_name),
                    None => r.member_name.clone(),
                },
                r.model.clone().unwrap_or_else(|| "-".into()),
                tokens(r.input_tokens),
                tokens(r.output_tokens),
                tokens(r.cached_tokens),
                tokens(r.cache_write_tokens),
                ms(r.first_token_ms),
                ms(r.duration_ms),
                tps(r.tps_milli),
                usd(r.cost),
                out.render(&text(match r.source {
                    superba_core::RequestSource::Rollout => "cli.requests.source.rollout",
                    _ => "cli.requests.source.router",
                })),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.requests.title",
        &[
            "cli.requests.col.date",
            "cli.requests.col.session",
            "cli.requests.col.status",
            "cli.requests.col.provider",
            "cli.requests.col.model",
            "cli.requests.col.input",
            "cli.requests.col.output",
            "cli.requests.col.cache_read",
            "cli.requests.col.cache_write",
            "cli.requests.col.first_token",
            "cli.requests.col.duration",
            "cli.requests.col.tps",
            "cli.requests.col.cost",
            "cli.requests.col.source",
        ],
        rows,
    )?;
    let t = &page.totals;
    out.line(
        text("cli.requests.totals")
            .arg("requests", grouped(t.requests))
            .arg("errors", grouped(t.errors))
            .arg("input", grouped(t.input_tokens))
            .arg("output", grouped(t.output_tokens))
            .arg("cached", grouped(t.cached_tokens))
            .arg("cost", total_cost(t)),
    )?;
    out.line(text("cli.requests.cost_note"))?;
    if page.truncated {
        out.line(text("cli.requests.truncated"))?;
    }
    if !page.recording {
        let configured = rt
            .service
            .router_status()
            .await
            .map(|v| v.configured)
            .unwrap_or(true);
        out.line(text(if configured {
            "cli.requests.not_recording"
        } else {
            "cli.requests.no_router"
        }))?;
    }
    out.line(text("cli.requests.remote_note"))
}

pub(super) async fn stats(by: StatsByArg, secs: u64, rt: &Runtime, out: &mut Output) -> Result {
    let offset = local_offset_minutes();
    let stats = rt
        .service
        .router_stats(StatsQuery {
            from: Some(since(rt, secs)),
            by: match by {
                StatsByArg::Model => StatsGroupBy::Model,
                StatsByArg::Provider => StatsGroupBy::Provider,
                StatsByArg::Day => StatsGroupBy::Day,
                StatsByArg::Session => StatsGroupBy::Session,
            },
            utc_offset_minutes: offset,
            ..StatsQuery::default()
        })
        .await?;
    let cells = |label: String, b: &StatsBucket| {
        vec![
            label,
            grouped(b.totals.requests),
            grouped(b.totals.errors),
            grouped(b.totals.input_tokens),
            grouped(b.totals.output_tokens),
            grouped(b.totals.cached_tokens),
            ms(b.totals.avg_first_token_ms()),
            tps(b.totals.tps_milli()),
            total_cost(&b.totals),
        ]
    };
    let mut rows = Vec::new();
    for b in &stats.buckets {
        let label = if b.label.is_empty() {
            "-".to_owned()
        } else {
            b.label.clone()
        };
        rows.push(cells(label, b));
        // A provider's lines, indented under it, when it used more than one.
        if b.lines.len() > 1 {
            rows.extend(b.lines.iter().map(|l| cells(format!("  {}", l.label), l)));
        }
    }
    out.section_table(
        "cli.requests.stats_title",
        &[
            match by {
                StatsByArg::Model => "cli.requests.col.model",
                StatsByArg::Provider => "cli.requests.col.provider",
                StatsByArg::Day => "cli.requests.col.date",
                StatsByArg::Session => "cli.requests.col.session",
            },
            "cli.requests.col.requests",
            "cli.requests.col.errors",
            "cli.requests.col.input",
            "cli.requests.col.output",
            "cli.requests.col.cache_read",
            "cli.requests.col.first_token",
            "cli.requests.col.tps",
            "cli.requests.col.cost",
        ],
        rows,
    )?;
    let t = &stats.totals;
    out.line(
        text("cli.requests.totals")
            .arg("requests", grouped(t.requests))
            .arg("errors", grouped(t.errors))
            .arg("input", grouped(t.input_tokens))
            .arg("output", grouped(t.output_tokens))
            .arg("cached", grouped(t.cached_tokens))
            .arg("cost", total_cost(t)),
    )?;
    out.line(text("cli.requests.cost_note"))?;
    out.value(&stats)?;
    Ok(exit::OK)
}

/// UTC offset of this machine's local time in minutes (from `TZ`-aware libc
/// where available; UTC otherwise).
fn local_offset_minutes() -> i32 {
    0
}

/// `spb router stats-line [on|off] [--clients ...] [--template T] [--italic]`.
/// Without arguments it shows the line's settings. `--template default` goes
/// back to the default template: stored as a marker, resolved in the UI
/// language whenever the settings reach the router (so it follows a later
/// language change); any other template is kept verbatim.
pub(super) async fn stats_line(
    state: Option<OnOff>,
    clients: &[StatsClientArg],
    template: Option<&str>,
    italic: bool,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let mut settings = rt.service.router_status().await?.settings.stats;
    // The language the service resolves the default template in.
    let ui = rt.service.profile_set().await.map(|p| p.settings);
    let locale = superba_switch::router::ui_locale(&ui.unwrap_or_default(), rt.env.as_ref());
    if state.is_none() && clients.is_empty() && template.is_none() && !italic {
        show_stats_line(&settings.line, locale, out)?;
        out.value(&StatsLineView::new(&settings.line, locale))?;
        return Ok(exit::OK);
    }
    if let Some(state) = state {
        settings.line.enabled = state.is_on();
    }
    if !clients.is_empty() {
        settings.line.clients = clients
            .iter()
            .map(|c| match c {
                StatsClientArg::App => StatsLineClient::Desktop,
                StatsClientArg::Tui => StatsLineClient::Tui,
            })
            .collect();
    }
    if let Some(t) = template {
        settings.line.set_template(t);
    }
    if italic {
        settings.line.style = StatsLineStyle::QuoteItalic;
    }
    let view = rt.service.router_set_stats(settings.clone()).await?;
    out.line(text(if settings.line.enabled {
        "cli.requests.line_on"
    } else {
        "cli.requests.line_off"
    }))?;
    if template.is_some() {
        template_fields(&settings.line, locale, out)?;
    }
    out.line(text("cli.requests.reload_note"))?;
    out.value(&view)?;
    Ok(exit::OK)
}

/// The stats line settings as `spb router stats-line --json` shows them.
#[derive(serde::Serialize)]
struct StatsLineView<'a> {
    enabled: bool,
    clients: &'a [StatsLineClient],
    style: StatsLineStyle,
    /// `default` while the default template is in use, else the custom one.
    template: &'a str,
    /// The text the router gets: the custom template, else the default in
    /// `locale`.
    resolved_template: String,
    /// The UI language the default is resolved in.
    locale: superba_core::Locale,
}

impl<'a> StatsLineView<'a> {
    fn new(line: &'a StatsLineSettings, locale: superba_core::Locale) -> Self {
        Self {
            enabled: line.enabled,
            clients: &line.clients,
            style: line.style,
            template: line
                .custom_template()
                .unwrap_or(StatsLineSettings::DEFAULT_TEMPLATE_KEYWORD),
            resolved_template: line.resolved_template(locale),
            locale,
        }
    }
}

fn show_stats_line(
    line: &StatsLineSettings,
    locale: superba_core::Locale,
    out: &mut Output,
) -> superba_core::Result<()> {
    out.line(text(if line.enabled {
        "cli.requests.line_on"
    } else {
        "cli.requests.line_off"
    }))?;
    let clients: Vec<String> = line
        .clients
        .iter()
        .map(|c| {
            out.render(
                &match c {
                    StatsLineClient::Tui => RequestClient::Tui,
                    _ => RequestClient::Desktop,
                }
                .text(),
            )
        })
        .collect();
    out.field("cli.requests.line_clients", clients.join(", "))?;
    out.field(
        "cli.requests.line_style",
        out.label(match line.style {
            StatsLineStyle::QuoteItalic => "cli.requests.line_style_italic",
            _ => "cli.requests.line_style_quote",
        }),
    )?;
    template_fields(line, locale, out)
}

/// `Template: default (follows the UI language: 简体中文)` or `custom`, then
/// the text the router gets.
fn template_fields(
    line: &StatsLineSettings,
    locale: superba_core::Locale,
    out: &mut Output,
) -> superba_core::Result<()> {
    let kind = if line.custom_template().is_some() {
        text("cli.requests.line_template_custom")
    } else {
        text("cli.requests.line_template_default").arg(
            "language",
            match locale {
                superba_core::Locale::ZhCn => "简体中文",
                _ => "English",
            },
        )
    };
    out.field("cli.requests.line_template", out.render(&kind))?;
    out.data_line(&format!("  {}", line.resolved_template(locale)))
}

pub(super) async fn record(state: OnOff, rt: &Runtime, out: &mut Output) -> Result {
    let mut settings = rt.service.router_status().await?.settings.stats;
    settings.record = state.is_on();
    let view = rt.service.router_set_stats(settings.clone()).await?;
    out.line(text(if settings.record {
        "cli.requests.record_on"
    } else {
        "cli.requests.record_off"
    }))?;
    out.value(&view)?;
    Ok(exit::OK)
}

pub(super) async fn clear(older_than: Option<u64>, rt: &Runtime, out: &mut Output) -> Result {
    let report = rt
        .service
        .router_clear(ClearRequest {
            before: older_than.map(|s| since(rt, s)),
        })
        .await?;
    out.line(text("cli.requests.cleared").arg("rows", grouped(report.rows)))?;
    out.value(&report)?;
    Ok(exit::OK)
}
