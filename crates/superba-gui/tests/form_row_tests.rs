#![allow(missing_docs)]
//! Form rows (user bug 19): a label and the controls of its row share one center
//! line — a text label next to a dropdown, a text field or a small button, a
//! radio-button label (Updates' 固定版本 row), a two-line settings label next to a
//! right-aligned toggle — whatever the controls' heights, in both themes; a
//! control taller than the line grows the line instead of sitting off-center;
//! multi-line content keeps the label on its first line; the hint sits under
//! the controls.

mod support;

use egui::{Rect, Response, TextEdit, Theme, Ui, vec2};
use superba_gui::design::Palette;
use superba_gui::design::tokens::size;
use superba_gui::design::widgets::{Badge, Button, Dropdown, FormRow, Text, Toggle};
use support::ui::Harness;

/// Allowed difference between two centers.
const EPS: f32 = 0.5;

fn harnesses() -> Vec<Harness> {
    [Theme::Light, Theme::Dark]
        .into_iter()
        .map(|t| Harness::new([800.0, 600.0], t))
        .collect()
}

fn assert_same_center(what: &str, a: Rect, b: Rect) {
    assert!(
        (a.center().y - b.center().y).abs() <= EPS,
        "{what}: centers {} vs {} ({a:?} / {b:?})",
        a.center().y,
        b.center().y
    );
}

fn dropdown(ui: &mut Ui, id: &str, text: &str) -> Response {
    Dropdown::new(id)
        .selected_text(text)
        .width(200.0)
        .show_ui(ui, |ui| {
            ui.label("0.162.0");
        })
        .response
}

#[test]
fn text_label_and_dropdown_share_the_center_line() {
    for h in harnesses() {
        let mut out = None;
        let texts = h.settle(&mut |ui| {
            ui.add_space(13.0);
            let r = FormRow::new("Fixed version").show(ui, |ui| dropdown(ui, "pick", "0.162.0"));
            out = Some((r.label_rect, r.inner.rect, r.line));
        });
        let (label, control, line) = out.unwrap();
        assert_same_center("label / dropdown", label, control);
        assert_same_center("label / line", label, line);
        assert_eq!(line.height(), size::CONTROL_SM);
        // The texts as drawn: the label and the dropdown's value.
        let find = |s: &str| texts.iter().find(|(t, _)| t == s).unwrap().1;
        assert_same_center("drawn texts", find("Fixed version"), find("0.162.0"));
    }
}

#[test]
fn a_radio_label_lines_up_with_a_dropdown_badge_and_checkbox() {
    // Updates, 桌面 app 使用的 Codex: [(•) 固定版本] [dropdown] [badge] [checkbox].
    for h in harnesses() {
        let mut out = None;
        let mut choice = 1;
        let mut pre = false;
        h.settle(&mut |ui| {
            let r = FormRow::default().label_width(120.0).show_with(
                ui,
                |ui| ui.radio_value(&mut choice, 1, "固定版本"),
                |ui| {
                    let d = dropdown(ui, "pin", "0.162.0");
                    let b = ui.add(Badge::new("Matches the app"));
                    let c = ui.checkbox(&mut pre, "Show pre-releases");
                    (d, b, c)
                },
            );
            out = Some((r.inner.0.rect, r.inner.1, r.line));
        });
        let (radio, (d, b, c), line) = out.unwrap();
        assert_same_center("radio / dropdown", radio, d.rect);
        assert_same_center("radio / badge", radio, b.rect);
        assert_same_center("radio / checkbox", radio, c.rect);
        assert!(
            d.rect.min.x >= line.min.x,
            "the controls start in their column"
        );
    }
}

#[test]
fn text_fields_buttons_and_toggles_center_on_the_label() {
    for h in harnesses() {
        let mut out = None;
        let mut text = String::from("/usr/local/bin/codex");
        let mut on = true;
        h.settle(&mut |ui| {
            let r = FormRow::new("Custom path").show(ui, |ui| {
                let t = ui.add(
                    TextEdit::singleline(&mut text)
                        .desired_width(240.0)
                        .margin(vec2(8.0, 5.0)),
                );
                let b = ui.add(Button::ghost("Browse…").small());
                let g = ui.add(Toggle::new(&mut on, "persist"));
                (t, b, g)
            });
            out = Some((r.label_rect, r.inner));
        });
        let (label, (t, b, g)) = out.unwrap();
        assert_same_center("label / text field", label, t.rect);
        assert_same_center("label / button", label, b.rect);
        assert_same_center("label / toggle", label, g.rect);
    }
}

#[test]
fn a_taller_control_grows_the_line_instead_of_sitting_low() {
    // A 32 px (medium) dropdown in a row whose line is the default 28 px.
    for h in harnesses() {
        let mut out = None;
        h.settle(&mut |ui| {
            let r = FormRow::new("Model").show(ui, |ui| {
                let small = ui.add(Badge::new("busy"));
                let d = Dropdown::new("m")
                    .medium()
                    .width(200.0)
                    .selected_text("gpt-5.5")
                    .show_ui(ui, |_| {})
                    .response;
                (small, d)
            });
            out = Some((r.label_rect, r.inner, r.line));
        });
        let (label, (small, d), line) = out.unwrap();
        assert_eq!(d.rect.height(), size::CONTROL);
        assert_eq!(line.height(), size::CONTROL);
        assert_same_center("label / medium dropdown", label, d.rect);
        assert_same_center("badge before it / medium dropdown", small.rect, d.rect);
    }
}

