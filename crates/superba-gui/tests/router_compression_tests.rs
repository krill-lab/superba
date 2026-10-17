#![allow(missing_docs)]
//! User request 14: the request detail's 通用 → 请求 card says how the request
//! body went upstream (the answering attempt's compression), in both
//! languages, and the cards still fit.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::Harness;
use superba_core::Locale;
use superba_gui::pages::router::{self, State};
use support::fake::{FakeOptions, FakeService};
use support::fit::SIZES;

#[test]
fn the_request_card_shows_how_the_body_went_upstream() {
    for (locale, label, value, last_tab) in [
        (Locale::ZhCn, "请求体", "zstd（路由器压缩）", "计时"),
        (
            Locale::En,
            "Request body",
            "zstd (compressed by the router)",
            "Timing",
        ),
    ] {
        let service = Arc::new(FakeService::new(FakeOptions {
            fast: true,
            ..Default::default()
        }));
        let mut h = Harness::new(SIZES[0], locale, service, true);
        let mut state = State::default();
        h.until(
            |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
            |f| f.has("Krill"),
        );
        state.recent.selected = Some(12);
        let f = h.until(
            |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
            |f| f.has(last_tab) && f.has(value),
        );
        assert!(f.has(label), "{locale:?}: {}", f.all_text());
        assert!(f.has(value), "{locale:?}: {}", f.all_text());
        assert!(f.fits(), "{locale:?}: the page no longer fits");
    }
}
