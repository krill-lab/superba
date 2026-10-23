//! The Overview's 今日 card (user request 27): today's estimated cost (else
//! tokens) as the big figure with its unit small, the change against yesterday
//! by the same time of day, the request count, today's tokens split into cached
//! input, uncached input and output, and a 7-day × 24-hour heat-strip of usage.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::Cx;
use super::cards::header;
use super::usage::{self, Delta, Heat, Measure, Snapshot, TodayView};
use crate::design::backdrop::Backdrop;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::text;
use crate::design::widgets::Kpi;
use crate::design::widgets::kpi::{self, KpiValue};

/// A value of the card's measure as a KPI figure.
pub fn figure(measure: Measure, value: i64, tokens_unit: &str) -> KpiValue {
    match measure {
        Measure::Cost => kpi::usd(value),
        Measure::Tokens => {
            let mut v = kpi::si(value.max(0) as u64);
            v.unit = format!("{} {tokens_unit}", v.unit).trim().to_owned();
            v
        }
    }
}

/// The size of the delta as text ("18%", "<1%"); the chip's arrow gives its
/// direction.
pub fn delta_text(delta: Delta) -> Option<String> {
    match delta {
        Delta::Percent(p) if p.abs() < 1.0 => Some("<1%".into()),
        Delta::Percent(p) if p.abs() >= 1000.0 => Some(format!("{:.0}x", 1.0 + p / 100.0)),
        Delta::Percent(p) => Some(format!("{:.0}%", p.abs())),
        _ => None,
    }
}

/// Draw the card.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, snap: Option<&Snapshot>, out: &mut Vec<super::Action>) {
    let tr = cx.tr;
    let view = snap.map(|s| TodayView::of(&s.today, &s.yesterday_same, &s.yesterday, &s.month));
    let tokens = tr.t("overview.unit.tokens");
    let month = view.as_ref().filter(|v| v.month > 0).map(|v| {
        tr.f(
            "overview.today.month",
            &[("value", &figure(v.measure, v.month, &tokens).text())],
        )
    });
    header(ui, cx, &tr.t("overview.card.today"), None, |ui| {
        if let Some(m) = &month {
            ui.label(
                egui::RichText::new(m)
                    .font(font(text::CAPTION, Weight::Regular))
                    .color(cx.pal.text_subtle),
            )
            .on_hover_text(tr.t("overview.today.month_tip"));
        }
    });
    ui.add_space(2.0);
    kpi_row(ui, cx, view.as_ref(), &tokens);
    // Every tool with use today (two or more): one chip each.
    if let (Some(s), Some(v)) = (snap, view.as_ref())
        && s.tools_today.len() > 1
    {
        super::tools::chips(ui, cx, &s.tools_today, v.measure, out);
    }
    ui.add_space(10.0);
    split(ui, cx, view.as_ref());
    ui.add_space(12.0);
    let full = ui.max_rect();
    let strip = Rect::from_min_max(pos2(full.min.x, ui.cursor().min.y), full.max);
    heat_strip(ui, cx, strip, snap);
}

fn kpi_row(ui: &mut Ui, cx: &Cx<'_>, view: Option<&TodayView>, tokens: &str) {
    let tr = cx.tr;
    let pal = cx.pal;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        match view {
            Some(v) if !v.is_empty() => {
                let r = ui.add(Kpi::new(figure(v.measure, v.value, tokens)));
                r.on_hover_text(match v.measure {
                    Measure::Cost => tr.t("overview.today.estimate_tip"),
                    Measure::Tokens => tr.t("overview.today.unpriced_tip"),
                });
                if let Some(d) = delta_text(v.delta) {
                    let up = matches!(v.delta, Delta::Percent(p) if p > 0.0);
                    let base = figure(v.measure, v.yesterday_same, tokens).text();
                    let full = figure(v.measure, v.yesterday, tokens).text();
                    delta_chip(ui, cx, &d, up).on_hover_text(tr.f(
                        "overview.today.delta_tip",
                        &[("same", &base), ("full", &full)],
                    ));
                } else if v.delta == Delta::New {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Max), |ui| {
                        ui.label(gray(cx, &tr.t("overview.today.delta_new")));
                    });
                }
            }
            _ => {
                ui.add(Kpi::new(KpiValue::none()).color(pal.text_disabled));
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Max), |ui| {
                    ui.label(gray(
                        cx,
                        &tr.t(if view.is_none() {
                            "overview.today.loading"
                        } else {
                            "overview.today.empty"
                        }),
                    ));
                });
            }
        }
        if let Some(v) = view.filter(|v| !v.is_empty()) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if v.errors > 0 {
                    ui.label(
                        egui::RichText::new(tr.f("overview.today.errors", &[("n", &v.errors)]))
                            .font(font(text::SMALL, Weight::Medium))
                            .color(pal.red),
                    );
                }
                ui.label(
                    egui::RichText::new(tr.f(
                        "overview.today.requests",
                        &[("n", &kpi::group(v.requests as i64))],
                    ))
                    .font(font(text::SMALL, Weight::Medium))
                    .color(pal.text_muted),
                );
            });
        }
    });
}