#[test]
fn a_two_line_label_and_a_right_aligned_toggle_share_the_center() {
    // Preferences: title + description on the left, the switch on the right.
    for h in harnesses() {
        let mut out = None;
        let mut on = false;
        h.settle(&mut |ui| {
            let pal = Palette::of(ui.ctx());
            let r = FormRow::default()
                .label_width(400.0)
                .height(56.0)
                .right()
                .show_with(
                    ui,
                    |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(Text::strong("Safe mode", pal));
                            ui.add(
                                egui::Label::new(Text::small(
                                    "Check the new provider before switching and roll back \
                                     automatically when it fails, so a typo in a key never \
                                     leaves Codex without a working connection.",
                                    pal,
                                ))
                                .wrap(),
                            );
                        })
                        .response
                    },
                    |ui| ui.add(Toggle::new(&mut on, "Safe mode")),
                );
            out = Some((r.inner, r.line, r.rect));
        });
        let ((block, toggle), line, rect) = out.unwrap();
        assert!(
            block.rect.height() > 56.0,
            "the description wraps: {block:?}"
        );
        assert_same_center("label block / toggle", block.rect, toggle.rect);
        assert_same_center("label block / line", block.rect, line);
        assert!(
            (toggle.rect.max.x - rect.max.x).abs() <= EPS,
            "right-aligned"
        );
    }
}

#[test]
fn a_short_label_block_is_centered_on_a_taller_row() {
    // Preferences rows are 56 px high; a title over a one-line description is
    // shorter. `ui.vertical` starts at the top of its parent, so the row has to
    // place the block, not the layout.
    for h in harnesses() {
        let mut out = None;
        let mut on = true;
        h.settle(&mut |ui| {
            let pal = Palette::of(ui.ctx());
            let r = FormRow::default()
                .label_width(400.0)
                .height(56.0)
                .right()
                .show_with(
                    ui,
                    |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(Text::strong("Safe mode", pal));
                            ui.label(Text::small("One line.", pal));
                        })
                        .response
                    },
                    |ui| ui.add(Toggle::new(&mut on, "Safe mode")),
                );
            out = Some((r.inner, r.line));
        });
        let ((block, toggle), line) = out.unwrap();
        assert!(block.rect.height() < 56.0, "{block:?}");
        assert_eq!(line.height(), 56.0);
        assert_same_center("short block / toggle", block.rect, toggle.rect);
        assert_same_center("short block / line", block.rect, line);
    }
}

#[test]
fn multi_line_content_keeps_the_label_on_the_first_line() {
    for h in harnesses() {
        let mut out = None;
        let mut text = "line 1\nline 2\nline 3\nline 4".to_owned();
        h.settle(&mut |ui| {
            let r = FormRow::new("Value").show(ui, |ui| {
                ui.add(TextEdit::multiline(&mut text).desired_rows(4))
            });
            out = Some((r.label_rect, r.inner, r.line));
        });
        let (label, editor, line) = out.unwrap();
        assert!(editor.rect.height() > size::CONTROL_LG);
        assert_eq!(line.height(), size::CONTROL_SM, "the line does not grow");
        assert_same_center("label / first line", label, line);
        assert!(
            (editor.rect.min.y - line.min.y).abs() <= EPS,
            "content starts at the line's top"
        );
    }
}

#[test]
fn the_hint_sits_under_the_controls_in_their_column() {
    for h in harnesses() {
        let mut out = None;
        let texts = h.settle(&mut |ui| {
            let r = FormRow::new("Fixed version")
                .hint("Downloaded and verified by Superba")
                .show(ui, |ui| dropdown(ui, "pick", "0.162.0"));
            ui.label("after");
            out = Some((r.inner.rect, r.rect));
        });
        let (control, row) = out.unwrap();
        let find = |s: &str| texts.iter().find(|(t, _)| t == s).unwrap().1;
        let hint = find("Downloaded and verified by Superba");
        assert!(
            hint.min.y >= control.max.y,
            "hint {hint:?} under {control:?}"
        );
        assert!(
            (hint.min.x - control.min.x).abs() <= EPS,
            "hint aligned with the control"
        );
        assert!(row.max.y >= hint.max.y, "the row holds its hint");
        assert!(
            find("after").min.y >= hint.max.y,
            "the next widget comes after the hint"
        );
    }
}

#[test]
fn rows_stack_with_their_controls_in_one_column() {
    for h in harnesses() {
        let mut rects = Vec::new();
        h.settle(&mut |ui| {
            rects.clear();
            for (label, id) in [("Name", "a"), ("A much longer label", "b"), ("", "c")] {
                let r = FormRow::new(label)
                    .label_width(150.0)
                    .show(ui, |ui| dropdown(ui, id, id));
                rects.push((r.label_rect, r.inner.rect));
            }
        });
        let x = rects[0].1.min.x;
        for (i, (label, control)) in rects.iter().enumerate() {
            assert!(
                (control.min.x - x).abs() <= EPS,
                "row {i} starts in the column"
            );
            if i < 2 {
                assert_same_center(&format!("row {i}"), *label, *control);
            }
        }
        assert!(rects[1].1.min.y >= rects[0].1.max.y, "rows do not overlap");
    }
}
