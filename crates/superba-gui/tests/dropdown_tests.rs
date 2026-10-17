#![allow(missing_docs)]
//! The shared dropdown (user bug 9: the Router & usage filter dropdowns had their
//! bottom border cut off). The field has a fixed height and paints only inside the
//! rectangle it allocates, so a row as tall as a small control never clips it; its
//! menu opens below it without covering it; and no page uses egui's stock
//! `ComboBox` (whose height followed the font) any more.

use std::cell::{Cell, RefCell};
use std::path::Path;

use egui::{Event, PointerButton, Pos2, Rect, Response, Theme, pos2, vec2};
use superba_gui::design::layout::{self, Track};
use superba_gui::design::tokens::size;
use superba_gui::design::widgets::Dropdown;
use superba_gui::design::widgets::dropdown::MENU_GAP;
use superba_gui::design::{fonts, style};

const SCREEN: [f32; 2] = [640.0, 480.0];

/// One drawn shape: what it covers and the clip rectangle it was drawn in.
#[derive(Clone, Copy, Debug)]
struct Drawn {
    bounds: Rect,
    clip: Rect,
}

struct Ctx {
    ctx: egui::Context,
}

impl Ctx {
    fn new(theme: Theme) -> Self {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
        style::install(&ctx, Some(theme));
        Self { ctx }
    }

    /// Run one frame; returns every shape drawn (texts included).
    fn frame(&self, events: Vec<Event>, f: &mut dyn FnMut(&mut egui::Ui)) -> Vec<Drawn> {
        let output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN.into())),
                events,
                ..Default::default()
            },
            |ui| f(ui),
        );
        let drawn = output
            .shapes
            .iter()
            .map(|s| Drawn {
                bounds: s.shape.visual_bounding_rect(),
                clip: s.clip_rect,
            })
            .filter(|d| d.bounds.is_positive())
            .collect();
        output.drop_without_applying_deltas();
        drawn
    }

    fn click(&self, pos: Pos2, f: &mut dyn FnMut(&mut egui::Ui)) {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(vec![Event::PointerMoved(pos)], f);
        self.frame(vec![Event::PointerMoved(pos), button(true)], f);
        self.frame(vec![button(false)], f);
        self.frame(Vec::new(), f);
    }
}

