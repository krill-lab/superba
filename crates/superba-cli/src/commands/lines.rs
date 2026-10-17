use super::{Result, site};
use crate::{Output, Runtime, exit, text::name_id};

pub(super) async fn run(site_id: &str, probe: bool, rt: &Runtime, out: &mut Output) -> Result {
    let site = site(rt, out, site_id).await?;
    if probe {
        let caps = rt.service.probe_lines(&site.id).await?;
        let rows = caps
            .iter()
            .map(|c| {
                vec![
                    site.line(&c.line_id)
                        .map(|l| out.render(&name_id(l.name.clone(), &l.id)))
                        .unwrap_or_else(|| c.line_id.to_string()),
                    out.optional(c.latency_ms),
                    out.boolean(c.pat_ok),
                    out.boolean(c.apikey_ok),
                    out.optional(c.codex_path.map(|p| p.suffix())),
                    out.boolean(c.blocked_by_edge),
                    c.detail
                        .as_ref()
                        .map(|d| out.render(d))
                        .unwrap_or_else(|| out.none()),
                ]
            })
            .collect::<Vec<_>>();
        out.table(
            &[
                "cli.label.line",
                "cli.label.latency",
                "cli.label.pat_ok",
                "cli.label.apikey_ok",
                "cli.label.codex_path",
                "cli.label.edge_blocked",
                "cli.label.detail",
            ],
            rows,
        )?;
        out.value(&caps)?;
    } else {
        let rows = site
            .lines
            .iter()
            .map(|line| {
                vec![
                    out.render(&name_id(line.name.clone(), &line.id)),
                    line.origin.as_str(),
                    if site
                        .default_line()
                        .is_some_and(|default| default.id == line.id)
                    {
                        out.label("cli.value.default")
                    } else {
                        out.none()
                    },
                ]
            })
            .collect::<Vec<_>>();
        out.table(
            &["cli.label.line", "cli.label.origin", "cli.label.default"],
            rows,
        )?;
        out.value(&site.lines)?;
    }
    Ok(exit::OK)
}
