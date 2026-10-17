//! Keyboard focus rings (user bug: the Providers tabs' ring was cut at the pane's
//! left edge, and a mouse click left a heavy blue box): rings show only while the
//! keyboard is in use, are never clipped by a pane or clip edge, are painted
//! above later siblings, and every focusable widget gets one.

#![allow(missing_docs)]

use std::cell::Cell;

use egui::{Event, Key, PointerButton, Pos2, Rect, Sense, Shape, Theme, pos2, vec2};
use superba_gui::design::layout;
use superba_gui::design::tokens::{DARK, Palette, focus};
use superba_gui::design::widgets::focus::{RingPlacement, fallback_ring, ring_placement};
use superba_gui::design::widgets::{Button, Segmented};
use superba_gui::design::{fonts, style};

/// A focus ring found in a frame's output: the stroke's centre rectangle and
/// the clip rectangle it was painted with.
#[derive(Clone, Copy, Debug)]
struct Ring {
    rect: Rect,
    clip: Rect,
    index: usize,
}

impl Ring {
    /// Everything the 2 px stroke covers.
    fn painted(&self) -> Rect {
        self.rect.expand(focus::WIDTH / 2.0)
    }
}

struct Frame {
    rings: Vec<Ring>,
    /// Index of every shape filled with `MARK` (a sibling painted after a widget).
    marks: Vec<usize>,
}

const MARK: egui::Color32 = egui::Color32::from_rgb(1, 2, 3);

struct Harness {
    ctx: egui::Context,
    pal: &'static Palette,
}

impl Harness {
    fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
        style::install(&ctx, Some(Theme::Dark));
        Self { ctx, pal: &DARK }
    }

    fn frame(&self, events: Vec<Event>, f: &mut dyn FnMut(&mut egui::Ui)) -> Frame {
        let out = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                events,
                ..Default::default()
            },
            |ui| f(ui),
        );
        let mut rings = Vec::new();
        let mut marks = Vec::new();
        for (index, clipped) in out.shapes.iter().enumerate() {
            if let Shape::Rect(r) = &clipped.shape {
                if r.stroke.color == self.pal.focus && r.stroke.width == focus::WIDTH {
                    rings.push(Ring {
                        rect: r.rect,
                        clip: clipped.clip_rect,
                        index,
                    });
                }
                if r.fill == MARK {
                    marks.push(index);
                }
            }
        }
        out.drop_without_applying_deltas();
        Frame { rings, marks }
    }

    fn key(&self, key: Key, f: &mut dyn FnMut(&mut egui::Ui)) -> Frame {
        self.frame(
            vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            f,
        )
    }

    /// Press in one frame, release in the next, then an idle frame.
    fn click(&self, pos: Pos2, f: &mut dyn FnMut(&mut egui::Ui)) -> Frame {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(vec![Event::PointerMoved(pos), button(true)], f);
        self.frame(vec![button(false)], f);
        self.frame(Vec::new(), f)
    }

    fn focused(&self) -> Option<egui::Id> {
        self.ctx.memory(|m| m.focused())
    }
}