/// A named dropdown drawn in a row of the given width.
type Case = (&'static str, f32, Box<dyn Fn() -> Dropdown>);

fn same_rect(a: Rect, b: Rect) -> bool {
    (a.min - b.min).length() < 0.01 && (a.max - b.max).length() < 0.01
}

/// Draw `dropdown()` alone in a row exactly as tall as the field (clipped to
/// the row, like `layout::rows` does), `width` wide.
fn in_row(
    ui: &mut egui::Ui,
    width: f32,
    dropdown: &dyn Fn() -> Dropdown,
    seen: &RefCell<Option<Response>>,
) {
    let height = dropdown().height();
    let area = Rect::from_min_size(pos2(40.0, 40.0), vec2(width, 200.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
    layout::rows(
        &mut child,
        &[Track::Fixed(height), Track::Fill],
        6.0,
        |ui, i| {
            if i == 0 {
                ui.horizontal(|ui| {
                    let r = dropdown().show_ui(ui, |ui| {
                        let _ = ui.selectable_label(false, "First");
                        let _ = ui.selectable_label(true, "Second");
                    });
                    *seen.borrow_mut() = Some(r.response);
                });
            }
        },
    );
}

#[test]
fn a_dropdown_paints_only_inside_its_rectangle_and_has_a_fixed_height() {
    let long = "A provider with a very long name that does not fit the field at all";
    let cases: Vec<Case> = vec![
        (
            "small zh",
            600.0,
            Box::new(|| Dropdown::new("d").width(104.0).selected_text("全部供应商")),
        ),
        (
            "small en",
            600.0,
            Box::new(|| {
                Dropdown::new("d")
                    .width(104.0)
                    .selected_text("All statuses")
            }),
        ),
        (
            "medium",
            600.0,
            Box::new(|| {
                Dropdown::new("d")
                    .medium()
                    .width(320.0)
                    .selected_text("gpt-5.5-codex")
            }),
        ),
        (
            "rich mono",
            600.0,
            Box::new(|| {
                Dropdown::new("d").selected_text(
                    egui::RichText::new("gpt-5.5").font(egui::FontId::monospace(13.0)),
                )
            }),
        ),
        (
            "chevron only",
            600.0,
            Box::new(|| Dropdown::new("d").width(20.0).selected_text("")),
        ),
        (
            "long text, narrow row",
            180.0,
            Box::new(move || Dropdown::new("d").width(104.0).selected_text(long)),
        ),
    ];
    for theme in [Theme::Light, Theme::Dark] {
        for (name, width, dropdown) in &cases {
            let h = Ctx::new(theme);
            let seen = RefCell::new(None);
            let mut draw = |ui: &mut egui::Ui| in_row(ui, *width, dropdown.as_ref(), &seen);
            h.frame(Vec::new(), &mut draw);
            let idle = h.frame(Vec::new(), &mut draw);
            let field = seen.borrow().clone().expect("drawn").rect;
            // Hovered: the border turns stronger, still inside.
            let hovered = h.frame(vec![Event::PointerMoved(field.center())], &mut draw);
            assert_eq!(field.height(), dropdown().height(), "{name}: height");
            assert!(
                field.width() <= *width + 0.01,
                "{name}: {} wide in a {width} px row",
                field.width()
            );
            for (state, drawn) in [("idle", &idle), ("hovered", &hovered)] {
                assert!(!drawn.is_empty(), "{name} {state}: nothing drawn");
                for d in drawn {
                    assert!(
                        field.expand(0.01).contains_rect(d.bounds),
                        "{theme:?} {name} {state}: {:?} is outside the field {field:?}",
                        d.bounds
                    );
                }
                // The frame (fill and border) covers the field and the row's clip
                // rectangle leaves all of it visible.
                let frame = drawn
                    .iter()
                    .find(|d| same_rect(d.bounds, field))
                    .unwrap_or_else(|| panic!("{theme:?} {name} {state}: no frame"));
                assert!(
                    frame.clip.expand(0.01).contains_rect(field),
                    "{theme:?} {name} {state}: the row ({:?}) clips the field {field:?}",
                    frame.clip
                );
            }
        }
    }
    assert_eq!(Dropdown::new("d").height(), size::CONTROL_SM);
    assert_eq!(Dropdown::new("d").medium().height(), size::CONTROL);
}

#[test]
fn the_open_menu_starts_below_the_field_and_a_pick_changes_the_value() {
    for theme in [Theme::Light, Theme::Dark] {
        let h = Ctx::new(theme);
        let options = vec![
            (None, "All providers".to_owned()),
            (Some(1), "Krill".to_owned()),
            (Some(2), "Example API".to_owned()),
        ];
        let value: Cell<Option<u8>> = Cell::new(None);
        let changed = Cell::new(false);
        let field = Cell::new(Rect::NOTHING);
        let menu = Cell::new(None);
        let mut draw = |ui: &mut egui::Ui| {
            let area = Rect::from_min_size(pos2(40.0, 40.0), vec2(400.0, size::CONTROL_SM));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
            child.set_clip_rect(area);
            child.horizontal(|ui| {
                menu.set(Some(Dropdown::menu_id(ui, "providers")));
                let mut v = value.get();
                let r = Dropdown::new("providers")
                    .width(104.0)
                    .show_options(ui, &mut v, &options);
                value.set(v);
                field.set(r.rect);
                changed.set(changed.get() || r.changed());
            });
        };
        h.frame(Vec::new(), &mut draw);
        h.frame(Vec::new(), &mut draw);
        let menu_id = menu.get().expect("drawn");
        let field = field.get();
        assert!(!egui::Popup::is_id_open(&h.ctx, menu_id));
        h.click(field.center(), &mut draw);
        assert!(
            egui::Popup::is_id_open(&h.ctx, menu_id),
            "{theme:?}: a click opens the menu"
        );
        let open = h.frame(Vec::new(), &mut draw);
        let menu_rect = h
            .ctx
            .memory(|m| m.area_rect(menu_id))
            .expect("the menu is an area");
        assert!(
            menu_rect.min.y >= field.max.y + MENU_GAP - 0.01,
            "{theme:?}: the menu ({menu_rect:?}) covers the field ({field:?})"
        );
        // The field itself is still drawn whole while its menu is open.
        assert!(
            open.iter()
                .any(|d| same_rect(d.bounds, field) && d.clip.expand(0.01).contains_rect(field)),
            "{theme:?}: the open field's frame is clipped or missing"
        );
        // Pick "Krill" (the second option: its row is the menu's second third).
        let pick = pos2(
            menu_rect.center().x,
            menu_rect.min.y + menu_rect.height() / 2.0,
        );
        h.click(pick, &mut draw);
        assert_eq!(value.get(), Some(1), "{theme:?}: picked");
        assert!(changed.get(), "{theme:?}: the response says changed");
        assert!(
            !egui::Popup::is_id_open(&h.ctx, menu_id),
            "{theme:?}: a pick closes the menu"
        );
    }
}

#[test]
fn a_disabled_dropdown_does_not_open() {
    let h = Ctx::new(Theme::Light);
    let field = Cell::new(Rect::NOTHING);
    let menu = Cell::new(None);
    let mut draw = |ui: &mut egui::Ui| {
        ui.add_enabled_ui(false, |ui| {
            menu.set(Some(Dropdown::menu_id(ui, "off")));
            field.set(
                Dropdown::new("off")
                    .selected_text("Krill")
                    .show_ui(ui, |ui| ui.label("never"))
                    .response
                    .rect,
            );
        });
    };
    h.frame(Vec::new(), &mut draw);
    h.click(field.get().center(), &mut draw);
    assert!(!egui::Popup::is_id_open(&h.ctx, menu.get().expect("drawn")));
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_page_builds_the_stock_combo_box() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut offenders = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("utf-8");
        for (i, line) in text.lines().enumerate() {
            if [
                "ComboBox::from_id_salt",
                "ComboBox::new",
                "ComboBox::from_label",
            ]
            .iter()
            .any(|c| line.contains(c))
            {
                offenders.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "use design::widgets::Dropdown (fixed height, never clipped) instead of egui's \
         ComboBox:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_selected_option_has_the_gray_selected_row_fill() {
    // Review ux-visual #16: the open menu marked the selected option with egui's
    // light-blue text-selection colour.
    use superba_gui::design::tokens::{DARK, LIGHT};
    for (theme, pal) in [(Theme::Light, &LIGHT), (Theme::Dark, &DARK)] {
        let h = Ctx::new(theme);
        let options = vec![
            (0u8, "All models".to_owned()),
            (1, "gpt-6-astra".to_owned()),
            (2, "gpt-5.5".to_owned()),
        ];
        let value = Cell::new(1u8);
        let field = Cell::new(Rect::NOTHING);
        let mut draw = |ui: &mut egui::Ui| {
            let area = Rect::from_min_size(pos2(40.0, 40.0), vec2(400.0, size::CONTROL_SM));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
            child.horizontal(|ui| {
                let mut v = value.get();
                let r = Dropdown::new("models")
                    .width(160.0)
                    .show_options(ui, &mut v, &options);
                value.set(v);
                field.set(r.rect);
            });
        };
        h.frame(Vec::new(), &mut draw);
        h.frame(Vec::new(), &mut draw);
        h.click(field.get().center(), &mut draw);
        // Move the pointer away so no row is hovered, then read the fills.
        h.frame(vec![Event::PointerMoved(pos2(600.0, 460.0))], &mut draw);
        let output = h.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN.into())),
                ..Default::default()
            },
            |ui| draw(ui),
        );
        let fills: Vec<egui::Color32> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Rect(r) if r.rect.min.y > field.get().max.y => Some(r.fill),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        // The menu may still be fading in: compare colours without their alpha.
        let rgb = |c: egui::Color32| {
            let [r, g, b, _] = c.to_srgba_unmultiplied();
            [r, g, b]
        };
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);
        let filled: Vec<[u8; 3]> = fills
            .iter()
            .filter(|c| c.a() > 0)
            .map(|c| rgb(*c))
            .collect();
        assert_eq!(filled.len(), 1, "{theme:?}: one filled row: {fills:?}");
        assert!(
            near(filled[0], rgb(pal.active)),
            "{theme:?}: the selected row has the gray selected-row fill: {fills:?}"
        );
        let blue = rgb(h.ctx.style_of(theme).visuals.selection.bg_fill);
        assert!(
            !near(filled[0], blue),
            "{theme:?}: not egui's selection blue"
        );
    }
}
