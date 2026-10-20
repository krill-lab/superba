#![allow(missing_docs)]
//! The design widgets the Router page was rebuilt with (ui-spec rules 4 and 6):
//! `Table` with right-aligned number columns and a token-high header, and the
//! `Disclosure` that holds 「高级」 and the details expanders.

mod support;

use egui::accesskit::{Action, Role};
use egui::{Event, Rect, Theme, pos2, vec2};
use superba_gui::design::tokens::size;
use superba_gui::design::widgets::{Disclosure, QuotePreview, Table, Text};
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

#[test]
fn the_reply_preview_is_exactly_as_wide_as_its_column() {
    // It was 2 pt wider (margins and stroke), which widened the settings
    // column under it and pushed the switches beside it into the scroll gutter.
    for theme in [Theme::Light, Theme::Dark] {
        let h = Harness::new([600.0, 300.0], theme);
        let mut rects = (Rect::NOTHING, Rect::NOTHING);
        h.settle(&mut |ui| {
            ui.set_width(400.0);
            let column = ui.max_rect();
            let r = QuotePreview::new("Done.", "⚡ 87 tok/s")
                .caption("Preview")
                .show(ui);
            rects = (column, r.rect);
        });
        let (column, preview) = rects;
        assert!(
            (preview.width() - column.width()).abs() < EPS,
            "{preview:?} in {column:?}"
        );
    }
}

/// ui-spec rule 7: a header label wider than its column is shortened with "…"
/// inside the column (it ran into the next column's label: "Anthropic
/// Messages" over "Gemini" in the conversion matrix at 960 pt); the
/// cells stay where they were, and a wide table shows every label whole.
#[test]
fn a_header_label_wider_than_its_column_is_shortened_inside_it() {
    let labels = [
        "Client protocol".to_owned(),
        "Anthropic Messages API".to_owned(),
        "Requests in the last 24 hours".to_owned(),
    ];
    for (width, shortened) in [(200.0, true), (900.0, false)] {
        let h = Harness::new([width, 200.0], Theme::Light);
        let mut columns = Vec::new();
        let mut draw = |ui: &mut egui::Ui| {
            let table = Table::new(&[1.0, 1.0, 1.0]).right_aligned(&[2]);
            let top = ui.cursor().min.y;
            let left = ui.cursor().min.x;
            let right = left + ui.available_width();
            table.header(ui, &labels);
            columns = table.cells(Rect::from_min_max(
                pos2(left, top),
                pos2(right, top + size::TABLE_HEADER),
            ));
        };
        h.settle(&mut draw);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 200.0))),
            ..Default::default()
        };
        let out = h.ctx.run_ui(input, |ui| draw(ui));
        // (text, where, shortened); a shortened galley keeps its full text.
        let drawn: Vec<(String, Rect, bool)> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) if !t.galley.text().is_empty() => Some((
                    t.galley.text().to_owned(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                    t.galley.elided,
                )),
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        assert_eq!(drawn.len(), 3, "{drawn:?}");
        for ((text, rect, elided), (label, col)) in drawn.iter().zip(labels.iter().zip(&columns)) {
            assert_eq!(text, label);
            // Inside its column (a label sized to its column may round over by 1 pt).
            assert!(
                rect.min.x >= col.min.x - 1.0 && rect.max.x <= col.max.x + 1.0,
                "{width}: {text:?} at {rect:?} outside {col:?}"
            );
            assert_eq!(
                *elided, shortened,
                "{width}: {text:?} at {rect:?} in {col:?}"
            );
        }
        let drawn: Vec<(String, Rect)> = drawn.into_iter().map(|(t, r, _)| (t, r)).collect();
        // A right-aligned label ends on its column's right edge, shortened or not.
        assert!((drawn[2].1.max.x - columns[2].max.x).abs() < 1.0);
        // No two labels overlap.
        for (i, (a, ra)) in drawn.iter().enumerate() {
            for (b, rb) in &drawn[i + 1..] {
                assert!(!ra.intersects(*rb), "{a:?} {ra:?} overlaps {b:?} {rb:?}");
            }
        }
    }
}

