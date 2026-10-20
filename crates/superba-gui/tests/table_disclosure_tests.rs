#![allow(missing_docs)]
//! The design widgets the Router page was rebuilt with (ui-spec rules 4 and 6):
//! `Table` with right-aligned number columns and a token-high header, and the
//! `Disclosure` that holds 「高级」 and the details expanders.

mod support;

use egui::accesskit::{Action, Role};
use egui::{Event, Rect, Theme, pos2, vec2};
use superba_gui::design::tokens::size;
use superba_gui::design::widgets::{Disclosure, Table, Text};
use superba_gui::design::{Palette, fonts, style};
use support::ui::Harness;

const EPS: f32 = 0.5;

fn find(texts: &[(String, Rect)], s: &str) -> Rect {
    texts
        .iter()
        .find(|(t, _)| t == s)
        .unwrap_or_else(|| panic!("{s} not drawn: {texts:?}"))
        .1
}

#[test]
fn right_aligned_columns_end_header_and_cells_on_the_same_edge() {
    for theme in [Theme::Light, Theme::Dark] {
        let h = Harness::new([800.0, 400.0], theme);
        let mut header = Rect::NOTHING;
        let texts = h.settle(&mut |ui| {
            let pal = Palette::of(ui.ctx());
            let table = Table::new(&[1.0, 1.0, 1.0]).right_aligned(&[1]);
            let top = ui.cursor().min.y;
            table.header(ui, &["Name".into(), "Requests".into(), "Address".into()]);
            let bottom = ui.cursor().min.y - ui.spacing().item_spacing.y;
            header = Rect::from_min_max(pos2(0.0, top), pos2(800.0, bottom));
            for (i, (name, n)) in [("Codex", "318"), ("Gemini", "0")].into_iter().enumerate() {
                table.row(ui, size::ROW, i == 1, |ui, col| {
                    let text = match col {
                        0 => name,
                        1 => n,
                        _ => "http://127.0.0.1",
                    };
                    ui.label(Text::small(text, pal));
                });
            }
        });
        // The header is the token's height (it was 32 pt).
        assert!(
            (header.height() - size::TABLE_HEADER).abs() < EPS,
            "{header:?}"
        );
        let requests = find(&texts, "Requests");
        for n in ["318", "0"] {
            assert!(
                (find(&texts, n).max.x - requests.max.x).abs() < EPS,
                "{n} ends at {} but its header at {}",
                find(&texts, n).max.x,
                requests.max.x
            );
        }
        // Left columns still start at their left edge, under their header.
        assert!((find(&texts, "Codex").min.x - find(&texts, "Name").min.x).abs() < EPS);
        assert!(
            (find(&texts, "http://127.0.0.1").min.x - find(&texts, "Address").min.x).abs() < EPS
        );
    }
}

#[test]
fn table_headers_are_column_headers_for_screen_readers() {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    style::install(&ctx, Some(Theme::Light));
    ctx.enable_accesskit();
    let input = || egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0))),
        ..Default::default()
    };
    let run = || {
        ctx.run_ui(input(), |ui| {
            Table::new(&[1.0, 1.0, 0.0])
                .min_widths(&[0.0, 0.0, 40.0])
                .right_aligned(&[1])
                .header(ui, &["Client".into(), "Last 24 h".into(), String::new()]);
        })
    };
    run().textures_delta.clear();
    let mut out = run();
    out.textures_delta.clear();
    let update = out.platform_output.accesskit_update.expect("accesskit on");
    let mut headers: Vec<String> = update
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::ColumnHeader)
        .map(|(_, n)| n.label().unwrap_or_default().to_owned())
        .collect();
    headers.sort();
    // The empty header of the actions column is not announced.
    assert_eq!(headers, ["Client", "Last 24 h"]);
}

struct Page {
    ctx: egui::Context,
    shown: usize,
    /// Texts of the last frame drawn shortened ("…").
    elided: Vec<String>,
}

