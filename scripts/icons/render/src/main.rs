//! Renders the icon masters into every committed raster icon: the PNGs and the
//! .ico from assets/icon/superba.svg; the .icns and superba-macos-256.png from
//! assets/icon/superba-macos.svg (the same artwork on Apple's macOS grid).
//! Usage: superba-icon-render <svg> <macos-svg> <png-dir> <ico-out> <icns-out>

use std::{env, fs, path::Path};

use resvg::{tiny_skia, usvg};

const PNG_SIZES: [u32; 8] = [16, 32, 48, 64, 128, 256, 512, 1024];
const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];
/// How an .icns entry stores its pixels.
#[derive(Clone, Copy)]
enum Icns {
    Png,
    /// `ARGB` + Apple's RLE of the alpha and premultiplied red, green and blue
    /// planes. macOS reads the 16 and 32 px entries only this way: PNG data in
    /// `icp4`/`icp5` comes out as noise (iconutil and NSWorkspace, macOS 14 to 26).
    Argb,
}

/// (pixel size, icns type, encoding): the entries `iconutil` writes for a full
/// .iconset, 16 to 512 px plus @2x (64 px is `ic12`, 32@2x).
const ICNS_TYPES: [(u32, &[u8; 4], Icns); 10] = [
    (16, b"ic04", Icns::Argb),
    (32, b"ic11", Icns::Png),
    (32, b"ic05", Icns::Argb),
    (64, b"ic12", Icns::Png),
    (128, b"ic07", Icns::Png),
    (256, b"ic13", Icns::Png),
    (256, b"ic08", Icns::Png),
    (512, b"ic14", Icns::Png),
    (512, b"ic09", Icns::Png),
    (1024, b"ic10", Icns::Png),
];

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

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let [svg, macos_svg, png_dir, ico_out, icns_out] = args.as_slice() else {
        eprintln!("usage: superba-icon-render <svg> <macos-svg> <png-dir> <ico-out> <icns-out>");
        std::process::exit(2);
    };
    let tree = load(svg);
    let macos_tree = load(macos_svg);

    fs::create_dir_all(png_dir).expect("png dir");
    for px in PNG_SIZES {
        let path = Path::new(png_dir).join(format!("superba-{px}.png"));
        render(&tree, px).save_png(path).expect("write png");
    }
    // macOS: the Dock icon that a bare (unbundled) superba binary sets at runtime.
    let dock = png(&rgba(&render(&macos_tree, 256)), 256);
    fs::write(Path::new(png_dir).join("superba-macos-256.png"), dock).expect("write png");

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for px in ICO_SIZES {
        let image = ico::IconImage::from_rgba_data(px, px, rgba(&render(&tree, px)));
        // BMP below 64 px (every resource compiler reads it), PNG above.
        let entry = if px < 64 {
            ico::IconDirEntry::encode_as_bmp(&image)
        } else {
            ico::IconDirEntry::encode_as_png(&image)
        };
        dir.add_entry(entry.expect("ico entry"));
    }
    dir.write(fs::File::create(ico_out).expect("ico file"))
        .expect("write ico");

    // macOS draws app icons on its own grid (824 px tile in 1024): the .icns
    // comes from the macOS master so Superba is as big as other apps there.
    let mut icns = Vec::new();
    for (px, ty, encoding) in ICNS_TYPES {
        let pixmap = render(&macos_tree, px);
        let data = match encoding {
            Icns::Png => png(&rgba(&pixmap), px),
            Icns::Argb => argb(pixmap.data()),
        };
        icns.extend_from_slice(ty);
        icns.extend_from_slice(&icns_len(8 + data.len()));
        icns.extend_from_slice(&data);
    }
    let mut file = b"icns".to_vec();
    file.extend_from_slice(&icns_len(8 + icns.len()));
    file.extend_from_slice(&icns);
    fs::write(icns_out, file).expect("write icns");
}

/// A PNG of straight RGBA pixels, compressed harder than `Pixmap::save_png`.
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

fn icns_len(len: usize) -> [u8; 4] {
    u32::try_from(len).expect("icns size").to_be_bytes()
}

/// An `ic04`/`ic05` payload: `ARGB`, then each plane (alpha, red, green, blue)
/// run-length encoded on its own. macOS reads these colours as premultiplied by
/// alpha (as tiny-skia keeps them), unlike PNG entries.
fn argb(premultiplied_rgba: &[u8]) -> Vec<u8> {
    let mut out = b"ARGB".to_vec();
    for channel in [3, 0, 1, 2] {
        let plane: Vec<u8> = premultiplied_rgba
            .iter()
            .skip(channel)
            .step_by(4)
            .copied()
            .collect();
        rle(&plane, &mut out);
    }
    out
}

/// Apple's icns RLE: a control byte below 0x80 is followed by that many plus one
/// literal bytes; from 0x80 up it repeats the next byte (control - 0x80 + 3) times.
fn rle(plane: &[u8], out: &mut Vec<u8>) {
    fn flush(literal: &mut Vec<u8>, out: &mut Vec<u8>) {
        if !literal.is_empty() {
            out.push((literal.len() - 1) as u8);
            out.append(literal);
        }
    }
    let mut literal = Vec::new();
    let mut i = 0;
    while i < plane.len() {
        let run = plane[i..]
            .iter()
            .take(130)
            .take_while(|&&b| b == plane[i])
            .count();
        if run >= 3 {
            flush(&mut literal, out);
            out.extend_from_slice(&[0x80 + (run - 3) as u8, plane[i]]);
            i += run;
        } else {
            literal.push(plane[i]);
            i += 1;
            if literal.len() == 128 {
                flush(&mut literal, out);
            }
        }
    }
    flush(&mut literal, out);
}
