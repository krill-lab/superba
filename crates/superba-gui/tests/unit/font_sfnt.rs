use super::*;
use skrifa::{
    FontRef, GlyphId, MetadataProvider, Tag,
    instance::{LocationRef, Size},
};

#[test]
fn quadratic_font_roundtrips_through_the_renderer_parser() {
    let p = |x, y, on_curve| Point { x, y, on_curve };
    let glyph = Glyph {
        contours: vec![
            vec![
                p(-100, -60, true),
                p(400, 900, false),
                p(650, 700, true),
                p(600, 0, true),
            ],
            vec![p(0, 10, true), p(40, 10, true), p(40, 40, true)],
        ],
    };
    let mut glyf = Vec::new();
    glyph.encode(&mut glyf).unwrap();
    let source = FontRef::new(super::super::GEIST_REGULAR).unwrap();
    let mut tables = BTreeMap::new();
    for tag in [
        b"head", b"hhea", b"OS/2", b"name", b"cmap", b"post", b"maxp",
    ] {
        tables.insert(
            *tag,
            source
                .table_data(Tag::new(tag))
                .unwrap()
                .as_bytes()
                .to_vec(),
        );
    }
    put16(tables.get_mut(b"head").unwrap(), 50, 1);
    put16(tables.get_mut(b"hhea").unwrap(), 34, 2);
    let maxp = tables.get_mut(b"maxp").unwrap();
    put16(maxp, 4, 2);
    put16(maxp, 6, 7);
    put16(maxp, 8, 2);
    let mut loca = vec![0; 8]; // empty glyph 0; glyph 1 starts at 0
    loca.extend((glyf.len() as u32).to_be_bytes());
    tables.insert(*b"glyf", glyf);
    tables.insert(*b"loca", loca);
    tables.insert(*b"hmtx", vec![3, 232, 0, 0, 3, 232, 255, 156]);
    let bytes = assemble(tables).unwrap();
    assert_eq!(checksum(&bytes), 0xb1b0_afba);
    let font = FontRef::new(&bytes).unwrap();
    let metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    assert_eq!(metrics.advance_width(GlyphId::new(1)), Some(1000.0));
    let bounds = metrics.bounds(GlyphId::new(1)).unwrap();
    assert_eq!(
        [bounds.x_min, bounds.y_min, bounds.x_max, bounds.y_max],
        [-100.0, -60.0, 650.0, 900.0]
    );
    // Parsing the outline verifies flag/delta encoding, including negative and
    // short deltas, off-curve control points and multiple contours.
    let mut pen = RecordingPen::default();
    font.outline_glyphs()
        .get(GlyphId::new(1))
        .unwrap()
        .draw((Size::unscaled(), LocationRef::default()), &mut pen)
        .unwrap();
    assert_eq!(pen.moves, 2);
    assert_eq!(pen.quads, vec![(400.0, 900.0, 650.0, 700.0)]);
}

#[derive(Default)]
struct RecordingPen {
    moves: usize,
    quads: Vec<(f32, f32, f32, f32)>,
}
impl skrifa::outline::OutlinePen for RecordingPen {
    fn move_to(&mut self, _: f32, _: f32) {
        self.moves += 1;
    }
    fn line_to(&mut self, _: f32, _: f32) {}
    fn quad_to(&mut self, x: f32, y: f32, a: f32, b: f32) {
        self.quads.push((x, y, a, b));
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        panic!("unexpected cubic");
    }
    fn close(&mut self) {}
}

#[test]
fn encoder_rejects_unrepresentable_contours_and_missing_head() {
    assert!(
        Glyph {
            contours: vec![vec![]]
        }
        .encode(&mut Vec::new())
        .is_none()
    );
    let glyph = Glyph {
        contours: vec![vec![
            Point {
                x: i16::MIN,
                y: 0,
                on_curve: true,
            },
            Point {
                x: i16::MAX,
                y: 0,
                on_curve: true,
            },
        ]],
    };
    assert!(glyph.encode(&mut Vec::new()).is_none());
    assert!(assemble(BTreeMap::from([(*b"head", vec![0; 4])])).is_none());
    assert!(assemble(BTreeMap::new()).is_none());
}
