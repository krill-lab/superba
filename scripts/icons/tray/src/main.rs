//! Renders the tray icon masters into the committed rasters (item 70):
//! - assets/icon/superba-tray.svg (the krill without its tile) ->
//!   assets/icon/tray/superba-tray-{16,20,22,24,32,40,48,64}.png (Linux status
//!   icons; Windows when the exe has no icon resource) and
//!   assets/icon/tray/superba-tray.ico (16/20/24/32/40/48/64: the small-icon
//!   size of the notification area at 100 % to 400 %, embedded in superba.exe);
//! - assets/icon/superba-tray-template.svg (black on transparency) ->
//!   assets/icon/tray/superba-tray-template-{18,36}.png (the macOS menu-bar
//!   template image, 18 pt at 1x and 2x).
//!
//! Usage: superba-tray-icon-render <tray-svg> <template-svg> <out-dir>

use std::{env, fs, path::Path};

use resvg::{tiny_skia, usvg};

const PNG_SIZES: [u32; 8] = [16, 20, 22, 24, 32, 40, 48, 64];
const ICO_SIZES: [u32; 7] = [16, 20, 24, 32, 40, 48, 64];
const TEMPLATE_SIZES: [u32; 2] = [18, 36];

fn load(path: &str) -> usvg::Tree {
    let data = fs::read(path).expect("read svg");
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("parse svg");
    assert_eq!(
        tree.size().width(),
        1024.0,
        "{path}: a master SVG is 1024x1024"
    );
    tree
}

fn render(tree: &usvg::Tree, px: u32) -> tiny_skia::Pixmap {
    let mut pixmap = tiny_skia::Pixmap::new(px, px).expect("pixmap");
    let scale = px as f32 / tree.size().width();
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
}

fn rgba(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// A PNG of straight RGBA pixels.
fn png(rgba: &[u8], px: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, px, px);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Best);
    encoder.set_adaptive_filter(png::AdaptiveFilterType::Adaptive);
    let mut writer = encoder.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png data");
    writer.finish().expect("png end");
    out
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let [svg, template_svg, out] = args.as_slice() else {
        eprintln!("usage: superba-tray-icon-render <tray-svg> <template-svg> <out-dir>");
        std::process::exit(2);
    };
    let tree = load(svg);
    let template = load(template_svg);
    let out = Path::new(out);
    fs::create_dir_all(out).expect("out dir");
    for px in PNG_SIZES {
        let data = png(&rgba(&render(&tree, px)), px);
        fs::write(out.join(format!("superba-tray-{px}.png")), data).expect("write png");
    }
    for px in TEMPLATE_SIZES {
        let data = png(&rgba(&render(&template, px)), px);
        fs::write(out.join(format!("superba-tray-template-{px}.png")), data).expect("write png");
    }
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for px in ICO_SIZES {
        let image = ico::IconImage::from_rgba_data(px, px, rgba(&render(&tree, px)));
        // BMP entries: every resource compiler and LoadImage read them.
        dir.add_entry(ico::IconDirEntry::encode_as_bmp(&image).expect("ico entry"));
    }
    dir.write(fs::File::create(out.join("superba-tray.ico")).expect("ico file"))
        .expect("write ico");
}
