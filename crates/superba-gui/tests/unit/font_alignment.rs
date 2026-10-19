use super::*;
use crate::design::fonts::{self, CjkFace, Weight};

fn metrics() -> Metrics {
    Metrics {
        em: 1000.0,
        ascent: 900.0,
        descent: -200.0,
        leading: 0.0,
        cap_height: Some(720.0),
        x_height: Some(520.0),
        ink_bottom: 0.0,
        ink_top: 720.0,
    }
}

#[test]
fn metric_formula_matches_ink_size_and_center_with_different_line_boxes() {
    let latin = metrics();
    let cjk = Metrics {
        em: 2000.0,
        ascent: 2200.0,
        descent: -600.0,
        leading: 200.0,
        ink_bottom: -160.0,
        ink_top: 1640.0,
        ..latin
    };
    let tweak = from_metrics(cjk, latin).unwrap();
    assert!((tweak.scale - 0.96).abs() < 1e-6);
    assert!((tweak.y_offset_factor - 0.009_583_333).abs() < 1e-6);
    let cjk_height = (cjk.ink_top - cjk.ink_bottom) / cjk.em * tweak.scale;
    assert!((cjk_height - 1.2 * 0.72).abs() < 1e-6);
    assert!(((cjk.center() + tweak.y_offset_factor) * tweak.scale - latin.center()).abs() < 1e-6);
    // The formula is independent of the design-units convention of the font.
    let doubled = Metrics {
        em: cjk.em * 2.0,
        ascent: cjk.ascent * 2.0,
        descent: cjk.descent * 2.0,
        leading: cjk.leading * 2.0,
        ink_top: cjk.ink_top * 2.0,
        ink_bottom: cjk.ink_bottom * 2.0,
        ..cjk
    };
    assert_eq!(from_metrics(doubled, latin).unwrap(), tweak);
}

#[test]
fn missing_cap_height_uses_x_height_then_measured_capitals() {
    let latin = metrics();
    let cjk = Metrics {
        ink_bottom: -80.0,
        ink_top: 820.0,
        ..latin
    };
    for (cap_height, x_height, expected) in [
        (None, Some(500.0), 0.7),
        (Some(0.0), Some(500.0), 0.7),
        (None, None, 0.72),
        (Some(f32::NAN), Some(-1.0), 0.72),
    ] {
        let tweak = from_metrics(
            cjk,
            Metrics {
                cap_height,
                x_height,
                ..latin
            },
        )
        .unwrap();
        assert!((tweak.scale * 0.9 - 1.2 * expected).abs() < 1e-6);
    }
}

#[test]
fn invalid_metrics_and_unreadable_collections_use_family_fallbacks() {
    for bad in [
        Metrics {
            em: 0.0,
            ..metrics()
        },
        Metrics {
            ascent: f32::NAN,
            ..metrics()
        },
        Metrics {
            descent: 2000.0,
            ..metrics()
        },
        Metrics {
            ink_top: 0.0,
            ..metrics()
        },
        Metrics {
            ink_top: 1.0,
            ..metrics()
        },
    ] {
        assert!(from_metrics(bad, metrics()).is_none());
    }
    for family in [
        "PingFang SC",
        "PingFang UI SC",
        "Hiragino Sans GB",
        "STHeiti",
        "Microsoft YaHei UI",
        "Noto Sans CJK SC",
        "Source Han Sans SC",
        "WenQuanYi Zen Hei",
    ] {
        assert_eq!(
            cjk_tweak(b"bad font", 7, 400, family, fonts::GEIST_REGULAR),
            fallback(family)
        );
        assert_ne!(fallback(family), FontTweak::default());
    }
    assert_ne!(fallback("Hiragino Sans GB"), fallback("PingFang SC"));
}

#[test]
fn available_cjk_faces_align_with_geist_in_an_egui_mixed_row() {
    let mut db = fontdb::Database::new();
    for path in fonts::candidate_paths() {
        if path.is_file() {
            db.load_font_file(path).unwrap();
        }
    }
    db.load_system_fonts();
    let mut checked = 0;
    for family in fonts::CJK_FAMILIES {
        let report = fonts::pick_cjk_families(&db, &[family]);
        if report.cjk_regular.is_none() {
            eprintln!("SKIP {family}: not installed");
            continue;
        }
        let definitions = fonts::definitions(&report);
        for (weight, face) in [
            (Weight::Regular, report.cjk_regular.as_ref()),
            (Weight::Medium, report.cjk_medium.as_ref()),
            (Weight::Semibold, report.cjk_bold.as_ref()),
        ] {
            let Some(face) = face else { continue };
            let bytes = std::fs::read(&face.path).unwrap();
            if let Some((min, max)) = weight_range(&bytes, face.index) {
                let wanted: u16 = match weight {
                    Weight::Regular => 400,
                    Weight::Medium => 500,
                    Weight::Semibold => 700,
                };
                assert_eq!(face.weight, wanted.clamp(min, max));
                assert!(report.cjk_medium.is_some());
                assert!(report.cjk_bold.is_some());
            }
            check_row(&definitions, face, weight);
            checked += 1;
        }
    }
    eprintln!("Checked {checked} installed CJK faces (missing families skipped)");
}

