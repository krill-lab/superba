//! Fonts: bundled Geist Sans / Geist Mono static weights plus a CJK fallback
//! discovered on the system at run time (plan 11.3).
//!
//! egui cannot pick a weight from a family, so every weight is registered as its
//! own `FontFamily::Name` ([`family`]) and headings / buttons reference it
//! explicitly through [`font`]. The CJK fallback is appended to every family; when
//! medium and bold CJK faces back the corresponding weights. If medium is absent,
//! bold backs medium too, so emphasized Chinese does not fall back to regular.
//!
//! Discovery order:
//! - Linux: `fc-match` for `sans-serif:lang=zh-cn` (respects the user's fontconfig),
//!   then well-known paths (Noto Sans CJK, Source Han Sans, WenQuanYi), then a full
//!   `fontdb` system scan. Serif CJK faces (Noto Serif CJK) are a last resort.
//! - macOS: PingFang (`/System/Library/Fonts` or the `AssetsV2` mobile-asset
//!   location), the system's PingFang UI, Hiragino Sans GB, STHeiti; then a system scan.
//! - Windows: Microsoft YaHei UI / YaHei (`msyh.ttc`, bold `msyhbd.ttc`), DengXian,
//!   SimHei; then a system scan.
//!
//! Faces inside `.ttc` collections are selected by family name, not by index, so the
//! Simplified Chinese face is used.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily, FontId};

/// Geist Regular (400).
pub const GEIST_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Geist-Regular.ttf");
/// Geist Medium (500).
pub const GEIST_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Geist-Medium.ttf");
/// Geist SemiBold (600).
pub const GEIST_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Geist-SemiBold.ttf");
/// Geist Mono Regular (400).
pub const GEIST_MONO_REGULAR: &[u8] = include_bytes!("../../assets/fonts/GeistMono-Regular.ttf");
/// Geist Mono Medium (500).
pub const GEIST_MONO_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/GeistMono-Medium.ttf");

/// Names of the registered font families.
pub mod family {
    /// Geist Medium.
    pub const MEDIUM: &str = "geist-medium";
    /// Geist SemiBold.
    pub const SEMIBOLD: &str = "geist-semibold";
    /// Geist Mono Medium.
    pub const MONO_MEDIUM: &str = "geist-mono-medium";
}

/// Font weights available to widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    /// 400.
    Regular,
    /// 500.
    Medium,
    /// 600.
    Semibold,
}

/// The egui family for a sans-serif weight.
pub fn sans(weight: Weight) -> FontFamily {
    match weight {
        Weight::Regular => FontFamily::Proportional,
        Weight::Medium => FontFamily::Name(family::MEDIUM.into()),
        Weight::Semibold => FontFamily::Name(family::SEMIBOLD.into()),
    }
}

/// The egui family for a monospace weight (semibold maps to medium).
pub fn mono(weight: Weight) -> FontFamily {
    match weight {
        Weight::Regular => FontFamily::Monospace,
        Weight::Medium | Weight::Semibold => FontFamily::Name(family::MONO_MEDIUM.into()),
    }
}

/// Sans font id.
pub fn font(size: f32, weight: Weight) -> FontId {
    FontId::new(size, sans(weight))
}

/// Monospace font id.
pub fn mono_font(size: f32, weight: Weight) -> FontId {
    FontId::new(size, mono(weight))
}

/// One CJK face found on the system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CjkFace {
    /// Family name (e.g. `Noto Sans CJK SC`).
    pub family: String,
    /// File path.
    pub path: PathBuf,
    /// Face index inside a collection.
    pub index: u32,
    /// OpenType weight (400 regular, 500 medium, 700 bold), or a `wght` axis value.
    pub weight: u16,
}

/// Result of the CJK discovery (also shown on the diagnostics page).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontReport {
    /// Regular CJK face.
    pub cjk_regular: Option<CjkFace>,
    /// Medium CJK face (backs both medium families; bold is used if absent).
    pub cjk_medium: Option<CjkFace>,
    /// Bold CJK face (backs the semibold family).
    pub cjk_bold: Option<CjkFace>,
}

impl FontReport {
    /// The face used for medium CJK text, including the bold/regular fallback.
    pub fn medium_face(&self) -> Option<&CjkFace> {
        self.cjk_medium
            .as_ref()
            .or(self.cjk_bold.as_ref())
            .or(self.cjk_regular.as_ref())
    }
}

