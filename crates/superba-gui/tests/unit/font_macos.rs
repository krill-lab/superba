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
