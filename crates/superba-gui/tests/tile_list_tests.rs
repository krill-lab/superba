#![allow(missing_docs)]
//! Tile mode of the paged table (card lists such as the provider list): the
//! rounded rows are separated by spacing, never by divider lines, while plain
//! table rows keep their dividers.

use egui::{Rect, Shape, Theme, UiBuilder, pos2, vec2};
use pretty_assertions::assert_eq;
use superba_gui::design::tokens::{DARK, size, space};
use superba_gui::design::widgets::{PagedState, PagedTable, Table};

const WIDTH: f32 = 260.0;
const ROWS: usize = 6;

/// What one frame of a list drew.
#[derive(Debug, Default)]
struct Drawn {
    per_page: usize,
    /// y of every divider line across the whole list.
    dividers: Vec<f32>,
    /// The selected row's fill.
    selected: Option<Rect>,
}

fn flatten(shape: &Shape, out: &mut Vec<Shape>) {
    match shape {
        Shape::Vec(v) => v.iter().for_each(|s| flatten(s, out)),
        s => out.push(s.clone()),
    }
}

/// Draw a list of [`ROWS`] two-line rows, row 1 selected, in a box with
/// `list_h` px for the rows (the pager footer comes on top).
fn draw(tiles: bool, list_h: f32) -> Drawn {
    let context = egui::Context::default();
    context.set_theme(Theme::Dark);
    let mut state = PagedState {
        selected: Some(1),
        ..PagedState::default()
    };
    let mut drawn = Drawn::default();
    let input = || egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
        ..egui::RawInput::default()
    };
    let mut add = |ui: &mut egui::Ui| {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(WIDTH, list_h + size::FOOTER));
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
        let mut table = PagedTable::new("tiles", Table::new(&[1.0]), ROWS, &mut state)
            .row_height(size::ROW_LIST2);
        if tiles {
            table = table.tiles(space::XS);
        }
        drawn.per_page = table.show(&mut child, |_, _, _| {}).per_page;
    };
    context
        .run_ui(input(), &mut add)
        .drop_without_applying_deltas();
    let mut out = context.run_ui(input(), &mut add);
    let clipped = std::mem::take(&mut out.shapes);
    out.drop_without_applying_deltas();
    let mut shapes = Vec::new();
    for c in &clipped {
        flatten(&c.shape, &mut shapes);
    }
    for shape in shapes {
        match shape {
            Shape::LineSegment { points, stroke }
                if points[0].y == points[1].y
                    && stroke.color == DARK.border
                    && (points[1].x - points[0].x).abs() >= WIDTH - 1.0 =>
            {
                drawn.dividers.push(points[0].y);
            }
            Shape::Rect(r) if r.fill == DARK.active && r.rect.width() == WIDTH => {
                drawn.selected = Some(r.rect);
            }
            _ => {}
        }
    }
    drawn
}

#[test]
fn tiles_are_separated_by_spacing_not_divider_lines() {
    // 3 tiles take 3 x 52 + 2 x 4 = 164 px: they fit exactly.
    let tiles = draw(true, 164.0);
    assert_eq!(tiles.per_page, 3);
    assert_eq!(tiles.dividers, Vec::<f32>::new(), "no line between tiles");
    let pitch = size::ROW_LIST2 + space::XS;
    let sel = tiles.selected.expect("the selected tile is filled");
    assert_eq!((sel.min.y, sel.height()), (pitch, size::ROW_LIST2));
    // 4 px less: the gaps count, only 2 tiles fit.
    assert_eq!(draw(true, 160.0).per_page, 2);
}

#[test]
fn plain_table_rows_keep_their_dividers() {
    let plain = draw(false, 160.0);
    assert_eq!(plain.per_page, 3);
    let h = size::ROW_LIST2;
    assert_eq!(plain.dividers, vec![h - 0.5, 2.0 * h - 0.5]);
    let sel = plain.selected.expect("the selected row is filled");
    assert_eq!((sel.min.y, sel.height()), (h, h));
}
