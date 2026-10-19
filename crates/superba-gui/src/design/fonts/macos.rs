//! CoreText bridge for macOS 26's PingFang UI `hvgl` collection. Fontations
//! cannot decode that private outline format. Ask the OS for the selected
//! face/weight's quadratic paths and cache a local static TTF for egui.

use std::{collections::BTreeMap, ffi::c_void, ptr};

use core_foundation::{
    array::{CFArray, CFArrayRef},
    base::{CFType, CFTypeRef, TCFType},
    data::{CFData, CFDataRef},
    number::{CFNumber, CFNumberRef},
    string::{CFString, CFStringRef},
    url::{CFURL, CFURLRef},
};
use skrifa::{FontRef, MetadataProvider, Tag, raw::TableProvider, string::StringId};

use super::{
    CjkFace, FontReport, InstalledFonts,
    cache::FontCache,
    sfnt::{self, Glyph, Point, put16, put32},
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Pair {
    x: f64,
    y: f64,
}

#[repr(C)]
struct PathElement {
    kind: i32,
    points: *const Pair,
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    static kCTFontNameAttribute: CFStringRef;
    fn CTFontManagerCreateFontDescriptorsFromURL(url: CFURLRef) -> CFArrayRef;
    fn CTFontDescriptorCopyAttribute(descriptor: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    fn CTFontDescriptorCreateCopyWithVariation(
        descriptor: CFTypeRef,
        axis: CFNumberRef,
        value: f64,
    ) -> CFTypeRef;
    fn CTFontCreateWithFontDescriptor(
        descriptor: CFTypeRef,
        size: f64,
        matrix: *const c_void,
    ) -> CFTypeRef;
    fn CTFontGetGlyphCount(font: CFTypeRef) -> isize;
    fn CTFontGetAscent(font: CFTypeRef) -> f64;
    fn CTFontGetDescent(font: CFTypeRef) -> f64;
    fn CTFontGetLeading(font: CFTypeRef) -> f64;
    fn CTFontGetCapHeight(font: CFTypeRef) -> f64;
    fn CTFontGetXHeight(font: CFTypeRef) -> f64;
    fn CTFontCopyTable(font: CFTypeRef, tag: u32, options: u32) -> CFDataRef;
    fn CTFontCreatePathForGlyph(font: CFTypeRef, glyph: u16, matrix: *const c_void) -> CFTypeRef;
    fn CTFontGetAdvancesForGlyphs(
        font: CFTypeRef,
        orientation: u32,
        glyphs: *const u16,
        advances: *mut Pair,
        count: isize,
    ) -> f64;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPathApply(
        path: CFTypeRef,
        info: *mut c_void,
        callback: extern "C" fn(*mut c_void, *const PathElement),
    );
}

/// Take ownership of a nullable result from a Core Foundation Create/Copy call.
unsafe fn owned(value: CFTypeRef) -> Option<CFType> {
    if value.is_null() {
        None
    } else {
        // SAFETY: callers pass owned results from Create/Copy APIs.
        Some(unsafe { CFType::wrap_under_create_rule(value) })
    }
}

struct NativeFont(CFType);

impl NativeFont {
    fn open(face: &CjkFace, postscript: &str) -> Option<Self> {
        let url = CFURL::from_path(&face.path, false)?;
        // SAFETY: the URL remains alive; Create returns a retained CFArray.
        let array = unsafe { CTFontManagerCreateFontDescriptorsFromURL(url.as_concrete_TypeRef()) };
        if array.is_null() {
            return None;
        }
        // SAFETY: checked non-null; array elements are CTFontDescriptors.
        let array = unsafe { CFArray::<CFType>::wrap_under_create_rule(array) };
        for desc in array.iter() {
            // SAFETY: descriptor and key are valid CF objects; Copy transfers ownership.
            let name = unsafe {
                owned(CTFontDescriptorCopyAttribute(
                    desc.as_CFTypeRef(),
                    kCTFontNameAttribute,
                ))
            }?;
            if name.downcast::<CFString>()? != postscript {
                continue;
            }
            let axis = CFNumber::from(i32::from_be_bytes(*b"wght"));
            // SAFETY: all objects live through the calls; null matrix means identity.
            // Enumerate descriptors from this file: resolving private font names
            // through a new name-only descriptor silently substitutes Helvetica.
            let font = unsafe {
                let varied = owned(CTFontDescriptorCreateCopyWithVariation(
                    desc.as_CFTypeRef(),
                    axis.as_concrete_TypeRef(),
                    f64::from(face.weight),
                ))?;
                owned(CTFontCreateWithFontDescriptor(
                    varied.as_CFTypeRef(),
                    1000.0,
                    ptr::null(),
                ))?
            };
            return Some(Self(font));
        }
        None
    }

    fn table(&self, tag: &[u8; 4]) -> Option<Vec<u8>> {
        // SAFETY: live CTFont; Copy returns owned CFData, or null for missing tables.
        let data = unsafe { CTFontCopyTable(self.0.as_CFTypeRef(), u32::from_be_bytes(*tag), 0) };
        if data.is_null() {
            return None;
        }
        // SAFETY: non-null Copy result.
        Some(
            unsafe { CFData::wrap_under_create_rule(data) }
                .bytes()
                .to_vec(),
        )
    }

    fn glyph(&self, id: u16) -> Option<(Glyph, u16)> {
        let mut advance = Pair::default();
        // SAFETY: valid font, one glyph and one writable CGSize (same ABI as Pair).
        let path = unsafe {
            CTFontGetAdvancesForGlyphs(self.0.as_CFTypeRef(), 0, &id, &mut advance, 1);
            owned(CTFontCreatePathForGlyph(
                self.0.as_CFTypeRef(),
                id,
                ptr::null(),
            ))
        };
        if !advance.x.is_finite() || !(0.0..=65535.0).contains(&advance.x) {
            return None;
        }
        let mut pen = Pen::default();
        if let Some(path) = path {
            // SAFETY: CGPathApply calls synchronously. Pen and the path live
            // throughout the call and the callback does not retain pointers.
            unsafe {
                CGPathApply(
                    path.as_CFTypeRef(),
                    (&mut pen as *mut Pen).cast(),
                    path_element,
                );
            }
        }
        pen.finish();
        (!pen.invalid).then_some((pen.glyph, advance.x.round() as u16))
    }

    fn encode(&self, expected_glyphs: u16, weight: u16) -> Option<Vec<u8>> {
        // SAFETY: self holds a live CTFont, all queries are read-only.
        let (count, ascent, descent, leading, cap, xheight) = unsafe {
            let f = self.0.as_CFTypeRef();
            (
                CTFontGetGlyphCount(f),
                CTFontGetAscent(f),
                -CTFontGetDescent(f),
                CTFontGetLeading(f),
                CTFontGetCapHeight(f),
                CTFontGetXHeight(f),
            )
        };
        if count != isize::try_from(expected_glyphs).ok()? {
            return None;
        }
        let mut glyf = Vec::new();
        let mut loca = Vec::new();
        let mut hmtx = Vec::new();
        let (mut max_points, mut max_contours, mut max_advance) = (0, 0, 0);
        let mut bbox = [0_i16; 4];
        let (mut min_lsb, mut min_rsb, mut max_extent) = (0_i16, 0_i16, 0_i16);
        for id in 0..expected_glyphs {
            loca.extend(u32::try_from(glyf.len()).ok()?.to_be_bytes());
            let (glyph, advance) = self.glyph(id)?;
            let b = glyph.bounds();
            bbox = [
                bbox[0].min(b[0]),
                bbox[1].min(b[1]),
                bbox[2].max(b[2]),
                bbox[3].max(b[3]),
            ];
            min_lsb = min_lsb.min(b[0]);
            min_rsb = min_rsb.min(i16::try_from(i32::from(advance) - i32::from(b[2])).ok()?);
            max_extent = max_extent.max(b[2]);
            max_points = max_points.max(glyph.contours.iter().map(Vec::len).sum::<usize>());
            max_contours = max_contours.max(glyph.contours.len());
            max_advance = max_advance.max(advance);
            glyph.encode(&mut glyf)?;
            hmtx.extend(advance.to_be_bytes());
            hmtx.extend(b[0].to_be_bytes());
        }
        loca.extend(u32::try_from(glyf.len()).ok()?.to_be_bytes());
        let mut head = self.table(b"head")?;
        let mut hhea = self.table(b"hhea")?;
        let mut os2 = self.table(b"OS/2")?;
        if head.len() < 54 || hhea.len() < 36 || os2.len() < 90 {
            return None;
        }
        put16(&mut head, 16, 3); // baseline at y=0; lsb equals xMin
        put16(&mut head, 18, 1000);
        for (i, value) in bbox.iter().enumerate() {
            put16(&mut head, 36 + i * 2, *value);
        }
        put16(&mut head, 50, 1); // long loca offsets
        for (offset, value) in [(4, ascent), (6, descent), (8, leading)] {
            put16(&mut hhea, offset, coordinate(value)?);
        }
        put16(&mut hhea, 10, max_advance as i16);
        put16(&mut hhea, 12, min_lsb);
        put16(&mut hhea, 14, min_rsb);
        put16(&mut hhea, 16, max_extent);
        put16(&mut hhea, 34, expected_glyphs as i16);
        put16(&mut os2, 4, weight as i16);
        put16(&mut os2, 62, 0x80); // use the same typo metrics as hhea
        for (offset, value) in [
            (68, ascent),
            (70, descent),
            (72, leading),
            (74, ascent),
            (76, -descent),
            (86, xheight),
            (88, cap),
        ] {
            put16(&mut os2, offset, coordinate(value)?);
        }
        let mut maxp = vec![0; 32];
        put32(&mut maxp, 0, 0x0001_0000);
        put16(&mut maxp, 4, expected_glyphs as i16);
        put16(&mut maxp, 6, u16::try_from(max_points).ok()? as i16);
        put16(&mut maxp, 8, u16::try_from(max_contours).ok()? as i16);
        put16(&mut maxp, 14, 1); // one zone, no instructions
        let mut post = vec![0; 32];
        put32(&mut post, 0, 0x0003_0000); // no glyph names
        sfnt::assemble(BTreeMap::from([
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"hmtx", hmtx),
            (*b"maxp", maxp),
            (*b"OS/2", os2),
            (*b"post", post),
            (*b"glyf", glyf),
            (*b"loca", loca),
            (*b"cmap", self.table(b"cmap")?),
            (*b"name", self.table(b"name")?),
        ]))
    }
}

fn coordinate(value: f64) -> Option<i16> {
    (value.is_finite() && (f64::from(i16::MIN)..=f64::from(i16::MAX)).contains(&value.round()))
        .then_some(value.round() as i16)
}

#[derive(Default)]
struct Pen {
    glyph: Glyph,
    contour: Vec<Point>,
    invalid: bool,
}

impl Pen {
    fn point(&mut self, p: Pair, on_curve: bool) {
        if let (Some(x), Some(y)) = (coordinate(p.x), coordinate(p.y)) {
            self.contour.push(Point { x, y, on_curve });
        } else {
            self.invalid = true;
        }
    }

    fn finish(&mut self) {
        if self.contour.len() > 1 && self.contour.first() == self.contour.last() {
            self.contour.pop();
        }
        if !self.contour.is_empty() {
            self.glyph.contours.push(std::mem::take(&mut self.contour));
        }
    }
}

extern "C" fn path_element(info: *mut c_void, element: *const PathElement) {
    // SAFETY: CGPathApply supplies a live PathElement and the Pen pointer passed
    // by glyph(). Its enum specifies exactly 1/1/2 points for move/line/quad.
    unsafe {
        let pen = &mut *info.cast::<Pen>();
        let e = &*element;
        match e.kind {
            0 => {
                pen.finish();
                pen.point(*e.points, true);
            }
            1 => pen.point(*e.points, true),
            2 => {
                pen.point(*e.points, false);
                pen.point(*e.points.add(1), true);
            }
            4 => pen.finish(),
            // hvgl paths are quadratic. Reject unexpected formats instead of
            // silently dropping a cubic segment or approximating its shape.
            _ => pen.invalid = true,
        }
    }
}

pub(super) fn renderable_face(bytes: Vec<u8>, face: &CjkFace) -> Option<(Vec<u8>, u32)> {
    let font = FontRef::from_index(&bytes, face.index).ok()?;
    if font.table_data(Tag::new(b"hvgl")).is_none() {
        return Some((bytes, face.index));
    }
    let postscript = font
        .localized_strings(StringId::POSTSCRIPT_NAME)
        .english_or_first()?
        .to_string();
    let native = NativeFont::open(face, &postscript)?;
    let converted = native.encode(font.maxp().ok()?.num_glyphs(), face.weight)?;
    Some((converted, 0))
}

/// A future OS may change the private format. Try the next installed family
/// when conversion fails, and report the font actually supplied to egui.
#[cfg(test)]
pub(super) fn definitions_with_fallback(report: FontReport) -> (FontReport, egui::FontDefinitions) {
    with_fallback(report, super::definitions)
}

fn with_fallback(
    mut report: super::FontReport,
    mut load: impl FnMut(&FontReport) -> egui::FontDefinitions,
) -> (super::FontReport, egui::FontDefinitions) {
    let mut definitions = load(&report);
    if definitions.font_data.contains_key("cjk-regular") || report.cjk_regular.is_none() {
        return (report, definitions);
    }
    let mut db = fontdb::Database::new();
    for path in super::candidate_paths() {
        let _ = db.load_font_file(path);
    }
    let mut scanned_system = false;
    let mut failed_paths = Vec::new();
    while let Some(failed) = &report.cjk_regular {
        tracing::warn!(family = %failed.family, "CJK font could not be loaded; trying the next family");
        failed_paths.push(failed.path.clone());
        remove_paths(&mut db, &failed_paths);
        report = super::pick_cjk(&db);
        if report.cjk_regular.is_none() && !scanned_system {
            db.load_system_fonts();
            scanned_system = true;
            remove_paths(&mut db, &failed_paths);
            report = super::pick_cjk(&db);
        }
        definitions = load(&report);
        if definitions.font_data.contains_key("cjk-regular") {
            break;
        }
    }
    (report, definitions)
}

fn remove_paths(db: &mut fontdb::Database, paths: &[std::path::PathBuf]) {
    let ids: Vec<_> = db
        .faces()
        .filter(|face| match &face.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                paths.contains(path)
            }
            fontdb::Source::Binary(_) => false,
        })
        .map(|face| face.id)
        .collect();
    for id in ids {
        db.remove_face(id);
    }
}

