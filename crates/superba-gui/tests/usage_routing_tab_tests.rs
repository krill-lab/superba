#![allow(missing_docs)]
//! The Usage page's 路由 / Routing tab (user bug 116): per routed request the
//! member and line that answered, every failover hop with its status and
//! time, the protocol conversion, the fallback to another provider, rows
//! recorded before the hops were kept, the router's local answers, and the
//! failover / failed filters with a clearable search.

mod router_harness;
mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use router_harness::Harness;
use superba_core::{
    Locale, ProfileId, RequestAttempt, RequestProtocol, RequestRow, RequestStatus, Timestamp,
};
use superba_gui::i18n::Tr;
use superba_gui::pages::router::backend::{BoxFut, RouterBackend, SystemRouter};
use superba_gui::pages::router::model::{Candidate, Live, Snapshot};
use superba_gui::pages::router::{self, State, Tab};
use support::fake::{FakeOptions, FakeService};

fn attempt(
    member: &str,
    line: &str,
    status: u16,
    outcome: &str,
    ms: u64,
    upstream: RequestProtocol,
) -> RequestAttempt {
    RequestAttempt {
        member_id: ProfileId::new(member),
        line_name: Some(line.into()),
        upstream_host: None,
        status: Some(status),
        outcome: outcome.into(),
        elapsed_ms: Some(ms),
        compression: None,
        upstream_protocol: Some(upstream),
    }
}

/// Four routed requests, newest first: a Claude Code request that failed over
/// twice (Krill 国内极速 429, Krill 海外线路 502, then My API) and was converted
/// from Anthropic Messages to OpenAI Responses; a native one; an old row
/// without hops; a request that failed.
fn rows(service: &FakeService) -> Vec<RequestRow> {
    let base = service.request_rows();
    let now = Timestamp::now();
    let mut template = base
        .into_iter()
        .find(|r| r.status == RequestStatus::Ok)
        .unwrap();
    template.failovers = 0;
    template.attempts.clear();
    let at = |s: u64| now.saturating_sub(Duration::from_secs(s));

    let mut multi = template.clone();
    multi.id = 9001;
    multi.started_at = at(10);
    multi.harness = superba_core::RequestHarness::ClaudeCode;
    multi.protocol = RequestProtocol::Messages;
    multi.model = Some("gpt-5.4".into());
    multi.member_id = ProfileId::new("my-api");
    multi.member_name = "My API".into();
    multi.line_name = Some("api.example.com".into());
    multi.failovers = 2;
    multi.attempts = vec![
        attempt(
            "krill",
            "国内极速",
            429,
            "HTTP 429",
            120,
            RequestProtocol::Responses,
        ),
        attempt(
            "krill",
            "海外线路",
            502,
            "HTTP 502",
            340,
            RequestProtocol::Responses,
        ),
        attempt(
            "my-api",
            "api.example.com",
            200,
            "answered",
            910,
            RequestProtocol::Responses,
        ),
    ];

    let mut native = template.clone();
    native.id = 9002;
    native.member_id = ProfileId::new("krill");
    native.member_name = "Krill".into();
    native.protocol = RequestProtocol::Responses;
    native.started_at = at(20);
    native.model = Some("gpt-5.3-native".into());
    native.attempts = vec![attempt(
        "krill",
        "海外线路",
        200,
        "answered",
        450,
        RequestProtocol::Responses,
    )];

    let mut old = template.clone();
    old.id = 9003;
    old.started_at = at(30);
    old.model = Some("gpt-old-row".into());

    let mut failed = template;
    failed.id = 9004;
    failed.started_at = at(40);
    failed.model = Some("gpt-failed".into());
    failed.status = RequestStatus::Http {
        status: 503,
        code: Some("router_unavailable".into()),
    };
    failed.http_status = Some(503);
    failed.attempts = vec![attempt(
        "krill",
        "国内极速",
        503,
        "HTTP 503",
        80,
        RequestProtocol::Responses,
    )];
    vec![multi, native, old, failed]
}

fn service() -> Arc<FakeService> {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    service.set_request_rows(rows(&service));
    service
}

/// The fake's snapshot with a live router that answered side calls itself.
struct LocalAnswers(SystemRouter);

impl RouterBackend for LocalAnswers {
    fn snapshot(&self) -> BoxFut<Snapshot> {
        let inner = self.0.snapshot();
        Box::pin(async move {
            let mut s = inner.await?;
            s.live = Ok(Live {
                listen: "127.0.0.1:18431".into(),
                version: "0.1.1".into(),
                local_answers: BTreeMap::from([
                    ("wham/usage".to_owned(), 7),
                    ("ps/plugins/installed".to_owned(), 3),
                ]),
                ..Live::default()
            });
            Ok(s)
        })
    }
    fn set_order(&self, ids: Vec<String>) -> BoxFut<()> {
        self.0.set_order(ids)
    }
    fn add_member(&self, candidate: Candidate) -> BoxFut<()> {
        self.0.add_member(candidate)
    }
    fn remove_member(&self, id: String) -> BoxFut<()> {
        self.0.remove_member(id)
    }
    fn install(&self) -> BoxFut<()> {
        self.0.install()
    }
    fn restart(&self) -> BoxFut<()> {
        self.0.restart()
    }
    fn uninstall(&self) -> BoxFut<()> {
        self.0.uninstall()
    }
}

