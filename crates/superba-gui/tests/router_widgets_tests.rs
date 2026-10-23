#![allow(missing_docs)]
//! B7 widgets (UX §4): LogTable (virtualised: 10,000 rows cost what the visible
//! rows cost; widths fit the area so no horizontal scrolling), SourceList,
//! InfoCard, DetailPane, Toolbar and QuotePreview (body-colour quote text).

use std::cell::Cell;
use std::time::{Duration, Instant};

use egui::{Rect, pos2};
use pretty_assertions::assert_eq;
use superba_gui::design::widgets::detail_pane::{self, DetailPane};
use superba_gui::design::widgets::info_card::{self, InfoCard};
use superba_gui::design::widgets::log_table::{LogCell, LogColumn, LogTable, fit_widths};
use superba_gui::design::widgets::source_list::{SourceEntry, SourceList};
use superba_gui::design::widgets::toolbar::{self, Toolbar, tool_button, tool_toggle};
use superba_gui::design::widgets::{QuotePreview, Status};
use superba_gui::design::{Palette, fonts, style};

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    style::install(&ctx, Some(egui::Theme::Light));
    ctx
}

fn run(
    ctx: &egui::Context,
    rect: Rect,
    events: Vec<egui::Event>,
    mut add: impl FnMut(&mut egui::Ui),
) -> Vec<(String, Rect, egui::Color32)> {
    let out = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(1600.0, 1000.0))),
            events,
            ..Default::default()
        },
        |ui| {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            add(&mut child);
        },
    );
    let t = texts(&out);
    out.drop_without_applying_deltas();
    t
}

fn texts(out: &egui::FullOutput) -> Vec<(String, Rect, egui::Color32)> {
    out.shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((
                t.galley.text().to_owned(),
                t.galley.rect.translate(t.pos.to_vec2()),
                t.galley
                    .job
                    .sections
                    .first()
                    .map_or(t.fallback_color, |s| s.format.color),
            )),
            _ => None,
        })
        .collect()
}

fn columns() -> Vec<LogColumn> {
    (0..13)
        .map(|i| {
            if i % 2 == 0 {
                LogColumn::left(format!("col{i}"), 60.0 + i as f32)
            } else {
                LogColumn::right(format!("num{i}"), 50.0)
            }
        })
        .collect()
}

#[test]
fn column_widths_fill_the_area_exactly() {
    let w = fit_widths(&[76.0, 60.0, 120.0], 512.0);
    assert!((w.iter().sum::<f32>() - 512.0).abs() < 0.01);
    assert!((w[2] / w[1] - 2.0).abs() < 0.001, "proportions kept");
    assert_eq!(fit_widths(&[], 100.0), Vec::<f32>::new());
}

#[test]
fn ten_thousand_rows_only_build_the_visible_ones_within_a_frame_budget() {
    let ctx = context();
    let cols = columns();
    let rect = Rect::from_min_size(pos2(10.0, 10.0), egui::vec2(834.0, 420.0));
    let calls = Cell::new(0usize);
    let mut worst = Duration::ZERO;
    for frame in 0..12 {
        calls.set(0);
        let start = Instant::now();
        let out = run(&ctx, rect, Vec::new(), |ui| {
            LogTable::new("perf", &cols, 10_000)
                .selected(Some(3))
                .totals((0..13).map(|i| LogCell::text(format!("t{i}"))).collect())
                .show(ui, |row, col| {
                    calls.set(calls.get() + 1);
                    LogCell::text(format!("{row}:{col}"))
                });
        });
        let took = start.elapsed();
        if frame > 1 {
            worst = worst.max(took);
        }
        // About 420 / 26 visible rows (+1 partially visible) of 13 cells.
        assert!(calls.get() <= 13 * 20, "built {} cells", calls.get());
        let shown: Vec<String> = out.clone().into_iter().map(|t| t.0).collect();
        assert!(shown.contains(&"0:0".to_owned()));
        assert!(!shown.contains(&"9999:0".to_owned()));
    }
    // Generous for debug builds and loaded CI machines; a release frame is ~1 ms.
    assert!(worst < Duration::from_millis(250), "worst frame {worst:?}");
}