/// A pane of a Fit page whose left edge is where the Providers detail starts.
const PANE: Rect = Rect {
    min: pos2(300.0, 100.0),
    max: pos2(700.0, 400.0),
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tab {
    Connection,
    Account,
}

/// The Providers detail's tab strip: a small segmented control flush with the
/// pane's left and top edges. Returns the track's rectangle.
fn strip(ui: &mut egui::Ui, tab: &Cell<Tab>) -> Rect {
    let mut value = tab.get();
    let rect = layout::fixed(ui, PANE, |ui| {
        Segmented::new("focus-test-tabs", &mut value)
            .option(Tab::Connection, "Connection")
            .option(Tab::Account, "Account & usage")
            .small()
            .show(ui)
            .rect
    });
    tab.set(value);
    rect
}

#[test]
fn ring_goes_outside_when_the_clip_has_room_and_inside_otherwise() {
    let widget = Rect::from_min_size(pos2(100.0, 100.0), vec2(80.0, 28.0));
    let roomy = widget.expand(20.0);
    match ring_placement(widget, roomy) {
        RingPlacement::Outside(r) => assert_eq!(r, widget.expand(3.0)),
        other => panic!("expected an outside ring, got {other:?}"),
    }
    // Exactly the bleed the layout panes give: still outside.
    let bleed = widget.expand(focus::BLEED);
    assert!(matches!(
        ring_placement(widget, bleed),
        RingPlacement::Outside(_)
    ));
    // Flush with the clip's left edge (the reported bug): inside, and painted
    // entirely within the clip.
    let flush = Rect::from_min_max(pos2(100.0, 50.0), pos2(400.0, 300.0));
    let ring = ring_placement(widget, flush);
    assert!(matches!(ring, RingPlacement::Inside(_)), "{ring:?}");
    assert!(flush.contains_rect(ring.painted()));
    assert!(widget.contains_rect(ring.painted()));
}

#[test]
fn layout_panes_leave_room_for_the_ring_of_a_control_on_their_edge() {
    let parent = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0));
    assert_eq!(layout::pane_clip(PANE, parent), PANE.expand(focus::BLEED));
    // Never wider than the parent's clip.
    let tight = PANE.expand(1.0);
    assert_eq!(layout::pane_clip(PANE, tight), tight);
    let control = Rect::from_min_size(PANE.min, vec2(120.0, 28.0));
    assert!(matches!(
        ring_placement(control, layout::pane_clip(PANE, parent)),
        RingPlacement::Outside(_)
    ));
}

#[test]
fn tab_strip_flush_with_a_pane_edge_shows_its_whole_ring_on_keyboard_focus() {
    let h = Harness::new();
    let tab = Cell::new(Tab::Connection);
    let track = Cell::new(Rect::NOTHING);
    let mut ui = |ui: &mut egui::Ui| track.set(strip(ui, &tab));
    let idle = h.frame(Vec::new(), &mut ui);
    assert!(idle.rings.is_empty(), "no ring without focus");
    let track = track.get();
    assert_eq!(track.min, PANE.min, "the strip sits on the pane's corner");

    let tabbed = h.key(Key::Tab, &mut ui);
    assert!(h.focused().is_some(), "Tab focuses the strip");
    assert_eq!(tabbed.rings.len(), 1, "{:?}", tabbed.rings);
    let ring = tabbed.rings[0];
    assert_eq!(ring.rect, track.expand(3.0), "the 2 px gap ring outside");
    assert!(
        ring.clip.contains_rect(ring.painted()),
        "ring {:?} is cut by its clip {:?}",
        ring.painted(),
        ring.clip
    );
    assert!(
        ring.painted().min.x < PANE.min.x,
        "it reaches past the pane edge"
    );
}

#[test]
fn a_click_selects_and_focuses_a_tab_without_a_ring_and_arrows_show_it() {
    let h = Harness::new();
    let tab = Cell::new(Tab::Connection);
    let track = Cell::new(Rect::NOTHING);
    let mut ui = |ui: &mut egui::Ui| track.set(strip(ui, &tab));
    h.frame(Vec::new(), &mut ui);
    h.frame(Vec::new(), &mut ui);
    // The second segment sits in the right part of the track.
    let t = track.get();
    let target = pos2(t.max.x - 30.0, t.center().y);
    let clicked = h.click(target, &mut ui);
    let focused = h.focused();
    assert_eq!(tab.get(), Tab::Account, "the click selects the segment");
    assert!(
        focused.is_some(),
        "the click focuses the track (arrow keys work)"
    );
    assert!(
        clicked.rings.is_empty(),
        "no focus ring after a mouse click"
    );

    let arrowed = h.key(Key::ArrowLeft, &mut ui);
    assert_eq!(h.focused(), focused, "the arrow does not move focus away");
    assert_eq!(arrowed.rings.len(), 1, "the keyboard shows the ring");
    assert_eq!(
        tab.get(),
        Tab::Connection,
        "the arrow moved the selection back"
    );
}

#[test]
fn segments_are_not_tab_stops_of_their_own() {
    let h = Harness::new();
    let tab = Cell::new(Tab::Connection);
    let mut ui = |ui: &mut egui::Ui| {
        strip(ui, &tab);
    };
    h.frame(Vec::new(), &mut ui);
    h.key(Key::Tab, &mut ui);
    let first = h.focused();
    assert!(first.is_some());
    // The track is the only stop: Tab leaves the control (nothing else here).
    h.key(Key::Tab, &mut ui);
    let after = h.focused();
    assert!(after.is_none() || after == first, "focus went to a segment");
}