/// Preferred CJK families, best first. Serif families are last-resort fallbacks.
pub const CJK_FAMILIES: &[&str] = &[
    "PingFang SC",
    "PingFang UI SC",
    ".PingFang SC",
    ".PingFang UI SC",
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "Source Han Sans CN",
    "Hiragino Sans GB",
    "WenQuanYi Micro Hei",
    "WenQuanYi Zen Hei",
    "DengXian",
    "Heiti SC",
    "STHeiti",
    "SimHei",
    "Noto Sans CJK JP",
    "Noto Serif CJK SC",
    "Source Han Serif SC",
    "SimSun",
];

/// Well-known CJK font files for this platform (only existing ones are loaded).
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        for p in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Medium.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Medium.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Bold.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Medium.ttc",
            "/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc",
            "/usr/share/fonts/adobe-source-han-sans/SourceHanSansSC-Regular.otf",
            "/usr/share/fonts/adobe-source-han-sans/SourceHanSansSC-Bold.otf",
            "/usr/share/fonts/adobe-source-han-sans/SourceHanSansSC-Medium.otf",
            "/usr/share/fonts/opentype/source-han-sans/SourceHanSansSC-Medium.otf",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
            "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSerifCJK-Bold.ttc",
        ] {
            out.push(PathBuf::from(p));
        }
        out.extend(fc_match_files());
    }
    #[cfg(target_os = "macos")]
    {
        // Older macOS keeps PingFang in /System/Library/Fonts; newer releases ship it
        // as a mobile asset under /System/Library/AssetsV2/com_apple_MobileAsset_Font*/
        // <hash>.asset/AssetData/PingFang.ttc. CoreText would resolve the exact path;
        // walking the two known levels is enough and needs no Objective-C. The
        // collection includes PingFang SC Medium; pick_cjk selects it by weight.
        out.push(PathBuf::from("/System/Library/Fonts/PingFang.ttc"));
        out.extend(macos_asset_fonts("PingFang.ttc"));
        for p in [
            "/System/Library/PrivateFrameworks/FontServices.framework/Versions/A/Resources/Reserved/PingFangUI.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/STHeiti Medium.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
        ] {
            out.push(PathBuf::from(p));
        }
    }
    #[cfg(target_os = "windows")]
    {
        // YaHei UI is a face in these collections. It normally has no 500 face;
        // its bold face is the medium fallback, rather than YaHei UI Regular.
        let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        let fonts = PathBuf::from(windir).join("Fonts");
        for f in [
            "msyh.ttc",
            "msyhbd.ttc",
            "Deng.ttf",
            "Dengb.ttf",
            "simhei.ttf",
            "simsun.ttc",
        ] {
            out.push(fonts.join(f));
        }
    }
    out
}

/// Files fontconfig picks for Chinese sans-serif text (regular, medium and bold).
#[cfg(target_os = "linux")]
fn fc_match_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for pattern in [
        "sans-serif:lang=zh-cn",
        "sans-serif:lang=zh-cn:weight=medium",
        "sans-serif:lang=zh-cn:weight=bold",
    ] {
        let Ok(output) = superba_core::process::command("fc-match")
            .args(["-f", "%{file}", pattern])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !path.is_empty() {
                out.push(PathBuf::from(path));
            }
        }
    }
    out
}

/// `/System/Library/AssetsV2/com_apple_MobileAsset_Font*/*/AssetData/<file>`.
#[cfg(target_os = "macos")]
fn macos_asset_fonts(file: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(roots) = std::fs::read_dir("/System/Library/AssetsV2") else {
        return out;
    };
    for root in roots.flatten() {
        if !root
            .file_name()
            .to_string_lossy()
            .starts_with("com_apple_MobileAsset_Font")
        {
            continue;
        }
        let Ok(assets) = std::fs::read_dir(root.path()) else {
            continue;
        };
        for asset in assets.flatten() {
            let candidate = asset.path().join("AssetData").join(file);
            if candidate.is_file() {
                out.push(candidate);
            }
        }
    }
    out
}

/// Pick the first preferred CJK family with a regular face, plus real medium
/// (500–599) and bold (600+) faces of that family when available.
pub fn pick_cjk(db: &fontdb::Database) -> FontReport {
    pick_cjk_families(db, CJK_FAMILIES)
}

