//! Renders the icon masters into every committed raster icon: the PNGs, the
//! .ico and the Windows installer's two bitmaps from assets/icon/superba.svg;
//! the .icns and superba-macos-256.png from assets/icon/superba-macos.svg (the
//! same artwork on Apple's macOS grid).
//! Usage: superba-icon-render <svg> <macos-svg> <png-dir> <ico-out> <icns-out>
//!        <msi-dialog-bmp> <msi-banner-bmp>

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
    let [
        svg,
        macos_svg,
        png_dir,
        ico_out,
        icns_out,
        msi_dialog,
        msi_banner,
    ] = args.as_slice()
    else {
        eprintln!(
            "usage: superba-icon-render <svg> <macos-svg> <png-dir> <ico-out> <icns-out> \
             <msi-dialog-bmp> <msi-banner-bmp>"
        );
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

    fs::write(msi_dialog, bmp24(&msi_dialog_bitmap(&tree))).expect("write dialog bmp");
    fs::write(msi_banner, bmp24(&msi_banner_bitmap(&tree))).expect("write banner bmp");
}

/// The icon's pearl tile gradient (#FFFFFF, #F4F6FA, #DFE6F0) and the krill's
/// coral one (#FF9A65, #FF6277, #EB327F), as (offset, rgb) stops.
const PEARL: [(f32, [u8; 3]); 3] = [
    (0.0, [0xFF, 0xFF, 0xFF]),
    (0.5, [0xF4, 0xF6, 0xFA]),
    (1.0, [0xDF, 0xE6, 0xF0]),
];
const CORAL: [(f32, [u8; 3]); 3] = [
    (0.0, [0xFF, 0x9A, 0x65]),
    (0.5, [0xFF, 0x62, 0x77]),
    (1.0, [0xEB, 0x32, 0x7F]),
];

fn gradient(stops: &[(f32, [u8; 3])], t: f32) -> tiny_skia::PremultipliedColorU8 {
    let mut rgb = stops[stops.len() - 1].1;
    for pair in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            let f = (t - t0) / (t1 - t0);
            for i in 0..3 {
                rgb[i] =
                    (f32::from(c0[i]) + (f32::from(c1[i]) - f32::from(c0[i])) * f).round() as u8;
            }
            break;
        }
    }
    tiny_skia::PremultipliedColorU8::from_rgba(rgb[0], rgb[1], rgb[2], 255).expect("opaque")
}

/// A white canvas with `icon` drawn on it at (x, y), `px` pixels square.
fn canvas_with_icon(
    tree: &usvg::Tree,
    (w, h): (u32, u32),
    fill: impl Fn(u32, u32) -> tiny_skia::PremultipliedColorU8,
    (x, y, px): (i32, i32, u32),
) -> tiny_skia::Pixmap {
    let mut canvas = tiny_skia::Pixmap::new(w, h).expect("pixmap");
    for (i, p) in canvas.pixels_mut().iter_mut().enumerate() {
        let i = u32::try_from(i).expect("pixel index");
        *p = fill(i % w, i / w);
    }
    let icon = render(tree, px);
    canvas.draw_pixmap(
        x,
        y,
        icon.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        None,
    );
    canvas
}

/// WixUIDialogBmp (493 x 312, the welcome and finish pages): the installer draws
/// its text over the white part from x = 180, so the icon sits on a pearl panel
/// on the left, edged in coral.
fn msi_dialog_bitmap(tree: &usvg::Tree) -> tiny_skia::Pixmap {
    const W: u32 = 493;
    const H: u32 = 312;
    const PANEL: u32 = 164;
    const ICON: u32 = 120;
    let fill = |x: u32, y: u32| {
        let t = y as f32 / (H - 1) as f32;
        if x < PANEL {
            gradient(&PEARL, t)
        } else if x < PANEL + 2 {
            gradient(&CORAL, t)
        } else {
            gradient(&PEARL, 0.0)
        }
    };
    let x = i32::try_from((PANEL - ICON) / 2).expect("x");
    canvas_with_icon(tree, (W, H), fill, (x, 84, ICON))
}

/// WixUIBannerBmp (493 x 58, the top of the other pages): white, the page title
/// on the left, the icon at the right end.
fn msi_banner_bitmap(tree: &usvg::Tree) -> tiny_skia::Pixmap {
    const W: u32 = 493;
    const H: u32 = 58;
    const ICON: u32 = 44;
    let x = i32::try_from(W - ICON - 9).expect("x");
    let y = i32::try_from((H - ICON) / 2).expect("y");
    canvas_with_icon(tree, (W, H), |_, _| gradient(&PEARL, 0.0), (x, y, ICON))
}

/// An uncompressed 24-bit BMP (BITMAPINFOHEADER, bottom-up rows padded to 4
/// bytes) of an opaque pixmap: the format Windows Installer's Bitmap control
/// shows everywhere.
fn bmp24(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    let row = (w * 3).div_ceil(4) * 4;
    let image = row * h;
    let le32 = |v: usize| u32::try_from(v).expect("bmp size").to_le_bytes();
    let mut out = Vec::with_capacity(54 + image);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&le32(54 + image));
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&le32(54));
    out.extend_from_slice(&le32(40));
    out.extend_from_slice(&le32(w));
    out.extend_from_slice(&le32(h));
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&le32(0)); // BI_RGB
    out.extend_from_slice(&le32(image));
    out.extend_from_slice(&le32(2835)); // 72 dpi
    out.extend_from_slice(&le32(2835));
    out.extend_from_slice(&le32(0));
    out.extend_from_slice(&le32(0));
    let pixels = pixmap.pixels();
    for y in (0..h).rev() {
        let start = out.len();
        for p in &pixels[y * w..(y + 1) * w] {
            assert_eq!(p.alpha(), 255, "the installer bitmaps are opaque");
            out.extend_from_slice(&[p.blue(), p.green(), p.red()]);
        }
        out.resize(start + row, 0);
    }
    out
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