#[test]
fn ring_is_painted_above_siblings_drawn_after_the_widget() {
    let h = Harness::new();
    let tab = Cell::new(Tab::Connection);
    let mut ui = |ui: &mut egui::Ui| {
        let track = strip(ui, &tab);
        // A neighbour painted later over the ring's bottom edge.
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(PANE.min.x, track.max.y + 1.0), vec2(200.0, 10.0)),
            0,
            MARK,
        );
    };
    h.frame(Vec::new(), &mut ui);
    let f = h.key(Key::Tab, &mut ui);
    assert_eq!(f.rings.len(), 1);
    assert!(!f.marks.is_empty());
    assert!(
        f.marks.iter().all(|&m| m < f.rings[0].index),
        "the ring must come after the sibling"
    );
}

#[test]
fn a_focusable_widget_without_its_own_ring_gets_one_from_the_plugin() {
    let h = Harness::new();
    let row = Rect::from_min_size(PANE.min, vec2(260.0, 36.0));
    let mut ui = |ui: &mut egui::Ui| {
        layout::fixed(ui, PANE, |ui| {
            ui.interact(row, egui::Id::unique("focus-test-row"), Sense::click());
        });
    };
    h.frame(Vec::new(), &mut ui);
    let clicked = h.click(row.center(), &mut ui);
    assert!(clicked.rings.is_empty());
    let f = h.key(Key::Tab, &mut ui);
    assert!(h.focused().is_some());
    assert_eq!(f.rings.len(), 1, "{:?}", f.rings);
    let ring = f.rings[0];
    assert!(
        row.contains_rect(ring.painted()),
        "a row's ring stays inside it"
    );
    assert!(ring.clip.contains_rect(ring.painted()));
}

#[test]
fn fallback_ring_is_outside_small_widgets_and_inside_rows() {
    let link = Rect::from_min_size(pos2(10.0, 10.0), vec2(120.0, 17.0));
    assert_eq!(
        fallback_ring(link, link),
        Some(RingPlacement::Outside(link.expand(3.0)))
    );
    let row = Rect::from_min_size(pos2(10.0, 10.0), vec2(260.0, 36.0));
    assert_eq!(
        fallback_ring(row, row),
        Some(RingPlacement::Inside(row.shrink(1.0)))
    );
    // A small widget half cut by its clip: inside its visible part.
    let visible = Rect::from_min_max(link.min, pos2(60.0, link.max.y));
    assert_eq!(
        fallback_ring(link, visible),
        Some(RingPlacement::Inside(visible.shrink(1.0)))
    );
    assert_eq!(fallback_ring(link, Rect::NOTHING), None);
}

#[test]
fn tab_scrolls_a_button_below_the_fold_into_view() {
    let h = Harness::new();
    let offset = Cell::new(0.0);
    let viewport = Cell::new(Rect::NOTHING);
    let mut ui = |ui: &mut egui::Ui| {
        let out = egui::ScrollArea::vertical()
            .id_salt("focus-test-scroll")
            .max_height(100.0)
            .animated(false)
            .show(ui, |ui| {
                for i in 0..10 {
                    ui.add(Button::new(format!("Button {i}")));
                }
            });
        offset.set(out.state.clamped_offset().y);
        viewport.set(out.inner_rect);
    };
    h.frame(Vec::new(), &mut ui);
    for _ in 0..6 {
        h.key(Key::Tab, &mut ui);
    }
    let f = h.frame(Vec::new(), &mut ui);
    assert!(
        offset.get() > 0.0,
        "the sixth button was scrolled into view"
    );
    assert_eq!(f.rings.len(), 1, "{:?}", f.rings);
    assert!(
        viewport.get().contains_rect(f.rings[0].rect),
        "ring {:?} outside the viewport {:?}",
        f.rings[0].rect,
        viewport.get()
    );
}