fn pick_cjk_families(db: &fontdb::Database, families: &[&str]) -> FontReport {
    let faces_of = |name: &str| -> Vec<CjkFace> {
        db.faces()
            .filter(|face| face.style == fontdb::Style::Normal)
            .filter(|face| face.families.iter().any(|(f, _)| f == name))
            .filter_map(|face| {
                let path = match &face.source {
                    fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => p.clone(),
                    fontdb::Source::Binary(_) => return None,
                };
                // Private UI collections can expose a hidden family name (a
                // leading dot), or reuse PingFang SC. Keep diagnostics explicit.
                let family = if path.file_name().is_some_and(|p| p == "PingFangUI.ttc") {
                    "PingFang UI SC"
                } else {
                    name
                };
                let candidate = CjkFace {
                    family: family.to_owned(),
                    path,
                    index: face.index,
                    weight: face.weight.0,
                };
                let range = db
                    .with_face_data(face.id, alignment::weight_range)
                    .flatten();
                Some(if let Some((min, max)) = range {
                    // Recent PingFang UI collections store variable faces whose
                    // OS/2 default weight is 500. Instantiate their real weight
                    // axis rather than rendering all three UI weights at 500.
                    [400_u16, 500, 700]
                        .map(|weight| CjkFace {
                            weight: weight.clamp(min, max),
                            ..candidate.clone()
                        })
                        .to_vec()
                } else {
                    vec![candidate]
                })
            })
            .flatten()
            .collect()
    };
    let closest = |faces: &[CjkFace], wanted: u16, accept: &dyn Fn(u16) -> bool| {
        faces
            .iter()
            .filter(|f| accept(f.weight))
            .min_by_key(|f| f.weight.abs_diff(wanted))
            .cloned()
    };
    for name in families {
        let faces = faces_of(name);
        let Some(regular) = closest(&faces, 400, &|w| w <= 500) else {
            continue;
        };
        let bold = closest(&faces, 700, &|w| w >= 600);
        return FontReport {
            cjk_regular: Some(regular),
            cjk_medium: closest(&faces, 500, &|w| (500..600).contains(&w)),
            cjk_bold: bold,
        };
    }
    FontReport::default()
}

/// Discover the CJK fallback faces on this system.
pub fn discover_cjk() -> FontReport {
    let mut db = fontdb::Database::new();
    for path in candidate_paths() {
        if path.is_file() {
            let _ = db.load_font_file(&path);
        }
    }
    #[cfg(feature = "font-test")]
    if let Ok(family) = std::env::var("SUPERBA_CJK_FONT") {
        // Only test builds honor the override; normal releases always discover
        // fonts automatically. Never load a font supplied by an arbitrary path.
        if CJK_FAMILIES.contains(&family.as_str()) {
            db.load_system_fonts();
            return pick_cjk_families(&db, &[&family]);
        }
    }
    let report = pick_cjk(&db);
    if report.cjk_regular.is_some() {
        return report;
    }
    db.load_system_fonts();
    pick_cjk(&db)
}

fn read_face(face: &CjkFace, geist: &[u8]) -> Option<FontData> {
    let bytes = std::fs::read(&face.path).ok()?;
    #[cfg(not(target_os = "macos"))]
    let index = face.index;
    #[cfg(target_os = "macos")]
    let (bytes, index) = macos::renderable_face(bytes, face)?;
    Some(face_data(bytes, index, face, geist))
}

fn face_data(bytes: Vec<u8>, index: u32, face: &CjkFace, geist: &[u8]) -> FontData {
    let tweak = cjk_tweak(&bytes, index, face.weight, &face.family, geist);
    FontData::from_blob(Arc::new(bytes), index).tweak(tweak)
}

mod alignment;
#[cfg(any(target_os = "macos", test))]
mod cache;
mod loading;
#[cfg(target_os = "macos")]
mod macos;
pub use loading::InstalledFonts;
#[cfg(any(target_os = "macos", test))]
mod sfnt;
use alignment::cjk_tweak;

/// Build the font definitions: Geist weights first, the CJK faces as fallbacks.
pub fn definitions(report: &FontReport) -> FontDefinitions {
    definitions_with(report, read_face)
}