#[test]
fn all_columns_stay_inside_the_table() {
    let ctx = context();
    let cols = columns();
    let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(700.0, 300.0));
    let mut out = None;
    for _ in 0..2 {
        out = Some(run(&ctx, rect, Vec::new(), |ui| {
            LogTable::new("inside", &cols, 3)
                .sort(1, true)
                .show(ui, |row, col| {
                    LogCell::text(format!("value {row} {col} long text"))
                });
        }));
    }
    for (t, r, _) in out.unwrap() {
        assert!(r.max.x <= rect.max.x + 0.5, "{t} at {r:?}");
    }
}

#[test]
fn a_click_selects_a_row_and_a_header_click_sorts() {
    let ctx = context();
    let cols = columns();
    let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(834.0, 300.0));
    let mut clicked = None;
    let mut sorted = None;
    let mut frame = |events: Vec<egui::Event>| {
        run(&ctx, rect, events, |ui| {
            let r = LogTable::new("click", &cols, 5)
                .show(ui, |row, col| LogCell::text(format!("r{row}c{col}")));
            if r.clicked.is_some() {
                clicked = r.clicked;
            }
            if r.sort_clicked.is_some() {
                sorted = r.sort_clicked;
            }
        })
    };
    let out = frame(Vec::new());
    let target = out
        .clone()
        .into_iter()
        .find(|t| t.0 == "r2c0")
        .unwrap()
        .1
        .center();
    let header = out
        .clone()
        .into_iter()
        .find(|t| t.0 == "num1")
        .unwrap()
        .1
        .center();
    let press = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    for pos in [target, header] {
        frame(vec![egui::Event::PointerMoved(pos)]);
        frame(vec![press(pos, true)]);
        frame(vec![press(pos, false)]);
    }
    assert_eq!(clicked, Some(2));
    assert_eq!(sorted, Some(1));
}

#[test]
fn the_source_list_selects_entries_and_collapses() {
    let ctx = context();
    let entries = vec![
        SourceEntry {
            key: "a".into(),
            label: "登录页样式".into(),
            count: 212,
            ..SourceEntry::default()
        },
        SourceEntry {
            key: "b".into(),
            label: "写周报".into(),
            count: 12,
            ..SourceEntry::default()
        },
    ];
    let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(150.0, 400.0));
    let mut selected: Option<String> = None;
    let mut collapsed = false;
    let mut grouping = 0u8;
    let out = run(&ctx, rect, Vec::new(), |ui| {
        let _ = SourceList::new("s", &mut selected, &mut collapsed, &entries)
            .grouping(
                &mut grouping,
                vec![(0, "会话".into()), (1, "供应商".into())],
            )
            .all("全部", 224)
            .footnote("远程主机的请求不经过本地路由")
            .show(ui);
    });
    let t: Vec<String> = out.clone().into_iter().map(|t| t.0).collect();
    for want in [
        "全部",
        "224",
        "登录页样式",
        "212",
        "写周报",
        "会话",
        "供应商",
    ] {
        assert!(t.iter().any(|x| x == want), "{want} missing in {t:?}");
    }
    let pos = out
        .clone()
        .into_iter()
        .find(|x| x.0 == "写周报")
        .unwrap()
        .1
        .center();
    let press = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    let mut changed = false;
    for events in [
        vec![egui::Event::PointerMoved(pos)],
        vec![press(true)],
        vec![press(false)],
    ] {
        run(&ctx, rect, events, |ui| {
            let r = SourceList::new("s", &mut selected, &mut collapsed, &entries)
                .grouping(
                    &mut grouping,
                    vec![(0, "会话".into()), (1, "供应商".into())],
                )
                .all("全部", 224)
                .show(ui);
            changed |= r.selection_changed;
        });
    }
    assert!(changed);
    assert_eq!(selected.as_deref(), Some("b"));
    // Collapsed: only a strip with the chevron.
    collapsed = true;
    let out = run(&ctx, rect, Vec::new(), |ui| {
        let _ = SourceList::<u8>::new("s", &mut selected, &mut collapsed, &entries).show(ui);
    });
    assert!(out.clone().iter().all(|t| t.0 != "写周报"));
}

