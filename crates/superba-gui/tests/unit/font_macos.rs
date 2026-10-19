use super::*;
use skrifa::instance::{LocationRef, Size};

#[test]
fn unreadable_candidate_falls_back_and_reports_the_loaded_family() {
    let missing = CjkFace {
        family: "PingFang UI SC".into(),
        path: "/fontmac-missing-font.ttc".into(),
        index: 0,
        weight: 400,
    };
    let (report, definitions) = definitions_with_fallback(super::super::FontReport {
        cjk_regular: Some(missing.clone()),
        ..Default::default()
    });
    if let Some(face) = report.cjk_regular {
        assert_ne!(face.path, missing.path);
        assert!(definitions.font_data.contains_key("cjk-regular"));
    } else {
        eprintln!("SKIP: no system CJK fallback installed");
    }
}

#[test]
fn hvgl_system_font_preserves_glyph_ids_advances_outlines_and_weight() {
    let path = super::super::candidate_paths()
        .into_iter()
        .find(|p| p.file_name().is_some_and(|n| n == "PingFangUI.ttc") && p.is_file());
    let Some(path) = path else {
        eprintln!("SKIP: PingFang UI not installed");
        return;
    };
    let mut db = fontdb::Database::new();
    db.load_font_file(path).unwrap();
    let report = super::super::pick_cjk(&db);
    let mut previous = None;
    for face in [report.cjk_regular, report.cjk_medium, report.cjk_bold]
        .into_iter()
        .flatten()
    {
        let bytes = std::fs::read(&face.path).unwrap();
        let font = FontRef::from_index(&bytes, face.index).unwrap();
        if font.table_data(Tag::new(b"hvgl")).is_none() {
            continue;
        }
        let name = font
            .localized_strings(StringId::POSTSCRIPT_NAME)
            .english_or_first()
            .unwrap()
            .to_string();
        let native = NativeFont::open(&face, &name).unwrap();
        let started = std::time::Instant::now();
        let encoded = native
            .encode(font.maxp().unwrap().num_glyphs(), face.weight)
            .unwrap();
        eprintln!(
            "CoreText {} weight {}: {} bytes in {:?}",
            name,
            face.weight,
            encoded.len(),
            started.elapsed()
        );
        assert_ne!(
            previous.as_ref(),
            Some(&encoded),
            "weight must change outlines"
        );
        let converted = FontRef::new(&encoded).unwrap();
        let metrics = converted.glyph_metrics(Size::unscaled(), LocationRef::default());
        for ch in "Superba 找到了这些 123永国中文田目，。简體繁體日本語한국어".chars()
        {
            let Some(id) = font.charmap().map(ch) else {
                continue;
            };
            assert_eq!(converted.charmap().map(ch), Some(id));
            let (glyph, advance) = native.glyph(id.to_u32() as u16).unwrap();
            assert_eq!(metrics.advance_width(id), Some(f32::from(advance)));
            if !glyph.contours.is_empty() {
                let b = metrics.bounds(id).unwrap();
                assert_eq!(
                    [b.x_min, b.y_min, b.x_max, b.y_max],
                    glyph.bounds().map(f32::from),
                    "{ch}"
                );
            }
        }
        previous = Some(encoded);
    }
}

#[test]
fn cold_install_renders_fallback_then_warm_install_uses_the_same_cached_outlines() {
    let Some(path) = super::super::candidate_paths()
        .into_iter()
        .find(|p| p.file_name().is_some_and(|n| n == "PingFangUI.ttc") && p.is_file())
    else {
        eprintln!("SKIP: PingFang UI not installed");
        return;
    };
    let mut db = fontdb::Database::new();
    db.load_font_file(path).unwrap();
    let preferred = super::super::pick_cjk(&db);
    let face = preferred.cjk_regular.as_ref().unwrap();
    let bytes = std::fs::read(&face.path).unwrap();
    if FontRef::from_index(&bytes, face.index)
        .unwrap()
        .table_data(Tag::new(b"hvgl"))
        .is_none()
    {
        eprintln!("SKIP: PingFang UI has standard outlines; no CoreText conversion needed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let cache = FontCache::new(temp.path().join("fonts"));
    let ctx = egui::Context::default();
    let started = std::time::Instant::now();
    let mut cold = install(&ctx, preferred.clone(), Some(cache.clone()));
    let cold_install = started.elapsed();
    assert_ne!(
        cold.report(),
        &preferred,
        "cold path returns before conversion"
    );
    assert!(
        cold.report().cjk_regular.is_some(),
        "system fallback remains available"
    );
    let frame = |ctx: &egui::Context| {
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("Superba 找到了这些 123");
            for ch in "找到了这些".chars() {
                assert!(ui.fonts_mut(|f| f.has_glyph(&egui::FontId::proportional(16.0), ch)));
            }
        })
        .drop_without_applying_deltas();
    };
    frame(&ctx);
    let cold_frame = started.elapsed();
    while cold.report() != &preferred {
        cold.poll(&ctx);
        assert!(started.elapsed() < std::time::Duration::from_secs(60));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    frame(&ctx);
    let converted = started.elapsed();
    for face in [
        &preferred.cjk_regular,
        &preferred.cjk_medium,
        &preferred.cjk_bold,
    ]
    .into_iter()
    .flatten()
    {
        assert!(cache.read(face).is_some());
    }
    let (warm_defs, deferred) = cached_definitions(&preferred, Some(&cache), false);
    assert!(!deferred);
    let started = std::time::Instant::now();
    let warm_ctx = egui::Context::default();
    let warm = install(&warm_ctx, preferred.clone(), Some(cache));
    let warm_install = started.elapsed();
    assert_eq!(warm.report(), &preferred);
    frame(&warm_ctx);
    let warm_frame = started.elapsed();
    ctx.fonts(|fonts| {
        for key in ["cjk-regular", "cjk-medium", "cjk-bold"] {
            assert_eq!(
                fonts.definitions().font_data.get(key),
                warm_defs.font_data.get(key)
            );
        }
    });
    eprintln!(
        "FONT_STARTUP cold_install_ms={:.3} cold_frame_ms={:.3} converted_ms={:.3} warm_install_ms={:.3} warm_frame_ms={:.3}",
        cold_install.as_secs_f64() * 1000.0,
        cold_frame.as_secs_f64() * 1000.0,
        converted.as_secs_f64() * 1000.0,
        warm_install.as_secs_f64() * 1000.0,
        warm_frame.as_secs_f64() * 1000.0
    );
}