fn definitions_with(
    report: &FontReport,
    mut read: impl FnMut(&CjkFace, &[u8]) -> Option<FontData>,
) -> FontDefinitions {
    let mut defs = FontDefinitions::empty();
    let mut add = |name: &str, data: FontData| {
        defs.font_data.insert(name.to_owned(), Arc::new(data));
    };
    add("geist-regular", FontData::from_static(GEIST_REGULAR));
    add("geist-medium", FontData::from_static(GEIST_MEDIUM));
    add("geist-semibold", FontData::from_static(GEIST_SEMIBOLD));
    add("geist-mono", FontData::from_static(GEIST_MONO_REGULAR));
    add(
        "geist-mono-medium",
        FontData::from_static(GEIST_MONO_MEDIUM),
    );

    let regular = report
        .cjk_regular
        .as_ref()
        .and_then(|face| read(face, GEIST_REGULAR));
    let medium = report
        .cjk_medium
        .as_ref()
        .and_then(|face| read(face, GEIST_MEDIUM));
    let bold = report
        .cjk_bold
        .as_ref()
        .and_then(|face| read(face, GEIST_SEMIBOLD));
    let has_regular = regular.is_some();
    let has_medium = medium.is_some();
    let has_bold = bold.is_some();
    if let Some(data) = regular {
        add("cjk-regular", data);
    }
    if let Some(data) = bold {
        add("cjk-bold", data);
    }
    if let Some(data) = medium {
        add("cjk-medium", data);
    }

    let chain = |primary: &str, weight: Weight| -> Vec<String> {
        let mut v = vec![primary.to_owned()];
        if weight == Weight::Medium && has_medium {
            v.push("cjk-medium".into());
        }
        if weight != Weight::Regular && has_bold {
            v.push("cjk-bold".into());
        }
        if has_regular {
            v.push("cjk-regular".into());
        }
        // Geist Sans covers some symbols Geist Mono lacks and vice versa.
        if primary.starts_with("geist-mono") {
            v.push("geist-regular".into());
        } else {
            v.push("geist-mono".into());
        }
        v
    };
    defs.families.insert(
        FontFamily::Proportional,
        chain("geist-regular", Weight::Regular),
    );
    defs.families
        .insert(FontFamily::Monospace, chain("geist-mono", Weight::Regular));
    defs.families.insert(
        FontFamily::Name(family::MEDIUM.into()),
        chain("geist-medium", Weight::Medium),
    );
    defs.families.insert(
        FontFamily::Name(family::SEMIBOLD.into()),
        chain("geist-semibold", Weight::Semibold),
    );
    defs.families.insert(
        FontFamily::Name(family::MONO_MEDIUM.into()),
        chain("geist-mono-medium", Weight::Medium),
    );
    defs
}

/// The bundled Geist faces alone, without any CJK fallback. Every character the
/// UI draws must be in these fonts or be CJK; anything else renders as the
/// missing-glyph box (the glyph coverage test checks this).
pub fn bundled_definitions() -> FontDefinitions {
    definitions(&FontReport::default())
}

/// Characters the system CJK fallback face draws (as the glyph coverage test
/// assumes): CJK symbols and punctuation, the unified ideographs, half-width and
/// full-width forms.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x303F | 0x4E00..=0x9FFF | 0xFF00..=0xFFEF)
}

/// `text` without the characters Superba cannot draw: anything neither in the
/// bundled Geist faces nor CJK (emoji, pictographs, private-use symbols, ...)
/// is dropped and whitespace runs collapse to one space. For text Superba did
/// not write, such as the name a user gave a device in the desktop app (user
/// bug 36): a missing glyph would render as a box.
pub fn drawable(text: &str) -> String {
    static COVERAGE: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();
    let ctx = COVERAGE.get_or_init(|| {
        let ctx = egui::Context::default();
        ctx.set_fonts(bundled_definitions());
        // The fonts are built on the first pass.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .drop_without_applying_deltas();
        ctx
    });
    let id = FontId::new(14.0, FontFamily::Proportional);
    let kept: String = ctx.fonts_mut(|f| {
        text.chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .filter(|&c| c == ' ' || is_cjk(c) || (!c.is_control() && f.has_glyph(&id, c)))
            .collect()
    });
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Install fonts without blocking the first frame on CoreText conversion.
/// Call [`InstalledFonts::poll`] before laying out each frame.
pub fn install(ctx: &egui::Context) -> InstalledFonts {
    let report = discover_cjk();
    #[cfg(target_os = "macos")]
    {
        let cache = superba_switch::detect_context()
            .ok()
            .map(|context| cache::FontCache::new(context.data_dir.cache_dir().join("fonts")));
        macos::install(ctx, report, cache)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let definitions = definitions(&report);
        InstalledFonts::new(ctx, report, definitions)
    }
}

/// True if `path` looks like a serif CJK face (used only as a last resort).
pub fn is_serif_face(face: &CjkFace) -> bool {
    let f = face.family.to_ascii_lowercase();
    f.contains("serif") || f.contains("song") || f.contains("simsun") || is_path_serif(&face.path)
}

fn is_path_serif(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase().contains("serif"))
        .unwrap_or(false)
}
