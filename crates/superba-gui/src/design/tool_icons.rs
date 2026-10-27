//! The tools' own logos, where their official repositories publish them
//! under a license that allows redistribution (sources and licenses:
//! `assets/tool-icons/README.md` and the third-party notices), and Claude
//! Code's mark from claude.ai, used only to identify it. A desktop app
//! without one shows the icon of the copy installed on this computer
//! ([`installed_app`]). Every other tool keeps its two-letter monogram, as
//! does any logo that fails to load.
//!
//! Logos are rasterized with resvg (PNGs scaled with tiny-skia) at the
//! display's pixel density, once per tool, theme and pixel size.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::{Color32, ColorImage, Context, Rect, TextureHandle, TextureOptions, Ui};
use resvg::tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use resvg::usvg;

/// One logo: the monogram it replaces and its artwork.
struct Logo {
    monogram: &'static str,
    art: Art,
}

/// A logo's artwork.
enum Art {
    /// The vendor's SVGs for light and for dark backgrounds.
    Svg {
        light: &'static str,
        dark: &'static str,
    },
    /// One single-colour SVG drawn in `currentColor`: black on light
    /// backgrounds, white on dark ones.
    Glyph(&'static str),
    /// One PNG for both backgrounds (the artwork brings its own plate).
    Png(&'static [u8]),
}

const LOGOS: &[Logo] = &[
    // Anthropic's Claude mark, https://claude.ai/favicon.svg: not licensed;
    // shown only to identify Claude Code, by maintainer decision (README.md).
    Logo {
        monogram: "cc",
        art: Art::Svg {
            light: include_str!("../../assets/tool-icons/claude.svg"),
            dark: include_str!("../../assets/tool-icons/claude.svg"),
        },
    },
    // openai/mcp-extensions, plugins/bits-and-bolts/assets/keycap-legends/codex.svg (Apache-2.0).
    Logo {
        monogram: "cx",
        art: Art::Glyph(include_str!("../../assets/tool-icons/codex.svg")),
    },
    // deepseek-ai/deepseek-harness, apps/web/public/favicon{,-dark}.svg (MIT).
    Logo {
        monogram: "ds",
        art: Art::Svg {
            light: include_str!("../../assets/tool-icons/dsh-light.svg"),
            dark: include_str!("../../assets/tool-icons/dsh-dark.svg"),
        },
    },
    // sst/opencode, packages/console/app/src/asset/brand/opencode-logo-*-square.svg (MIT).
    Logo {
        monogram: "oc",
        art: Art::Svg {
            light: include_str!("../../assets/tool-icons/opencode-light.svg"),
            dark: include_str!("../../assets/tool-icons/opencode-dark.svg"),
        },
    },
    // earendil-works/pi, the logo of packages/coding-agent/src/modes/interactive/components/pi-logo.ts (MIT).
    Logo {
        monogram: "pi",
        art: Art::Svg {
            light: include_str!("../../assets/tool-icons/pi.svg"),
            dark: include_str!("../../assets/tool-icons/pi.svg"),
        },
    },
    // google-gemini/gemini-cli, packages/vscode-ide-companion/assets/icon.png (Apache-2.0).
    Logo {
        monogram: "gm",
        art: Art::Png(include_bytes!("../../assets/tool-icons/gemini-cli.png")),
    },
    // zai-org/ZCode, packages/desktop/build/icons/256x256.png (Apache-2.0).
    Logo {
        monogram: "zc",
        art: Art::Png(include_bytes!("../../assets/tool-icons/zcode.png")),
    },
];

/// Whether `monogram` has a logo Superba ships.
pub fn ships_logo(monogram: &str) -> bool {
    LOGOS.iter().any(|l| l.monogram == monogram)
}

/// The installed copy of a desktop app whose logo Superba does not ship.
enum Installed {
    /// Known, not read yet.
    Found { path: PathBuf, name: &'static str },
    /// Being read off the egui thread.
    Reading { path: PathBuf },
    /// Read.
    Ready { path: PathBuf, icon: Arc<Pixmap> },
    /// Unreadable: the monogram stays.
    Failed { path: PathBuf },
}
impl Installed {
    fn path(&self) -> &PathBuf {
        match self {
            Self::Found { path, .. }
            | Self::Reading { path }
            | Self::Ready { path, .. }
            | Self::Failed { path } => path,
        }
    }
}
static INSTALLED: Mutex<Option<HashMap<&'static str, Installed>>> = Mutex::new(None);

/// Show the icon of the app installed at `path` for `monogram` (its Linux
/// package `name` finds the icon there); `None` when it is not installed.
/// Ignored for a tool whose logo Superba ships. Cheap: the app is read the
/// first time the icon is drawn, off the egui thread.
pub fn installed_app(monogram: &'static str, app: Option<(PathBuf, &'static str)>) {
    if ships_logo(monogram) {
        return;
    }
    let mut installed = INSTALLED.lock().unwrap_or_else(|p| p.into_inner());
    let installed = installed.get_or_insert_with(HashMap::new);
    match app {
        Some((path, name)) => {
            if installed.get(monogram).map(Installed::path) != Some(&path) {
                installed.insert(monogram, Installed::Found { path, name });
                forget_textures(monogram);
            }
        }
        None => {
            if installed.remove(monogram).is_some() {
                forget_textures(monogram);
            }
        }
    }
}

/// The installed app's icon for `monogram` (and the key it is kept under),
/// reading it first if needed.
fn installed_icon(ctx: &Context, monogram: &str) -> Option<(&'static str, Arc<Pixmap>)> {
    let mut installed = INSTALLED.lock().unwrap_or_else(|p| p.into_inner());
    let installed = installed.as_mut()?;
    let key = *installed.get_key_value(monogram)?.0;
    let state = installed.get_mut(key)?;
    match state {
        Installed::Ready { icon, .. } => Some((key, icon.clone())),
        Installed::Found { path, name } => {
            let (path, name) = (path.clone(), *name);
            *state = Installed::Reading { path: path.clone() };
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let icon = super::installed_icon::load(&path, name);
                let mut installed = INSTALLED.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(slot) = installed.as_mut().and_then(|m| m.get_mut(key))
                    && *slot.path() == path
                {
                    *slot = match icon {
                        Some(icon) => Installed::Ready {
                            path,
                            icon: Arc::new(icon),
                        },
                        None => Installed::Failed { path },
                    };
                    forget_textures(key);
                }
                ctx.request_repaint();
            });
            None
        }
        Installed::Reading { .. } | Installed::Failed { .. } => None,
    }
}

