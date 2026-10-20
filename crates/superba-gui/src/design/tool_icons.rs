//! The tools' own logos, where their official repositories publish them
//! under a license that allows redistribution (sources and licenses:
//! `assets/tool-icons/README.md` and the third-party notices). Every other
//! tool keeps its two-letter monogram, as does any logo that fails to load.
//!
//! Each logo has an ink for light and one for dark backgrounds, both taken
//! from the vendor. They are rasterized with resvg at the display's pixel
//! density, once per tool, theme and pixel size.

use std::collections::HashMap;
use std::sync::Mutex;

use egui::{Color32, ColorImage, Context, Rect, TextureHandle, TextureOptions, Ui};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg;

/// One logo: the monogram it replaces and its two inks.
struct Logo {
    monogram: &'static str,
    light: &'static str,
    dark: &'static str,
}

const LOGOS: &[Logo] = &[
    // deepseek-ai/deepseek-harness, apps/web/public/favicon{,-dark}.svg (MIT).
    Logo {
        monogram: "ds",
        light: include_str!("../../assets/tool-icons/dsh-light.svg"),
        dark: include_str!("../../assets/tool-icons/dsh-dark.svg"),
    },
    // sst/opencode, packages/console/app/src/asset/brand/opencode-logo-*-square.svg (MIT).
    Logo {
        monogram: "oc",
        light: include_str!("../../assets/tool-icons/opencode-light.svg"),
        dark: include_str!("../../assets/tool-icons/opencode-dark.svg"),
    },
];

/// The SVG source for `monogram` on a light or dark background.
pub fn source(monogram: &str, dark: bool) -> Option<&'static str> {
    LOGOS
        .iter()
        .find(|l| l.monogram == monogram)
        .map(|l| if dark { l.dark } else { l.light })
}

type Key = (&'static str, bool, u32);
/// `None`: rasterizing failed; the monogram is drawn instead.
static CACHE: Mutex<Option<HashMap<Key, Option<TextureHandle>>>> = Mutex::new(None);

/// A square RGBA rendering of `svg`, `px` pixels wide, the artwork centered.
pub fn rasterize(svg: &str, px: u32) -> Option<ColorImage> {
    if px == 0 || px > 1024 {
        return None;
    }
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = px as f32 / size.width().max(size.height());
    let dx = (px as f32 - size.width() * scale) / 2.0;
    let dy = (px as f32 - size.height() * scale) / 2.0;
    let mut pixmap = Pixmap::new(px, px)?;
    resvg::render(
        &tree,
        Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
        &mut pixmap.as_mut(),
    );
    Some(ColorImage::from_rgba_premultiplied(
        [px as usize, px as usize],
        pixmap.data(),
    ))
}

fn texture(ctx: &Context, monogram: &str, dark: bool, px: u32) -> Option<TextureHandle> {
    let logo = LOGOS.iter().find(|l| l.monogram == monogram)?;
    let key = (logo.monogram, dark, px);
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    cache
        .entry(key)
        .or_insert_with(|| {
            let svg = if dark { logo.dark } else { logo.light };
            let image = rasterize(svg, px)?;
            Some(ctx.load_texture(
                format!("superba-tool-icon-{}-{dark}-{px}", logo.monogram),
                image,
                TextureOptions::LINEAR,
            ))
        })
        .clone()
}

/// Paint the logo for `monogram` inside the tile `tile`; `false` when the
/// tool has none (or it failed to load) and the caller draws the monogram.
pub fn paint(ui: &Ui, monogram: &str, tile: Rect) -> bool {
    let ppp = ui.ctx().pixels_per_point();
    // The artwork fills 64% of the tile, like the monogram's letters.
    let side = (tile.width() * 0.64).round();
    let px = (side * ppp).round() as u32;
    let Some(tex) = texture(ui.ctx(), monogram, ui.visuals().dark_mode, px) else {
        return false;
    };
    let r = Rect::from_center_size(tile.center(), egui::vec2(side, side));
    ui.painter().image(
        tex.id(),
        r,
        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_logo_rasterizes_in_both_inks() {
        for l in LOGOS {
            for svg in [l.light, l.dark] {
                assert!(!svg.contains("<script"), "{}", l.monogram);
                let img = rasterize(svg, 40).expect(l.monogram);
                assert_eq!(img.size, [40, 40]);
                assert!(img.pixels.iter().any(|p| p.a() > 0), "{} is blank", l.monogram);
            }
        }
    }

    #[test]
    fn tools_without_a_licensed_logo_keep_their_monogram() {
        for m in ["cx", "cc", "pi", "zc", "wb", "gm"] {
            assert!(source(m, false).is_none(), "{m}");
        }
        assert!(source("ds", true).is_some());
        assert!(source("oc", false).is_some());
    }

    #[test]
    fn broken_svg_falls_back() {
        assert!(rasterize("<svg", 30).is_none());
        assert!(rasterize(LOGOS[0].light, 0).is_none());
    }
}
