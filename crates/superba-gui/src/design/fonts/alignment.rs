//! Align the *ink* of a system CJK face with Geist, accounting for egui's
//! centering of fallback line boxes. Line metrics alone do not describe ink:
//! Hiragino and PingFang can have similar em sizes but different ideograph boxes.

use egui::FontTweak;
use skrifa::{
    FontRef, MetadataProvider, Tag,
    instance::{LocationRef, Size},
};

#[derive(Clone, Copy, Debug)]
struct Metrics {
    em: f32,
    ascent: f32,
    descent: f32,
    leading: f32,
    cap_height: Option<f32>,
    x_height: Option<f32>,
    ink_bottom: f32,
    ink_top: f32,
}

impl Metrics {
    fn read(bytes: &[u8], index: u32, weight: u16, sample: &str) -> Option<Self> {
        let face = FontRef::from_index(bytes, index).ok()?;
        let location = LocationRef::default();
        // Use the same parser and OS/2/hhea selection as egui. In particular,
        // using hhea unconditionally gives the wrong baseline for some fonts.
        let m = face.metrics(Size::unscaled(), location);
        // egui caches line metrics at the default location, but shapes and
        // rasterizes the requested variation. Match both parts of that behavior.
        let instance = face.axes().location([("wght", f32::from(weight))]);
        let glyphs = face.glyph_metrics(Size::unscaled(), &instance);
        let charmap = face.charmap();
        let mut bottom = f32::INFINITY;
        let mut top = f32::NEG_INFINITY;
        for ch in sample.chars() {
            if let Some(bounds) = charmap.map(ch).and_then(|id| glyphs.bounds(id))
                && bounds.y_max > bounds.y_min
            {
                bottom = bottom.min(bounds.y_min);
                top = top.max(bounds.y_max);
            }
        }
        let result = Self {
            em: f32::from(m.units_per_em),
            ascent: m.ascent,
            descent: m.descent,
            leading: m.leading,
            cap_height: m.cap_height,
            x_height: m.x_height,
            ink_bottom: bottom,
            ink_top: top,
        };
        result.valid().then_some(result)
    }

    fn valid(self) -> bool {
        [
            self.em,
            self.ascent,
            self.descent,
            self.leading,
            self.ink_bottom,
            self.ink_top,
        ]
        .into_iter()
        .all(f32::is_finite)
            && self.em > 0.0
            && self.ascent > self.descent
            && self.ascent - self.descent + self.leading > 0.0
            && self.ink_top > self.ink_bottom
    }

    /// Position of the visual center relative to the center of the line box,
    /// in em units, with screen y increasing downwards.
    fn center(self) -> f32 {
        (self.ascent + self.descent - self.leading - self.ink_top - self.ink_bottom)
            / (2.0 * self.em)
    }
}

fn positive(value: Option<f32>) -> Option<f32> {
    value.filter(|v| v.is_finite() && *v > 0.0)
}

fn from_metrics(cjk: Metrics, geist: Metrics) -> Option<FontTweak> {
    if !cjk.valid() || !geist.valid() {
        return None;
    }
    let cap = positive(geist.cap_height)
        .or_else(|| positive(geist.x_height).map(|x| x * 1.4))
        .unwrap_or(geist.ink_top - geist.ink_bottom);
    // A square ideograph needs a little more ink height than Latin capitals
    // to keep dense strokes readable at UI sizes. Keep this ratio consistent
    // across families instead of applying a constant scale to different ems.
    let scale = 1.2 * (cap / geist.em) / ((cjk.ink_top - cjk.ink_bottom) / cjk.em);
    // egui centers each fallback's line box within the primary line box, then
    // applies y_offset_factor * font_size * scale. Thus:
    //   scale * (cjk.center + offset) = geist.center.
    // Geist's alignment target is its capitals/digits, not lowercase descenders.
    let latin = Metrics {
        ink_bottom: 0.0,
        ink_top: cap,
        ..geist
    };
    let y_offset_factor = latin.center() / scale - cjk.center();
    // Broken tables must not make text enormous or shift it outside its row.
    if !(0.5..=1.6).contains(&scale) || y_offset_factor.abs() > 0.5 {
        return None;
    }
    Some(FontTweak {
        scale,
        y_offset_factor,
        ..Default::default()
    })
}

fn fallback(family: &str) -> FontTweak {
    // Only used when metrics/ideograph outlines cannot be read. Values are
    // specific to these families, relative to Geist's 0.71-em cap height.
    const FAMILIES: &[(&str, f32, f32)] = &[
        ("PingFang UI", 0.94, -0.01),
        ("PingFang", 0.92, 0.0),
        ("Hiragino", 0.93, 0.251),
        ("STHeiti", 0.92, 0.0065),
        ("Heiti SC", 0.92, 0.0065),
        ("YaHei", 0.96, 0.02),
        ("Noto Sans", 0.92, -0.053),
        ("Source Han Sans", 0.92, -0.053),
        ("WenQuanYi", 0.87, 0.073),
    ];
    let (_, scale, y_offset_factor) = FAMILIES
        .iter()
        .find(|(name, _, _)| family.contains(name))
        .copied()
        .unwrap_or(("", 1.0, 0.0));
    FontTweak {
        scale,
        y_offset_factor,
        ..Default::default()
    }
}

pub(super) fn weight_range(bytes: &[u8], index: u32) -> Option<(u16, u16)> {
    let face = FontRef::from_index(bytes, index).ok()?;
    let axis = face.axes().get_by_tag(Tag::new(b"wght"))?;
    let (min, max) = (axis.min_value().ceil(), axis.max_value().floor());
    (min >= 1.0 && max <= 1000.0 && min <= max).then_some((min as u16, max as u16))
}

pub(super) fn cjk_tweak(
    bytes: &[u8],
    index: u32,
    weight: u16,
    family: &str,
    geist: &[u8],
) -> FontTweak {
    // Common full-height ideographs, independent of the strings in the UI or
    // regression tests. Avoid punctuation and short glyphs such as 一/二.
    let mut tweak = Metrics::read(bytes, index, weight, "永国中文田目")
        .zip(Metrics::read(geist, 0, weight, "H0123456789"))
        .and_then(|(cjk, latin)| from_metrics(cjk, latin))
        .unwrap_or_else(|| fallback(family));
    if weight_range(bytes, index).is_some() {
        tweak.coords.push(b"wght", f32::from(weight));
    }
    tweak
}

#[cfg(test)]
#[path = "../../../tests/unit/font_alignment.rs"]
mod tests;
