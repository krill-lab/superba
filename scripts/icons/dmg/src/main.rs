//! Renders the macOS disk image's window background (user bug 84):
//! packaging/macos/dmg-background.svg (640 x 400 pt, the DMG window) ->
//!   packaging/macos/dmg-background.png    (640 x 400 px, 72 dpi)
//!   packaging/macos/dmg-background@2x.png (1280 x 800 px, 144 dpi)
//! dmgbuild joins the two into one multi-resolution TIFF (`tiffutil
//! -cathidpicheck`), from which Finder draws the 2x image on Retina displays.
//!
//! Text is shaped only with the font files named on the command line (there is
//! no system font lookup), and only the families the SVG uses are kept, so the
//! pixels depend on the SVG and those files alone.
//!
//! Usage: superba-dmg-background-render <svg> <out-dir> <font-file>...

use std::{env, fs, path::Path};

use resvg::{tiny_skia, usvg};

/// The SVG's font families: Geist (the app's typeface) and, for the Chinese
/// hint, Noto Sans CJK SC. A font file may hold more faces (the Noto CJK
/// collection also has the JP, KR, TC and HK ones); the others are dropped so a
/// fallback can never pick them.
const FAMILIES: [&str; 2] = ["Geist", "Noto Sans CJK SC"];
const SIZE: (u32, u32) = (640, 400);

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let [svg, out, fonts @ ..] = args.as_slice() else {
        usage();
    };
    if fonts.is_empty() {
        usage();
    }

    let mut options = usvg::Options {
        font_family: FAMILIES[0].to_owned(),
        ..usvg::Options::default()
    };
    let db = options.fontdb_mut();
    for font in fonts {
        db.load_font_data(fs::read(font).unwrap_or_else(|e| panic!("read {font}: {e}")));
    }
    let wanted = |face: &usvg::fontdb::FaceInfo| {
        face.families
            .iter()
            .any(|(name, _)| FAMILIES.contains(&name.as_str()))
    };
    let unwanted: Vec<_> = db.faces().filter(|f| !wanted(f)).map(|f| f.id).collect();
    for id in unwanted {
        db.remove_face(id);
    }
    for family in FAMILIES {
        assert!(
            db.faces()
                .any(|f| f.families.iter().any(|(name, _)| name == family)),
            "no font file given for the family {family}"
        );
    }

    let data = fs::read(svg).unwrap_or_else(|e| panic!("read {svg}: {e}"));
    let tree = usvg::Tree::from_data(&data, &options).expect("parse svg");
    assert_eq!(
        (tree.size().width(), tree.size().height()),
        (SIZE.0 as f32, SIZE.1 as f32),
        "{svg}: the background is {} x {} pt",
        SIZE.0,
        SIZE.1
    );

    let out = Path::new(out);
    fs::create_dir_all(out).expect("out dir");
    for (scale, name) in [(1, "dmg-background.png"), (2, "dmg-background@2x.png")] {
        let (w, h) = (SIZE.0 * scale, SIZE.1 * scale);
        let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("pixmap");
        let s = scale as f32;
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(s, s),
            &mut pixmap.as_mut(),
        );
        fs::write(out.join(name), png_rgb(&pixmap, 72 * scale)).expect("write png");
    }
}

fn usage() -> ! {
    eprintln!("usage: superba-dmg-background-render <svg> <out-dir> <font-file>...");
    std::process::exit(2);
}

/// An opaque RGB PNG with its resolution (pHYs) set: 72 dpi for 1x, 144 for 2x.
fn png_rgb(pixmap: &tiny_skia::Pixmap, dpi: u32) -> Vec<u8> {
    let rgb: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            assert_eq!(p.alpha(), 255, "the background is opaque");
            [p.red(), p.green(), p.blue()]
        })
        .collect();
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, pixmap.width(), pixmap.height());
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Best);
    encoder.set_adaptive_filter(png::AdaptiveFilterType::Adaptive);
    // Pixels per metre: 72 dpi = 2835, 144 dpi = 5669.
    let ppm = (f64::from(dpi) / 0.0254).round() as u32;
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: ppm,
        yppu: ppm,
        unit: png::Unit::Meter,
    }));
    let mut writer = encoder.write_header().expect("png header");
    writer.write_image_data(&rgb).expect("png data");
    writer.finish().expect("png end");
    out
}
