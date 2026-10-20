//! The installed coding tools in the sidebar (user request 114): hidden
//! until detection finds them, in a stable order, each opening its own page.

mod a11y_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use superba_core::Locale;
use superba_core::harness::HarnessId;
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use support::fake::{FakeOptions, FakeService};

fn fake(installed: &[HarnessId]) -> Arc<FakeService> {
    let f = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    f.installed_tools(installed);
    Arc::new(f)
}

/// The tool tabs in the sidebar, top to bottom.
fn tool_tabs(t: &a11y_harness::Tree, tr: &Tr) -> Vec<HarnessId> {
    let mut found: Vec<(f64, HarnessId)> = HarnessId::ALL
        .into_iter()
        .filter_map(|h| {
            let name = tr.t(Page::Tool(h).title_key());
            t.find_all(Some(Role::Tab), &name)
                .into_iter()
                .filter_map(|id| t.node(id).bounds())
                .find(|r| r.x1 <= f64::from(superba_gui::design::tokens::size::SIDEBAR))
                .map(|r| (r.y0, h))
        })
        .collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.into_iter().map(|(_, h)| h).collect()
}

#[test]
fn no_tool_installed_shows_no_tool_entry() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(fake(&[]), locale, Page::Overview);
        let t = h.until("sidebar", |t| t.find(Some(Role::Tab), &tr.t("nav.usage")).is_some());
        let t = { let _ = t; h.settle() };
        assert!(tool_tabs(&t, &tr).is_empty(), "{}", t.dump());
    }
}

#[test]
fn exactly_the_installed_tools_show_in_order_and_open_their_page() {
    let tr = Tr::new(Locale::ZhCn);
    // Given out of order: the sidebar keeps its own order.
    let installed = [HarnessId::Gemini, HarnessId::Dsh, HarnessId::Pi];
    let mut h = AppHarness::new(fake(&installed), Locale::ZhCn, Page::Overview);
    let t = h.until("tool tabs", |t| tool_tabs(t, &tr).len() == 3);
    assert_eq!(
        tool_tabs(&t, &tr),
        vec![HarnessId::Dsh, HarnessId::Pi, HarnessId::Gemini]
    );
    // Every entry has a name (its title).
    for hh in installed {
        assert!(t.find(Some(Role::Tab), &tr.t(Page::Tool(hh).title_key())).is_some());
    }
    // Clicking one opens that tool's page: its title and its detail.
    let t = h.click(Some(Role::Tab), &tr.t(Page::Tool(HarnessId::Dsh).title_key()));
    let t = { let _ = t; h.until("dsh page", |t| t.has(&tr.t("page.tool.subtitle"))) };
    assert!(t.has(&tr.t("page.tool.subtitle")), "{}", t.dump());
}

#[test]
fn all_six_fit_the_minimum_window_without_scrolling() {
    let tr = Tr::new(Locale::ZhCn);
    let mut h = AppHarness::new(fake(&HarnessId::ALL), Locale::ZhCn, Page::Overview);
    h.resize(superba_gui::design::tokens::size::MIN_WINDOW);
    let t = h.until("six tool tabs", |t| tool_tabs(t, &tr).len() == 6);
    let bottom = f64::from(superba_gui::design::tokens::size::MIN_WINDOW[1]);
    for p in Page::ALL {
        let name = tr.t(p.sidebar_page().title_key());
        let r = t
            .find_all(Some(Role::Tab), &name)
            .into_iter()
            .filter_map(|id| t.node(id).bounds())
            .find(|r| r.x1 <= f64::from(superba_gui::design::tokens::size::SIDEBAR))
            .unwrap_or_else(|| panic!("{name} missing: {}", t.dump()));
        assert!(r.y1 <= bottom, "{name} below the window: {r:?}");
    }
}
