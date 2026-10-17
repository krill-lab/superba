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
//!   location), Hiragino Sans GB, STHeiti; then a system scan.
//! - Windows: Microsoft YaHei UI / YaHei (`msyh.ttc`, bold `msyhbd.ttc`), DengXian,
//!   SimHei; then a system scan.
//!
//! Faces inside `.ttc` collections are selected by family name, not by index, so the
//! Simplified Chinese face is used.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily, FontId, FontTweak};

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
    /// OpenType weight (400 regular, 500 medium, 700 bold).
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
    let faces_of = |name: &str| -> Vec<CjkFace> {
        db.faces()
            .filter(|face| face.style == fontdb::Style::Normal)
            .filter(|face| face.families.iter().any(|(f, _)| f == name))
            .filter_map(|face| {
                let path = match &face.source {
                    fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => p.clone(),
                    fontdb::Source::Binary(_) => return None,
                };
                Some(CjkFace {
                    family: name.to_owned(),
                    path,
                    index: face.index,
                    weight: face.weight.0,
                })
            })
            .collect()
    };
    let closest = |faces: &[CjkFace], wanted: u16, accept: &dyn Fn(u16) -> bool| {
        faces
            .iter()
            .filter(|f| accept(f.weight))
            .min_by_key(|f| f.weight.abs_diff(wanted))
            .cloned()
    };
    for name in CJK_FAMILIES {
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
    let report = pick_cjk(&db);
    if report.cjk_regular.is_some() {
        return report;
    }
    db.load_system_fonts();
    pick_cjk(&db)
}

fn read_face(face: &CjkFace) -> Option<FontData> {
    let bytes = std::fs::read(&face.path).ok()?;
    Some(FontData::from_blob(Arc::new(bytes), face.index).tweak(cjk_tweak()))
}

/// Size/baseline tweak that makes CJK glyphs sit on Geist's baseline with a similar
/// optical size.
fn cjk_tweak() -> FontTweak {
    FontTweak {
        scale: 0.96,
        y_offset_factor: 0.02,
        ..FontTweak::default()
    }
}

/// Build the font definitions: Geist weights first, the CJK faces as fallbacks.
pub fn definitions(report: &FontReport) -> FontDefinitions {
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

    let regular = report.cjk_regular.as_ref().and_then(read_face);
    let medium = report.cjk_medium.as_ref().and_then(read_face);
    let bold = report.cjk_bold.as_ref().and_then(read_face);
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

/// Discover fonts and install them into `ctx`. Returns what was found.
pub fn install(ctx: &egui::Context) -> FontReport {
    let report = discover_cjk();
    ctx.set_fonts(definitions(&report));
    report
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