fn check_row(definitions: &egui::FontDefinitions, face: &CjkFace, weight: Weight) {
    let ctx = egui::Context::default();
    let name = match weight {
        Weight::Regular => "cjk-regular",
        Weight::Medium => "cjk-medium",
        Weight::Semibold => "cjk-bold",
    };
    let data = &definitions.font_data[name];
    assert!(
        Metrics::read(data.bytes(), data.index, face.weight, "永国中文田目").is_some(),
        "{face:?}"
    );
    let tweak = &data.tweak;
    if weight_range(data.bytes(), data.index).is_some() {
        assert_eq!(
            tweak.coords.as_ref(),
            &[(
                egui::epaint::text::Tag::new(b"wght"),
                f32::from(face.weight)
            )]
        );
    }
    eprintln!(
        "{} {weight:?}: scale={:.4} offset={:.4}",
        face.family, tweak.scale, tweak.y_offset_factor
    );
    ctx.set_fonts(definitions.clone());
    // At high raster resolution, the 3% limit measures typography rather than
    // a one-pixel hinting difference. The actual glyph quads still come from
    // egui's shaping, layout, and rasterization, not from the calibration code.
    ctx.set_pixels_per_point(8.0);
    ctx.run_ui(egui::RawInput::default(), |_| {})
        .drop_without_applying_deltas();
    for size in [14.0, 18.0, 24.0, 32.0] {
        let galley = ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                "Superba 找到了这些 123".into(),
                fonts::font(size, weight),
                egui::Color32::WHITE,
            )
        });
        let row = &galley.rows[0];
        let mut cjk = egui::Rect::NOTHING;
        let mut latin = egui::Rect::NOTHING;
        for glyph in &row.glyphs {
            let bounds =
                egui::Rect::from_min_size(glyph.pos + glyph.uv_rect.offset, glyph.uv_rect.size);
            if fonts::is_cjk(glyph.chr) {
                cjk = cjk.union(bounds);
            } else if glyph.chr.is_ascii_uppercase() || glyph.chr.is_ascii_digit() {
                // Compare against capitals/digits: lowercase p has a descender
                // and cannot share their visual center by design.
                latin = latin.union(bounds);
            }
        }
        let delta = (cjk.center().y - latin.center().y).abs() / size;
        let ratio = cjk.height() / latin.height();
        eprintln!(
            "{} index={} {weight:?} {size}pt: center={delta:.4} height={ratio:.3}",
            face.family, face.index
        );
        assert!(
            delta <= 0.03,
            "{face:?} {size}pt: centers differ by {delta:.4}em"
        );
        assert!(
            (1.05..=1.35).contains(&ratio),
            "{face:?} {size}pt: height ratio {ratio:.3}"
        );
    }
}

#[test]
fn pingfang_ui_collection_is_selected_by_weight_and_named_in_report() {
    let mut db = fontdb::Database::new();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/Geist-Regular.ttf");
    db.load_font_file(path).unwrap();
    let template = db.faces().next().unwrap().clone();
    for (index, weight) in [(5, 700), (2, 400), (8, 500)] {
        let mut face = template.clone();
        face.families = vec![(
            ".PingFang SC".into(),
            fontdb::Language::English_UnitedStates,
        )];
        face.source = fontdb::Source::File("/system/fonts/PingFangUI.ttc".into());
        face.index = index;
        face.weight = fontdb::Weight(weight);
        db.push_face_info(face);
    }
    let report = fonts::pick_cjk(&db);
    assert_eq!(
        report.cjk_regular.as_ref().unwrap().family,
        "PingFang UI SC"
    );
    assert_eq!(report.cjk_regular.as_ref().unwrap().index, 2);
    assert_eq!(report.cjk_medium.as_ref().unwrap().index, 8);
    assert_eq!(report.cjk_bold.as_ref().unwrap().index, 5);
}

#[cfg(target_os = "macos")]
#[test]
fn pingfang_ui_precedes_hiragino_in_macos_candidates() {
    let paths = fonts::candidate_paths();
    let position = |file: &str| {
        paths
            .iter()
            .position(|p| p.file_name().is_some_and(|n| n == file))
            .unwrap()
    };
    assert!(position("PingFang.ttc") < position("PingFangUI.ttc"));
    assert!(position("PingFangUI.ttc") < position("Hiragino Sans GB.ttc"));
}