#[test]
fn provider_lines_are_indented_sub_entries_and_the_tabs_fit_150_px_in_english() {
    let ctx = context();
    let entry = |key: &str, label: &str, count: u64, child: bool| SourceEntry {
        tool: None,
        key: key.into(),
        label: label.into(),
        count,
        tooltip: None,
        child,
    };
    let entries = vec![
        entry("krill", "Krill", 80, false),
        entry("krill\u{1f}cn", "China Express", 50, true),
        entry("krill\u{1f}ov", "Overseas", 30, true),
        entry("my-api", "Example API", 28, false),
    ];
    let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(150.0, 400.0));
    let mut selected: Option<String> = None;
    let mut collapsed = false;
    let mut grouping = 1u8;
    let mut show = |events| {
        run(&ctx, rect, events, |ui| {
            let _ = SourceList::new("p", &mut selected, &mut collapsed, &entries)
                .grouping(
                    &mut grouping,
                    vec![(0, "Sessions".into()), (1, "Providers".into())],
                )
                .all("All", 108)
                .show(ui);
        })
    };
    let out = show(Vec::new());
    // Unabbreviated widths of the tab labels at the smallest UI size.
    let mut full = 0.0f32;
    run(&ctx, rect, Vec::new(), |ui| {
        let f = superba_gui::design::fonts::font(
            superba_gui::design::tokens::text::CAPTION,
            superba_gui::design::fonts::Weight::Regular,
        );
        full = ui
            .painter()
            .layout_no_wrap("Providers".into(), f, egui::Color32::WHITE)
            .size()
            .x;
    });
    let at = |t: &str| {
        out.iter()
            .find(|x| x.0 == t)
            .unwrap_or_else(|| {
                panic!(
                    "{t} missing in {:?}",
                    out.iter().map(|x| &x.0).collect::<Vec<_>>()
                )
            })
            .1
    };
    // Both grouping tabs unabbreviated next to the chevron, inside the column
    // (an elided label would be narrower than the whole word).
    assert!(full > 40.0);
    assert!(
        at("Providers").width() >= full - 0.5,
        "{:?} < {full}",
        at("Providers")
    );
    for tab in ["Sessions", "Providers"] {
        assert!(
            at(tab).max.x <= 150.0 - 8.0 - 20.0,
            "{tab} at {:?}",
            at(tab)
        );
    }
    // Lines sit indented under their provider, counts stay right-aligned.
    let krill = at("Krill");
    let line = at("China Express");
    assert!(line.min.x >= krill.min.x + 10.0, "{krill:?} {line:?}");
    assert!(line.min.y > krill.min.y && at("Overseas").min.y > line.min.y);
    assert!(at("Example API").min.x == krill.min.x);
    for count in ["80", "50", "30", "28"] {
        assert!(
            at(count).max.x <= 150.0 && at(count).max.x >= 120.0,
            "{count}"
        );
    }
    // A sub-entry is selected by its own key.
    let pos = line.center();
    let press = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    show(vec![egui::Event::PointerMoved(pos)]);
    show(vec![press(true)]);
    show(vec![press(false)]);
    assert_eq!(selected.as_deref(), Some("krill\u{1f}cn"));
}

/// User bug 10: the detail pane is sized to its content and the split between
/// the log and the pane is clamped: never below the pane's minimum, never more
/// than leaves the log its minimum, never more than the room.
#[test]
fn the_detail_pane_height_is_clamped_between_the_log_and_its_minimum() {
    let min = detail_pane::MIN_HEIGHT;
    // Header, three rows and the totals row of the log.
    let log = 28.0 + 4.0 * 26.0;
    assert_eq!(detail_pane::clamp_height(236.0, 504.0, log), 236.0);
    assert_eq!(detail_pane::clamp_height(900.0, 504.0, log), 504.0 - log);
    assert_eq!(detail_pane::clamp_height(10.0, 504.0, log), min);
    // Little room: the pane keeps its minimum; no room: never more than there is.
    assert_eq!(detail_pane::clamp_height(236.0, 200.0, log), min);
    assert_eq!(detail_pane::clamp_height(236.0, 80.0, log), 80.0);
    assert!(min > detail_pane::CHROME);
}