fn gray(cx: &Cx<'_>, s: &str) -> egui::RichText {
    egui::RichText::new(s)
        .font(font(text::SMALL, Weight::Regular))
        .color(cx.pal.text_subtle)
}

/// "↑ 18% vs. yesterday" as a soft pill with a drawn arrow.
fn delta_chip(ui: &mut Ui, cx: &Cx<'_>, value: &str, up: bool) -> egui::Response {
    let pal = cx.pal;
    let label = format!("{value} {}", cx.tr.t("overview.today.vs_yesterday"));
    let g = ui
        .painter()
        .layout_no_wrap(label, font(text::CAPTION, Weight::Medium), pal.text_muted);
    let size = vec2(g.size().x + 30.0, 22.0);
    // Sit on the KPI's baseline row.
    let (outer, resp) = ui.allocate_exact_size(vec2(size.x, 40.0), Sense::hover());
    let r = Rect::from_min_size(pos2(outer.min.x, outer.max.y - size.y - 4.0), size);
    let p = ui.painter();
    p.rect_filled(r, r.height() / 2.0, pal.hover);
    icons::paint(
        p,
        Rect::from_center_size(pos2(r.min.x + 13.0, r.center().y), vec2(12.0, 12.0)),
        if up { Icon::ArrowUp } else { Icon::ArrowDown },
        pal.text_muted,
    );
    p.galley(
        pos2(r.min.x + 22.0, r.center().y - g.size().y / 2.0),
        g,
        pal.text_muted,
    );
    resp
}

/// Colours of the three token parts: cached input (light), uncached input,
/// output.
pub fn split_colors(b: &Backdrop, dark: bool) -> [Color32; 3] {
    [
        Backdrop::area(b.chart[0], if dark { 110 } else { 95 }),
        b.chart[0],
        b.chart[1],
    ]
}

fn split(ui: &mut Ui, cx: &Cx<'_>, view: Option<&TodayView>) {
    let pal = cx.pal;
    let b = Backdrop::of(ui.ctx());
    let colors = split_colors(&b, pal.dark);
    let (bar, bar_resp) = ui.allocate_exact_size(vec2(ui.available_width(), 10.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(bar, 5.0, pal.active);
    let shares = view.map_or([0.0; 3], TodayView::split);
    let mut x = bar.min.x;
    let gap = 2.0;
    let parts: Vec<(usize, f32)> = shares
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, s)| *s > 0.0)
        .collect();
    for (k, (i, s)) in parts.iter().enumerate() {
        let w = bar.width() * s;
        let end = if k + 1 == parts.len() {
            bar.max.x
        } else {
            (x + w - gap).max(x + 2.0)
        };
        p.rect_filled(
            Rect::from_min_max(pos2(x, bar.min.y), pos2(end, bar.max.y)),
            5.0,
            colors[*i],
        );
        x += w;
    }
    let v = view.cloned();
    // Legend.
    let tr = cx.tr;
    let values = v
        .as_ref()
        .map_or([0, 0, 0], |v| [v.cached, v.uncached, v.output]);
    let names = [
        tr.t("overview.today.cached"),
        tr.t("overview.today.uncached"),
        tr.t("overview.today.output"),
    ];
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    let p = ui.painter();
    let mut x = row.min.x;
    for i in 0..3 {
        p.circle_filled(pos2(x + 4.0, row.center().y), 4.0, colors[i]);
        x += 13.0;
        let name = p.layout_no_wrap(
            names[i].clone(),
            font(text::CAPTION, Weight::Regular),
            pal.text_subtle,
        );
        let nw = name.size().x;
        p.galley(
            pos2(x, row.center().y - name.size().y / 2.0),
            name,
            pal.text_subtle,
        );
        x += nw + 5.0;
        let val = p.layout_no_wrap(
            kpi::si(values[i]).text(),
            font(text::CAPTION, Weight::Semibold),
            pal.text,
        );
        let vw = val.size().x;
        p.galley(pos2(x, row.center().y - val.size().y / 2.0), val, pal.text);
        x += vw + 18.0;
    }
    let mut tip = tr.t("overview.today.split_tip");
    if let Some(rate) = v.as_ref().and_then(TodayView::cache_rate) {
        let text = tr.f(
            "overview.today.cache_rate",
            &[("pct", &format!("{:.0}%", rate * 100.0))],
        );
        let galley = p.layout_no_wrap(
            text.clone(),
            font(text::CAPTION, Weight::Medium),
            pal.text_muted,
        );
        // Right-aligned when it clears the legend; otherwise only in the tip.
        if row.max.x - galley.size().x > x - 18.0 + 8.0 {
            p.galley(
                pos2(
                    row.max.x - galley.size().x,
                    row.center().y - galley.size().y / 2.0,
                ),
                galley,
                pal.text_muted,
            );
        } else {
            tip = format!("{tip}\n{text}");
        }
    }
    bar_resp.on_hover_text(tip);
}

