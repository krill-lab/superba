//! Renders assets/icon/superba.svg into every committed raster icon.
//! Usage: superba-icon-render <svg> <png-dir> <ico-out> <icns-out>

use std::{env, fs, path::Path};

use resvg::{tiny_skia, usvg};

const PNG_SIZES: [u32; 8] = [16, 32, 48, 64, 128, 256, 512, 1024];
const ICO_SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];
/// (pixel size, icns type): the sizes macOS asks for, including @2x.
const ICNS_TYPES: [(u32, icns::IconType); 10] = [
    (16, icns::IconType::RGBA32_16x16),
    (32, icns::IconType::RGBA32_16x16_2x),
    (32, icns::IconType::RGBA32_32x32),
    (64, icns::IconType::RGBA32_32x32_2x),
    (128, icns::IconType::RGBA32_128x128),
    (256, icns::IconType::RGBA32_128x128_2x),
    (256, icns::IconType::RGBA32_256x256),
    (512, icns::IconType::RGBA32_256x256_2x),
    (512, icns::IconType::RGBA32_512x512),
    (1024, icns::IconType::RGBA32_512x512_2x),
];

fn render(tree: &usvg::Tree, px: u32) -> tiny_skia::Pixmap {
    let mut pixmap = tiny_skia::Pixmap::new(px, px).expect("pixmap");
    let scale = px as f32 / tree.size().width();
    resvg::render(tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
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
    let [svg, png_dir, ico_out, icns_out] = args.as_slice() else {
        eprintln!("usage: superba-icon-render <svg> <png-dir> <ico-out> <icns-out>");
        std::process::exit(2);
    };
    let data = fs::read(svg).expect("read svg");
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("parse svg");
    assert_eq!(tree.size().width(), 1024.0, "the master SVG is 1024x1024");

    fs::create_dir_all(png_dir).expect("png dir");
    for px in PNG_SIZES {
        let path = Path::new(png_dir).join(format!("superba-{px}.png"));
        render(&tree, px).save_png(path).expect("write png");
    }

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
    dir.write(fs::File::create(ico_out).expect("ico file")).expect("write ico");

    let mut family = icns::IconFamily::new();
    for (px, ty) in ICNS_TYPES {
        let image = icns::Image::from_data(icns::PixelFormat::RGBA, px, px, rgba(&render(&tree, px)))
            .expect("icns image");
        family.add_icon_with_type(&image, ty).expect("icns icon");
    }
    family.write(fs::File::create(icns_out).expect("icns file")).expect("write icns");
}