/// An InfoCard is exactly as wide and as tall as it is told, border included,
/// so a clip rectangle around it never cuts its right or bottom border, and its
/// height is known before it is drawn.
#[test]
fn info_cards_draw_their_frame_inside_their_rect() {
    let ctx = context();
    for lines in 1..=4 {
        let rect = Rect::from_min_size(pos2(10.0, 10.0), egui::vec2(241.0, 300.0));
        let mut card = Rect::NOTHING;
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0))),
                ..Default::default()
            },
            |ui| {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                let mut c = InfoCard::new("会话");
                for i in 0..lines {
                    c = c.line("缓存键", format!("019a7c00-0000-7000-8000-00000000000{i}"));
                }
                card = c.show(&mut child).response.rect;
            },
        );
        assert_eq!(
            card,
            Rect::from_min_size(rect.min, egui::vec2(241.0, info_card::height(lines))),
            "{lines} lines"
        );
        let frames: Vec<Rect> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) if r.stroke.width > 0.0 => {
                    Some(s.shape.visual_bounding_rect())
                }
                _ => None,
            })
            .collect();
        assert!(!frames.is_empty());
        for f in frames {
            assert!(card.expand(0.01).contains_rect(f), "{f:?} outside {card:?}");
        }
        out.drop_without_applying_deltas();
    }
    // Each line adds its row and the gap above it.
    assert_eq!(
        info_card::height(3),
        info_card::height(2) + info_card::LINE_GAP + info_card::LINE_HEIGHT
    );
}

#[test]
fn info_cards_detail_panes_and_toolbars_draw_their_parts() {
    let ctx = context();
    let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 260.0));
    let mut tab = 0usize;
    let out = run(&ctx, rect, Vec::new(), |ui| {
        DetailPane::new("d", &mut tab)
            .tabs(vec!["通用".into(), "计时".into()])
            .title("登录页样式 · gpt-5.5")
            .status(Status::Ok, "完成 · HTTP 200")
            .show(ui, |ui, t| {
                assert_eq!(t, 0);
                InfoCard::new("用量")
                    .line("输入", "668")
                    .mono("缓存键", "019a7c00-0000-7000-8000-0000000000e1")
                    .show(ui);
            });
    });
    let t: Vec<String> = out.clone().into_iter().map(|t| t.0).collect();
    for want in ["通用", "计时", "完成 · HTTP 200", "用量", "输入", "668"] {
        assert!(t.iter().any(|x| x == want), "{want} missing in {t:?}");
    }
    let bar = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, toolbar::HEIGHT));
    let out = run(&ctx, bar, Vec::new(), |ui| {
        let ((), ()) = Toolbar::show(
            ui,
            |ui| {
                tool_button(ui, None, "清空", true);
                tool_toggle(ui, "记录请求内容", true, Status::Warn, true);
            },
            |ui| {
                tool_button(ui, None, "收起详情", true);
            },
        );
    });
    let t: Vec<String> = out.clone().into_iter().map(|t| t.0).collect();
    assert!(t.contains(&"清空".to_owned()) && t.contains(&"收起详情".to_owned()));
}

#[test]
fn the_quote_preview_uses_the_body_colour_like_the_app() {
    let ctx = context();
    let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(480.0, 200.0));
    let line = "⚡ 87 tok/s · 首字 0.82s · 本轮 12,345 in / 321 out";
    let out = run(&ctx, rect, Vec::new(), |ui| {
        QuotePreview::new("好的。", line)
            .caption("预览 · 示例数据")
            .show(ui);
    });
    let pal = Palette::of(&ctx);
    let quote = out
        .clone()
        .into_iter()
        .find(|t| t.0 == line.trim_start_matches("⚡ "))
        .expect("quote drawn");
    assert_eq!(quote.2, pal.text, "body colour, not grey text");
}