type Key = (&'static str, bool, u32);
/// `None`: rasterizing failed; the monogram is drawn instead.
static CACHE: Mutex<Option<HashMap<Key, Option<TextureHandle>>>> = Mutex::new(None);

fn forget_textures(monogram: &str) {
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(cache) = cache.as_mut() {
        cache.retain(|k, _| k.0 != monogram);
    }
}

/// A square RGBA rendering of `svg`, `px` pixels wide, the artwork centered;
/// `ink` is its `currentColor`.
pub fn rasterize(svg: &str, ink: Option<&str>, px: u32) -> Option<ColorImage> {
    if px == 0 || px > 1024 {
        return None;
    }
    let options = usvg::Options {
        style_sheet: ink.map(|c| format!("svg {{ color: {c} }}")),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &options).ok()?;
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
    Some(image(&pixmap))
}

/// `image` scaled into a square `px` pixels wide, centered: halved while it
/// is twice too large (each halving averages 2 x 2 pixels), then filtered.
pub fn fit(image: &Pixmap, px: u32) -> Option<ColorImage> {
    if px == 0 || px > 1024 || image.width() == 0 {
        return None;
    }
    let mut src = image.clone();
    while src.width().max(src.height()) >= 2 * px {
        let (w, h) = (src.width().div_ceil(2), src.height().div_ceil(2));
        let mut half = Pixmap::new(w, h)?;
        half.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &PixmapPaint {
                quality: FilterQuality::Bilinear,
                ..Default::default()
            },
            Transform::from_scale(0.5, 0.5),
            None,
        );
        src = half;
    }
    let scale = px as f32 / src.width().max(src.height()) as f32;
    let dx = (px as f32 - src.width() as f32 * scale) / 2.0;
    let dy = (px as f32 - src.height() as f32 * scale) / 2.0;
    let mut out = Pixmap::new(px, px)?;
    out.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..Default::default()
        },
        Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
        None,
    );
    Some(self::image(&out))
}