#[test]
fn hops_list_every_line_tried_and_old_rows_still_show() {
    let s = service();
    let rows = rows(&s);
    let names = |id: &str| (id == "krill").then(|| "Krill".to_owned());
    let hops = router::routing::hops(&rows[0], &names);
    assert_eq!(hops.len(), 3);
    assert_eq!(hops[0].member, "Krill");
    assert_eq!(hops[0].status, Some(429));
    assert!(!hops[0].answered);
    assert_eq!(hops[2].member, "My API");
    assert!(hops[2].answered);
    assert!(router::routing::changed_member(&rows[0]));
    assert_eq!(rows[0].converted_to(), Some(RequestProtocol::Responses));
    assert!(!router::routing::changed_member(&rows[1]));
    assert_eq!(rows[1].converted_to(), None);
    // A row recorded before the hops were kept: the answering line alone.
    let old = router::routing::hops(&rows[2], &names);
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].line, rows[2].line_name);
}

#[test]
fn the_routing_tab_shows_hops_conversion_fallback_and_local_answers() {
    for (locale, size) in [
        (Locale::ZhCn, [1280.0, 800.0]),
        (Locale::En, [960.0, 640.0]),
    ] {
        let tr = Tr::new(locale);
        let mut h = Harness::new(size, locale, service(), true);
        let mut state = State::with_backend(Arc::new(LocalAnswers(SystemRouter::for_service(
            h.service.clone(),
        ))));
        state.tab = Tab::Routing;
        let f = h.until(
            |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
            |f| f.has("gpt-failed") && f.has(&tr.t("usage.routing.local")),
        );
        let text = f.all_text();
        assert!(f.has(&tr.t("usage.tab.routing")), "{text}");
        // Every hop, its status and time, in order.
        for needle in [
            "Krill · 国内极速",
            "429 · 120 ms",
            "Krill · 海外线路",
            "502 · 340 ms",
            "My API · api.example.com",
            "200 · 910 ms",
        ] {
            assert!(text.contains(needle), "{locale:?}: {needle}\n{text}");
        }
        let a = text.find("Krill · 国内极速").unwrap();
        let b = text.find("My API · api.example.com").unwrap();
        assert!(a < b, "hops in order");
        // The conversion and the fallback to another provider.
        let converted = tr.f(
            "usage.routing.converted",
            &[
                ("from", &tr.text(&RequestProtocol::Messages.text())),
                ("to", &tr.text(&RequestProtocol::Responses.text())),
            ],
        );
        assert!(f.has(&converted), "{text}");
        assert!(
            f.has(&tr.f("usage.routing.fallback", &[("n", &2)])),
            "{text}"
        );
        // The old row (no hops kept) and the native one are listed.
        assert!(f.has("gpt-old-row") && f.has("gpt-5.3-native"), "{text}");
        // The router's own answers, from the live status.
        assert!(text.contains("wham/usage 7"), "{text}");
        // No horizontal squeeze at either size; at 1280×800 the page fits (the
        // Usage page's minimum height is taller than a 640 pt window, where
        // the shell scrolls every Usage tab the same way).
        let beyond: Vec<&str> = f
            .texts
            .iter()
            .filter(|(_, r)| r.max.x > f.rect.max.x + 0.5)
            .map(|(t, _)| t.as_str())
            .collect();
        assert!(
            beyond.is_empty(),
            "{size:?} {locale:?}: past the edge {beyond:?}"
        );
        if size[1] >= 800.0 {
            assert!(f.fits(), "{size:?} {locale:?}: {text}");
        }
    }
}

#[test]
fn filters_and_search_narrow_the_list() {
    let tr = Tr::new(Locale::ZhCn);
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service(), true);
    let mut state = State::default();
    state.tab = Tab::Routing;
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("gpt-failed"),
    );
    // Failed over only.
    state.routing.filter = router::routing::Filter::Failover;
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| !f.has("gpt-failed") && f.has("My API · api.example.com"),
    );
    assert!(!f.has("gpt-5.3-native"), "{}", f.all_text());
    // Failed only.
    state.routing.filter = router::routing::Filter::Failed;
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("gpt-failed") && !f.has("gpt-5.3-native"),
    );
    assert!(!f.has("My API · api.example.com"), "{}", f.all_text());
    // Search, then clear it with its button.
    state.routing.filter = router::routing::Filter::All;
    state.search = "gpt-old-row".into();
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("gpt-old-row") && !f.has("gpt-failed"),
    );
    h.click_text(&f, &tr.t("requests.search_clear"));
    let f = h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("gpt-failed") && f.has("gpt-old-row"),
    );
    assert!(state.search.is_empty(), "{}", f.all_text());
}

#[test]
fn the_tab_is_reachable_by_command() {
    let service = service();
    let mut h = Harness::new([1280.0, 800.0], Locale::En, service, true);
    let mut state = State::default();
    h.until(
        |ui, cx, out| router::show_usage(ui, cx, &mut state, out),
        |f| f.has("Routing"),
    );
    assert_eq!(Tab::Routing.command(), "routing");
    assert!(Tab::ALL.contains(&Tab::Routing));
}