#[test]
fn a_clip_edge_off_by_layout_rounding_keeps_the_outside_ring() {
    // User bug 25 / e923162: egui rounds layout to 1/32 pt, so a widget meant to
    // be flush with its visible part may stick out by that much; it keeps the
    // outside ring (64f1a39 set the tolerance to egui::emath::GUI_ROUNDING).
    let link = Rect::from_min_size(pos2(10.0, 10.0), vec2(120.0, 17.0));
    let rounded = Rect::from_min_max(
        link.min,
        pos2(link.max.x - egui::emath::GUI_ROUNDING, link.max.y),
    );
    assert_eq!(
        fallback_ring(link, rounded),
        Some(RingPlacement::Outside(link.expand(3.0)))
    );
    let room = link.expand(focus::BLEED);
    let tight = Rect::from_min_max(
        room.min,
        pos2(room.max.x - egui::emath::GUI_ROUNDING, room.max.y),
    );
    assert!(matches!(
        ring_placement(link, tight),
        RingPlacement::Outside(_)
    ));
    // Anything more is a real cut: inside.
    let cut = Rect::from_min_max(link.min, pos2(link.max.x - 0.5, link.max.y));
    assert!(matches!(
        fallback_ring(link, cut),
        Some(RingPlacement::Inside(_))
    ));
}

#[test]
fn a_paged_tables_ring_frames_its_rows_not_the_header_or_the_pager() {
    // User bug 25: the focus ring of a whole table also framed its pager.
    use superba_gui::design::tokens::size;
    use superba_gui::design::widgets::{PagedState, PagedTable, Table};
    let h = Harness::new();
    let mut paging = PagedState::default();
    let mut ui = |ui: &mut egui::Ui| {
        layout::fixed(ui, PANE, |ui| {
            PagedTable::new("focus-test-paged", Table::new(&[1.0]), 40, &mut paging)
                .header(vec!["Name".into()])
                .show(ui, |ui, row, _| {
                    ui.label(format!("row {row}"));
                });
        });
    };
    h.frame(Vec::new(), &mut ui);
    let first_row = pos2(
        PANE.center().x,
        PANE.min.y + size::TABLE_HEADER + size::ROW / 2.0,
    );
    assert!(h.click(first_row, &mut ui).rings.is_empty());
    let f = h.key(Key::ArrowDown, &mut ui);
    assert_eq!(f.rings.len(), 1, "{:?}", f.rings);
    let ring = f.rings[0].painted();
    let rows = Rect::from_min_max(
        pos2(PANE.min.x, PANE.min.y + size::TABLE_HEADER),
        pos2(PANE.max.x, PANE.max.y - size::FOOTER),
    );
    assert!(
        rows.contains_rect(ring),
        "{ring:?} outside the rows {rows:?}"
    );
}

#[test]
fn a_log_tables_ring_frames_its_rows_not_the_header_or_the_totals() {
    use superba_gui::design::tokens::size;
    use superba_gui::design::widgets::LogTable;
    use superba_gui::design::widgets::log_table::{LogCell, LogColumn, ROW_HEIGHT};
    let h = Harness::new();
    let columns = vec![LogColumn::left("Name", 100.0), LogColumn::right("n", 60.0)];
    let selected = Cell::new(None);
    let mut ui = |ui: &mut egui::Ui| {
        layout::fixed(ui, PANE, |ui| {
            let r = LogTable::new("focus-test-log", &columns, 40)
                .selected(selected.get())
                .totals(vec![LogCell::text("Total"), LogCell::text("40")])
                .show(ui, |row, _| LogCell::text(format!("row {row}")));
            if let Some(row) = r.clicked.or(r.key_selected) {
                selected.set(Some(row));
            }
        });
    };
    h.frame(Vec::new(), &mut ui);
    let first_row = pos2(
        PANE.center().x,
        PANE.min.y + size::TABLE_HEADER + ROW_HEIGHT / 2.0,
    );
    assert!(h.click(first_row, &mut ui).rings.is_empty());
    let f = h.key(Key::ArrowDown, &mut ui);
    assert_eq!(selected.get(), Some(1));
    assert_eq!(f.rings.len(), 1, "{:?}", f.rings);
    let ring = f.rings[0].painted();
    let rows = Rect::from_min_max(
        pos2(PANE.min.x, PANE.min.y + size::TABLE_HEADER),
        pos2(PANE.max.x, PANE.max.y - ROW_HEIGHT),
    );
    assert!(
        rows.contains_rect(ring),
        "{ring:?} outside the rows {rows:?}"
    );
}