fn image(pixmap: &Pixmap) -> ColorImage {
    ColorImage::from_rgba_premultiplied(
        [pixmap.width() as usize, pixmap.height() as usize],
        pixmap.data(),
    )
}

/// The artwork of `logo` on a light or dark background.
fn render(art: &Art, dark: bool, px: u32) -> Option<ColorImage> {
    match art {
        Art::Svg { light, dark: d } => rasterize(if dark { d } else { light }, None, px),
        Art::Glyph(svg) => rasterize(svg, Some(if dark { "#ffffff" } else { "#000000" }), px),
        Art::Png(data) => fit(&Pixmap::decode_png(data).ok()?, px),
    }
}

fn texture(ctx: &Context, monogram: &str, dark: bool, px: u32) -> Option<TextureHandle> {
    let logo = LOGOS.iter().find(|l| l.monogram == monogram);
    let (name, installed) = match logo {
        Some(l) => (l.monogram, None),
        None => {
            let (name, icon) = installed_icon(ctx, monogram)?;
            (name, Some(icon))
        }
    };
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    cache
        .entry((name, dark, px))
        .or_insert_with(|| {
            let image = match (logo, installed) {
                (Some(l), _) => render(&l.art, dark, px)?,
                (None, Some(icon)) => fit(&icon, px)?,
                (None, None) => return None,
            };
            Some(ctx.load_texture(
                format!("superba-tool-icon-{name}-{dark}-{px}"),
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
    fn every_logo_renders_on_both_backgrounds() {
        for l in LOGOS {
            if let Art::Svg { light, dark } = &l.art {
                assert!(!light.contains("<script") && !dark.contains("<script"));
            }
            if let Art::Glyph(svg) = &l.art {
                assert!(!svg.contains("<script"));
            }
            for dark in [false, true] {
                let img = render(&l.art, dark, 40).expect(l.monogram);
                assert_eq!(img.size, [40, 40]);
                let opaque = img.pixels.iter().filter(|p| p.a() > 0).count();
                assert!(opaque > 40, "{} is blank", l.monogram);
            }
        }
    }

    #[test]
    fn glyph_takes_the_ink_of_the_background() {
        let Art::Glyph(svg) = LOGOS[1].art else {
            panic!("Codex is a glyph");
        };
        let ink = |dark| {
            render(&Art::Glyph(svg), dark, 40)
                .unwrap()
                .pixels
                .into_iter()
                .filter(|p| p.a() == 255)
                .map(|p| p.r())
                .max()
                .unwrap()
        };
        assert_eq!(ink(false), 0);
        assert_eq!(ink(true), 255);
    }

    #[test]
    fn png_logos_are_scaled_down_whole() {
        // Gemini CLI's 1645 pixel icon: its plate reaches the edges, centred.
        let Art::Png(data) = LOGOS[5].art else {
            panic!("Gemini CLI is a PNG");
        };
        let img = fit(&Pixmap::decode_png(data).unwrap(), 26).unwrap();
        assert_eq!(img.size, [26, 26]);
        assert!(img.pixels[13 * 26 + 13].a() == 255);
        assert!(img.pixels[13 * 26].a() > 0 && img.pixels[13 * 26 + 25].a() > 0);
    }

    #[test]
    fn only_workbuddy_has_no_shipped_logo() {
        assert!(!ships_logo("wb"));
        for m in ["cc", "cx", "ds", "oc", "pi", "gm", "zc"] {
            assert!(ships_logo(m), "{m}");
        }
    }

    #[test]
    fn an_installed_app_is_ignored_when_a_logo_ships() {
        installed_app("zc", Some((PathBuf::from("/opt/ZCode/zcode"), "zcode")));
        assert!(
            INSTALLED
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|m| !m.contains_key("zc"))
        );
    }

    #[test]
    fn broken_artwork_falls_back() {
        assert!(rasterize("<svg", None, 30).is_none());
        assert!(render(&LOGOS[2].art, false, 0).is_none());
        assert!(render(&Art::Png(b"\x89PNG"), false, 30).is_none());
    }
}