/// Short label of day `i` of the strip: 今天, else the weekday.
fn day_label(cx: &Cx<'_>, heat: &Heat, i: usize, offset: i64) -> String {
    if i + 1 == usage::DAYS {
        cx.tr.t("overview.heat.today")
    } else {
        cx.tr
            .t(&format!("overview.weekday.{}", heat.weekday(i, offset)))
    }
}

/// The 7 × 24 heat-strip: one row per day (today at the bottom), one cell per
/// local hour, shaded by tokens; hours still ahead today stay empty, the
/// current hour is outlined.
fn heat_strip(ui: &mut Ui, cx: &Cx<'_>, rect: Rect, snap: Option<&Snapshot>) {
    let pal = cx.pal;
    let b = Backdrop::of(ui.ctx());
    let label_w = 40.0;
    let axis_h = 16.0;
    let grid = Rect::from_min_max(
        pos2(rect.min.x + label_w, rect.min.y),
        pos2(rect.max.x, rect.max.y - axis_h),
    );
    let gap = 3.0;
    let cw = (grid.width() + gap) / usage::HOURS as f32;
    let ch = ((grid.height() + gap) / usage::DAYS as f32)
        .min(cw)
        .min(20.0);
    let grid = Rect::from_min_size(grid.min, vec2(grid.width(), ch * usage::DAYS as f32 - gap));
    let empty = if pal.dark {
        Color32::from_white_alpha(10)
    } else {
        Color32::from_black_alpha(10)
    };
    let shades = [60u8, 115, 175, 255];
    let p = ui.painter();
    let fallback;
    let heat = match snap {
        Some(s) => &s.heat,
        None => {
            fallback = Heat::default();
            &fallback
        }
    };
    let offset = snap.map_or(0, |s| s.offset_secs);
    let cell_rect = |d: usize, h: usize| {
        Rect::from_min_size(
            pos2(grid.min.x + h as f32 * cw, grid.min.y + d as f32 * ch),
            vec2(cw - gap, ch - gap),
        )
    };
    // Rows too short for every label: today and every other day.
    let sparse = ch < 13.0;
    for d in 0..usage::DAYS {
        let labelled = !sparse || d == usage::DAYS - 1 || (usage::DAYS - 1 - d).is_multiple_of(2);
        if snap.is_some() && labelled {
            p.text(
                pos2(rect.min.x, grid.min.y + d as f32 * ch + (ch - gap) / 2.0),
                Align2::LEFT_CENTER,
                day_label(cx, heat, d, offset),
                font(text::CAPTION - 1.0, Weight::Regular),
                pal.text_subtle,
            );
        }
        for h in 0..usage::HOURS {
            let r = cell_rect(d, h);
            let future = d == usage::DAYS - 1 && h > heat.now_hour;
            let cell = heat.cells.get(d).map(|row| row[h]).unwrap_or_default();
            let level = heat.level(&cell);
            if future {
                p.rect_stroke(r, 3.0, Stroke::new(1.0, empty), StrokeKind::Inside);
            } else if level == 0 {
                p.rect_filled(r, 3.0, empty);
            } else {
                p.rect_filled(
                    r,
                    3.0,
                    Backdrop::area(b.chart[0], shades[level as usize - 1]),
                );
            }
            if d == usage::DAYS - 1 && h == heat.now_hour && snap.is_some() {
                p.rect_stroke(
                    r.expand(1.0),
                    4.0,
                    Stroke::new(1.5, pal.text_muted),
                    StrokeKind::Inside,
                );
            }
        }
    }
    for h in [0usize, 6, 12, 18] {
        let r = cell_rect(usage::DAYS - 1, h);
        p.text(
            pos2(r.min.x, grid.max.y + 4.0),
            Align2::LEFT_TOP,
            format!("{h:02}"),
            mono_font(text::CAPTION - 2.0, Weight::Regular),
            pal.text_subtle,
        );
    }
    // Legend: less -> more, right-aligned under the strip.
    let ly = grid.max.y + 4.0 + 6.0;
    let small = font(text::CAPTION - 2.0, Weight::Regular);
    let more = p.layout_no_wrap(
        cx.tr.t("overview.heat.more"),
        small.clone(),
        pal.text_subtle,
    );
    let less = p.layout_no_wrap(
        cx.tr.t("overview.heat.less"),
        small.clone(),
        pal.text_subtle,
    );
    // The words give way to the swatches when they would run into the hour
    // ticks above them.
    let ticks_end = cell_rect(usage::DAYS - 1, 18).min.x + 16.0;
    let full_left = grid.max.x - more.size().x - 4.0 - 12.0 * 4.0 - 1.0 - less.size().x;
    let words = full_left > ticks_end + 6.0;
    let mut x = grid.max.x;
    if words {
        x -= more.size().x;
        let mh = more.size().y;
        p.galley(pos2(x, ly - mh / 2.0), more, pal.text_subtle);
        x -= 4.0;
    }
    for a in shades.iter().rev() {
        let r = Rect::from_min_size(pos2(x - 9.0, ly - 4.5), vec2(9.0, 9.0));
        p.rect_filled(r, 2.0, Backdrop::area(b.chart[0], *a));
        x -= 12.0;
    }
    if words {
        let lh = less.size().y;
        p.galley(
            pos2(x - 1.0 - less.size().x, ly - lh / 2.0),
            less,
            pal.text_subtle,
        );
    }
    if snap.is_some() && heat.is_empty() {
        let msg_rect = grid;
        p.rect_filled(
            Rect::from_center_size(msg_rect.center(), vec2(260.0, 26.0)),
            13.0,
            b.card,
        );
        p.text(
            msg_rect.center(),
            Align2::CENTER_CENTER,
            cx.tr.t("overview.heat.empty"),
            font(text::SMALL, Weight::Regular),
            pal.text_subtle,
        );
    }
    // The hovered cell's numbers.
    let resp = ui.interact(grid, ui.scope_id().with("overview-heat"), Sense::hover());
    if let (Some(pos), Some(s)) = (resp.hover_pos(), snap) {
        let h = (((pos.x - grid.min.x) / cw).floor() as usize).min(usage::HOURS - 1);
        let d = (((pos.y - grid.min.y) / ch).floor() as usize).min(usage::DAYS - 1);
        let cell = heat.cells.get(d).map(|row| row[h]).unwrap_or_default();
        let day = heat.days.get(d).copied().unwrap_or(s.at);
        let date =
            superba_core::Timestamp(day.as_millis() + s.offset_secs * 1000).utc_date_string();
        let mut tip = format!("{} {} {h:02}:00", date, day_label(cx, heat, d, offset));
        if d == usage::DAYS - 1 && h > heat.now_hour {
            tip.push('\n');
            tip.push_str(&cx.tr.t("overview.heat.future"));
        } else if cell.requests == 0 {
            tip.push('\n');
            tip.push_str(&cx.tr.t("overview.heat.none"));
        } else {
            tip.push('\n');
            tip.push_str(&cx.tr.f(
                "overview.heat.cell",
                &[
                    ("tokens", &kpi::si(cell.tokens).text()),
                    ("n", &cell.requests),
                ],
            ));
            if cell.cost_micros > 0 {
                tip.push_str(&format!(" · {}", kpi::usd(cell.cost_micros).text()));
            }
        }
        if let Some(since) = heat.complete_since {
            tip.push('\n');
            tip.push_str(&cx.tr.f(
                "overview.heat.partial",
                &[("since", &crate::pages::clock_time(since, false))],
            ));
        }
        resp.on_hover_text(tip);
    }
}