/// What one frame of [`inline_frame`] drew.
struct Inline {
    out: egui::FullOutput,
    texts: Vec<(String, Rect)>,
    /// Header lines of the two disclosures.
    lines: [Rect; 2],
    /// Left edge of each body, when it was drawn.
    body_left: [Option<f32>; 2],
}

/// Two inline disclosures, the second open by default.
fn inline_frame(ctx: &egui::Context, events: Vec<Event>) -> Inline {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0))),
        events,
        ..Default::default()
    };
    let mut lines = [Rect::NOTHING; 2];
    let mut body_left = [None; 2];
    let mut out = ctx.run_ui(input, |ui| {
        for (i, (salt, label)) in [("details", "Line details"), ("rules", "Recovery rules")]
            .into_iter()
            .enumerate()
        {
            let left = ui.cursor().min.x;
            let top = ui.cursor().min.y;
            let r = Disclosure::new(salt, label)
                .default_open(i == 1)
                .show(ui, |ui| {
                    body_left[i] = Some(ui.cursor().min.x);
                    ui.label(format!("Body {i}"));
                });
            assert_eq!(r.open, r.inner.is_some());
            lines[i] = Rect::from_min_max(
                pos2(left, top),
                pos2(r.header.rect.max.x, top + r.header.rect.height()),
            );
        }
    });
    out.textures_delta.clear();
    let texts = out
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((
                t.galley.text().to_owned(),
                t.galley.rect.translate(t.pos.to_vec2()),
            )),
            _ => None,
        })
        .collect();
    Inline {
        out,
        texts,
        lines,
        body_left,
    }
}

/// The inline disclosure (details expanders inside a section): a 13 pt label
/// on a CONTROL_SM line, closed by default unless asked, each with its own
/// state, the body under it at the header's left edge (not indented 18 pt
/// as egui's CollapsingHeader did), and the header named and expandable for
/// screen readers.
#[test]
fn inline_disclosures_keep_their_own_state_and_put_the_body_under_the_header() {
    let page = Page::new();
    let ctx = &page.ctx;
    inline_frame(ctx, Vec::new());
    let f = inline_frame(ctx, Vec::new());
    // Closed by default; default_open(true) starts open.
    assert!(!f.texts.iter().any(|(t, _)| t == "Body 0"));
    assert!(f.texts.iter().any(|(t, _)| t == "Body 1"));
    assert_eq!(f.body_left[0], None);
    for line in f.lines {
        assert!((line.height() - size::CONTROL_SM).abs() < EPS, "{line:?}");
    }
    // The label is the small size, not a section heading.
    let label = find(&f.texts, "Line details");
    assert!(
        label.height() < size::CONTROL_SM && label.height() > 10.0,
        "{label:?}"
    );
    // The open body starts at the header line's left edge.
    let left = f.body_left[1].expect("open body drawn");
    assert!(
        (left - f.lines[1].min.x).abs() < EPS,
        "{left} {:?}",
        f.lines[1]
    );
    let update = f
        .out
        .platform_output
        .accesskit_update
        .expect("accesskit on");
    let mut triangles: Vec<(String, Option<bool>)> = update
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::DisclosureTriangle)
        .map(|(_, n)| (n.label().unwrap_or_default().to_owned(), n.is_expanded()))
        .collect();
    triangles.sort();
    assert_eq!(
        triangles,
        [
            ("Line details".to_owned(), Some(false)),
            ("Recovery rules".to_owned(), Some(true))
        ]
    );
    // Opening the first leaves the second as it was.
    let at = label.center();
    let button = |pressed| Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    inline_frame(ctx, vec![Event::PointerMoved(at), button(true)]);
    inline_frame(ctx, vec![button(false)]);
    for _ in 0..30 {
        inline_frame(ctx, Vec::new());
    }
    let f = inline_frame(ctx, Vec::new());
    assert!(f.texts.iter().any(|(t, _)| t == "Body 0"));
    assert!(f.texts.iter().any(|(t, _)| t == "Body 1"));
    let left = f.body_left[0].expect("opened body drawn");
    assert!(
        (left - f.lines[0].min.x).abs() < EPS,
        "{left} {:?}",
        f.lines[0]
    );
}