fn cached_definitions(
    report: &FontReport,
    cache: Option<&FontCache>,
    convert: bool,
) -> (egui::FontDefinitions, bool) {
    let mut deferred = false;
    let definitions = super::definitions_with(report, |face, geist| {
        if let Some(bytes) = cache.and_then(|cache| cache.read(face)) {
            return Some(super::face_data(bytes, 0, face, geist));
        }
        let bytes = std::fs::read(&face.path).ok()?;
        let font = FontRef::from_index(&bytes, face.index).ok()?;
        if font.table_data(Tag::new(b"hvgl")).is_none() {
            return Some(super::face_data(bytes, face.index, face, geist));
        }
        if !convert {
            deferred = true;
            return None;
        }
        // Re-read under the cache's metadata check so an OS font update during
        // conversion cannot publish a stale font under the updated source key.
        let encode = || renderable_face(std::fs::read(&face.path).ok()?, face).map(|f| f.0);
        let bytes = match cache {
            Some(cache) => cache.convert(face, encode),
            None => encode(),
        }?;
        Some(super::face_data(bytes, 0, face, geist))
    });
    (definitions, deferred)
}

pub(super) fn install(
    ctx: &egui::Context,
    report: FontReport,
    cache: Option<FontCache>,
) -> InstalledFonts {
    let started = std::time::Instant::now();
    if let Some(cache) = &cache {
        cache.prune();
    }
    let (ready, deferred) = cached_definitions(&report, cache.as_ref(), false);
    if !deferred && (ready.font_data.contains_key("cjk-regular") || report.cjk_regular.is_none()) {
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            "CJK fonts loaded without conversion"
        );
        return InstalledFonts::new(ctx, report, ready);
    }
    // Never convert on the UI thread, including while trying another family.
    // Discard partial hits so all weights switch to the preferred family together.
    let (fallback, definitions) = with_fallback(report.clone(), |candidate| {
        if candidate == &report {
            return super::bundled_definitions();
        }
        let (definitions, deferred) = cached_definitions(candidate, cache.as_ref(), false);
        if deferred {
            super::bundled_definitions()
        } else {
            definitions
        }
    });
    let mut installed = InstalledFonts::new(ctx, fallback.clone(), definitions);
    if deferred {
        installed.defer(ctx, move || {
            let started = std::time::Instant::now();
            let (loaded, definitions) = with_fallback(report, |candidate| {
                cached_definitions(candidate, cache.as_ref(), true).0
            });
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis(),
                "CJK font conversion finished"
            );
            (loaded != fallback).then_some((loaded, definitions))
        });
    }
    installed
}

#[cfg(test)]
#[path = "../../../tests/unit/font_macos.rs"]
mod tests;