impl Page {
    fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
        style::install(&ctx, Some(Theme::Dark));
        ctx.enable_accesskit();
        Self {
            ctx,
            shown: 0,
            elided: Vec::new(),
        }
    }

    fn frame(&mut self, width: f32, events: Vec<Event>) -> (egui::FullOutput, Vec<(String, Rect)>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 400.0))),
            events,
            ..Default::default()
        };
        let shown = &mut self.shown;
        let mut out = self.ctx.run_ui(input, |ui| {
            Disclosure::new("advanced", "Advanced")
                .section()
                .caption("Reply stats line · Prices · Recording · Service")
                .show(ui, |ui| {
                    *shown += 1;
                    ui.label("Body");
                });
        });
        out.textures_delta.clear();
        let mut texts = Vec::new();
        self.elided.clear();
        for s in &out.shapes {
            if let egui::Shape::Text(t) = &s.shape {
                let text = t.galley.text().to_owned();
                if t.galley.elided {
                    self.elided.push(text.clone());
                }
                texts.push((text, t.galley.rect.translate(t.pos.to_vec2())));
            }
        }
        (out, texts)
    }

    fn header(out: &egui::FullOutput) -> (bool, bool, bool) {
        let update = out
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("accesskit on");
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::DisclosureTriangle)
            .expect("a disclosure triangle");
        (
            node.label() == Some("Advanced"),
            node.is_expanded() == Some(true),
            node.supports_action(Action::Click),
        )
    }

    fn click(&mut self, at: egui::Pos2) {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(600.0, vec![Event::PointerMoved(at), button(true)]);
        self.frame(600.0, vec![button(false)]);
        for _ in 0..30 {
            self.frame(600.0, Vec::new());
        }
    }
}

#[test]
fn a_section_disclosure_starts_closed_and_opens_on_click_with_its_state_for_screen_readers() {
    let mut page = Page::new();
    page.frame(600.0, Vec::new());
    let (out, texts) = page.frame(600.0, Vec::new());
    assert_eq!(page.shown, 0, "closed by default");
    assert_eq!(Page::header(&out), (true, false, true));
    assert!(!texts.iter().any(|(t, _)| t == "Body"));
    // A section heading on a row-high line, the caption on the same line.
    let title = find(&texts, "Advanced");
    let caption = find(&texts, "Reply stats line · Prices · Recording · Service");
    assert!(
        (title.center().y - size::ROW / 2.0).abs() < 1.0,
        "{title:?}"
    );
    assert!((caption.center().y - title.center().y).abs() < 1.0);
    assert!(caption.min.x > title.max.x);
    // Clicking the label opens it; the tree says so.
    page.click(title.center());
    let (out, texts) = page.frame(600.0, Vec::new());
    assert!(page.shown > 0);
    assert!(texts.iter().any(|(t, _)| t == "Body"));
    assert_eq!(Page::header(&out), (true, true, true));
    // And again closes it.
    page.click(title.center());
    let (out, texts) = page.frame(600.0, Vec::new());
    assert!(!texts.iter().any(|(t, _)| t == "Body"));
    assert!(!Page::header(&out).1);
}

#[test]
fn a_long_caption_is_shortened_inside_a_narrow_line() {
    let mut page = Page::new();
    page.frame(220.0, Vec::new());
    let (_, texts) = page.frame(220.0, Vec::new());
    let (_, rect) = texts
        .iter()
        .find(|(t, _)| t.starts_with("Reply") || t.starts_with('…'))
        .expect("caption drawn");
    assert_eq!(page.elided.len(), 1, "the caption is shortened: {texts:?}");
    assert!(rect.max.x <= 220.0 + EPS, "{rect:?}");
    // At full width nothing is shortened.
    page.frame(600.0, Vec::new());
    page.frame(600.0, Vec::new());
    assert!(page.elided.is_empty(), "{:?}", page.elided);
}
