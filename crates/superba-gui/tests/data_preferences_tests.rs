mod router_harness;
mod support;

use router_harness::Harness;
use std::sync::Arc;
use superba_core::data::{DataCategoryUsage, DataLocation, DataUsage};
use superba_core::{Locale, Settings};
use superba_gui::{
    app::PageStates,
    i18n::Tr,
    pages::{Action, Page, SuperbaSection, data, superba},
};
use support::fake::{FakeOptions, FakeService};

#[test]
fn data_block_shows_separate_usage_retention_and_actions_in_both_languages() {
    for locale in Locale::ALL {
        let service = Arc::new(FakeService::new(FakeOptions::default()));
        let mut h = Harness::new([1280.0, 1800.0], locale, service.clone(), false);
        let mut settings = Settings::default();
        let mut state = data::State::default();
        let usage = DataUsage {
            location: DataLocation {
                directory: "/fixture/superba".into(),
                pointer: "/fixture/default/data-location.json".into(),
                codex_home: "/fixture/codex".into(),
            },
            categories: vec![DataCategoryUsage {
                category: "session_backups".into(),
                bytes: 2048,
                files: 1,
            }],
            total_bytes: 2048,
            codex_sessions_bytes: 8192,
            codex_home_bytes: 10240,
            codex_sqlite_bytes: 0,
            skipped: 0,
        };
        service.set_data_usage(Ok(usage.clone()));
        state.usage = Some(usage);
        let frame = h.frame(|ui, cx, out| {
            superba::data::data_location_section(ui, cx, &mut settings, &mut state, out)
        });
        for expected in [
            "/fixture/superba",
            "/fixture/codex",
            "2.0 KiB",
            "8.0 KiB",
            "10.0 KiB",
        ] {
            assert!(frame.has(expected), "{expected}: {}", frame.all_text());
        }
        let labels = if locale == Locale::ZhCn {
            [
                "迁移全部数据",
                "立即清理",
                "请求记录条数上限",
                "会话迁移备份",
            ]
        } else {
            [
                "Move all data",
                "Clean now",
                "Maximum request rows",
                "Session migration backups",
            ]
        };
        for label in labels {
            assert!(frame.has(label), "{label}: {}", frame.all_text());
        }
        assert!(
            !frame
                .actions
                .iter()
                .any(|a| matches!(a, Action::SaveSettings))
        );
    }
}

#[test]
fn directory_rejections_render_in_the_selected_language() {
    let service = Arc::new(FakeService::new(FakeOptions::default()));
    service.set_data_usage(Err(superba_core::Error::invalid("data.path.space")));
    let mut h = Harness::new([1280.0, 1800.0], Locale::ZhCn, service, false);
    let mut state = data::State::default();
    let mut settings = Settings::default();
    let frame = h.frame(|ui, cx, out| data::body_ui(ui, cx, &mut settings, &mut state, out));
    assert!(frame.has("可用空间不足"), "{}", frame.all_text());
}

#[test]
fn superba_data_page_has_one_location_block_and_keeps_backups_reachable() {
    for locale in Locale::ALL {
        let tr = Tr::new(locale);
        for dark in [false, true] {
            for size in [[960.0, 640.0], [1280.0, 800.0]] {
                let service = Arc::new(FakeService::new(FakeOptions::default()));
                let page = Page::Superba(SuperbaSection::Data);
                let mut h = Harness::new(size, locale, service, true).min_height(page.min_height());
                if dark {
                    h = h.dark();
                }
                let mut pages = PageStates::default();
                let mut settings = Settings::default();
                let frame = h.settle(|ui, cx, out| pages.show(page, ui, cx, &mut settings, out));
                for key in ["prefs.data.dir", "prefs.data.move"] {
                    assert_eq!(
                        frame
                            .texts
                            .iter()
                            .filter(|(text, _)| text == &tr.t(key))
                            .count(),
                        1,
                        "{locale:?} {dark} {size:?} {key}: {}",
                        frame.all_text()
                    );
                }
                assert!(!frame.has(&tr.t("prefs.data.retention_moved")));
                let mut frames = vec![frame];
                for _ in 0..12 {
                    let rect = frames[0].rect;
                    h.scroll_at(
                        egui::pos2(rect.right() - 40.0, rect.center().y),
                        egui::vec2(0.0, -240.0),
                    );
                    frames
                        .push(h.settle(|ui, cx, out| pages.show(page, ui, cx, &mut settings, out)));
                    if frames
                        .last()
                        .unwrap()
                        .has(&tr.t("settings.uninstall.title"))
                    {
                        break;
                    }
                }
                for key in [
                    "prefs.data.requests",
                    "prefs.data.clean",
                    "superba.backups.title",
                    "prefs.restore.title",
                    "settings.uninstall.title",
                ] {
                    assert!(
                        frames.iter().any(|f| f.has(&tr.t(key))),
                        "{locale:?} {dark} {size:?}: missing {key}"
                    );
                }
                for frame in frames {
                    assert!(frame.fits() && !frame.scrolled, "{size:?}: {frame:?}");
                    for (text, rect) in &frame.texts {
                        assert!(
                            rect.right() <= frame.rect.right() + 1.0,
                            "{size:?}: {text} overflows at {rect:?}"
                        );
                    }
                }
            }
        }
    }
}
