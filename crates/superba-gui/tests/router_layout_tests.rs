#![allow(missing_docs)]
//! Router priority and the collapsed advanced settings at the two review sizes.

mod a11y_harness;
mod router_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use router_harness::Harness;
use superba_core::{Locale, ProfileId, RequestProtocol as P, RouterConversion};
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use superba_gui::pages::router::backend::{BoxFut, RouterBackend, SystemRouter};
use superba_gui::pages::router::model::{Candidate, Snapshot};
use superba_gui::pages::router::{self, State};
use support::fake::{FakeOptions, FakeService};

fn members() -> Arc<FakeService> {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    service.edit_profiles(|set| {
        let mut backup = set.profile(&ProfileId::new("krill")).unwrap().clone();
        backup.id = ProfileId::new("backup");
        backup.name = "Backup provider".into();
        set.profiles.push(backup);
        set.settings.router.members = vec![ProfileId::new("krill"), ProfileId::new("backup")];
    });
    service
}

#[test]
fn both_members_and_line_priority_fit_before_advanced_at_every_review_size() {
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            for dark in [false, true] {
                let mut h = Harness::new(size, locale, members(), true)
                    .min_height(Page::Router.min_height());
                if dark {
                    h = h.dark();
                }
                let mut state = State::default();
                let f = h.until(
                    |ui, cx, out| router::show(ui, cx, &mut state, out),
                    |f| f.has("Backup provider"),
                );
                assert!(f.fits() && !f.scrolled, "{size:?}: {}", f.all_text());
                assert_eq!(f.scroll_areas, 1);
                assert!(
                    !f.texts
                        .iter()
                        .any(|(text, _)| text == &tr.t("stats_line.title"))
                );
                for name in [
                    tr.t("router.members.title"),
                    "Krill".into(),
                    "Backup provider".into(),
                    tr.t("router.members.primary"),
                    tr.f("router.members.fallback", &[("n", &1)]),
                ] {
                    assert!(
                        f.clipped_texts
                            .iter()
                            .any(|(text, rect, clip)| text == &name
                                && clip.contains_rect(*rect)
                                && f.rect.contains_rect(*rect)),
                        "{size:?} {locale:?} dark={dark}: {name} clipped\n{}",
                        f.all_text()
                    );
                }
                // Both line sequences follow their member header, without entering a second column.
                let first = f.find("Krill").unwrap();
                let backup = f.find("Backup provider").unwrap();
                assert!((first.min.x - backup.min.x).abs() < 1.0);
                assert!(first.max.y < backup.min.y);
                let preferred = tr.f("router.preference.first", &[("line", &"")]);
                assert!(
                    f.texts
                        .iter()
                        .filter(|(text, _)| text.contains(&preferred))
                        .count()
                        >= 2,
                    "both members show their preferred line: {}",
                    f.all_text()
                );
            }
        }
    }
}

/// ui-spec rule 5: one primary button per view. The header's 开启路由… is the
/// primary action only once a member can carry traffic; until then the
/// members section's first "add … as a member" is, and 关闭路由… never is.
#[test]
fn one_primary_action_in_every_router_state() {
    use superba_gui::design::widgets::ButtonKind;
    use superba_gui::pages::router::eligibility::primary_fix;
    use superba_switch::router_doctor::router_candidates;

    assert_eq!(router::switch_kind(false, true), ButtonKind::Primary);
    assert_eq!(router::switch_kind(false, false), ButtonKind::Secondary);
    assert_eq!(router::switch_kind(true, true), ButtonKind::Secondary);
    assert_eq!(router::switch_kind(true, false), ButtonKind::Secondary);

    // Krill with a key (addable) after a provider without one: Krill's button
    // is the primary fix, the key fix stays secondary.
    let service = members();
    service.edit_profiles(|set| {
        set.settings.router.members.clear();
        let mut keyless = set.profile(&ProfileId::new("krill")).unwrap().clone();
        keyless.id = ProfileId::new("plain");
        keyless.name = "Plain".into();
        keyless.key_ref = None;
        set.profiles.insert(0, keyless);
    });
    let mut set = None;
    service.edit_profiles(|s| set = Some(s.clone()));
    let sites = router_harness::data_from(&service).sites;
    let all = router_candidates(&set.unwrap(), &sites);
    let fixable: Vec<_> = all
        .iter()
        .filter(|c| c.addable() || c.eligibility.needs_key())
        .collect();
    let primary = primary_fix(&fixable).expect("an addable provider");
    assert!(fixable[primary].addable());
    assert!(fixable[..primary].iter().all(|c| !c.addable()));
    assert_eq!(primary_fix(&[]), None);
}

#[test]
fn the_conversion_matrix_has_the_same_protocols_down_and_across() {
    use superba_core::{RequestProtocol as P, RouterConversion};
    let c = |from, to| RouterConversion {
        from,
        to,
        enabled: true,
    };
    // Listed in the router's order (client Messages first), shown in one order.
    let conversions = [
        c(P::Messages, P::Responses),
        c(P::Gemini, P::ChatCompletions),
        c(P::Responses, P::Messages),
    ];
    assert_eq!(
        router::ingress::matrix_axes(&conversions),
        [P::Responses, P::ChatCompletions, P::Messages, P::Gemini]
    );
    assert_eq!(
        router::ingress::matrix_axes(&[c(P::Messages, P::Responses)]),
        [P::Responses, P::Messages]
    );
}

/// The whole body scrolls under the header at 960×640: no fixed status card or
/// eligibility panel above it (they took half the window), the fix for "no
/// member" is in view, every section title is drawn once (no column header
/// repeats 入口), and the entry table's counts end under their header.
#[test]
fn the_off_page_guides_to_a_member_in_one_scrolling_column_at_both_sizes() {
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            for dark in [false, true] {
                let service = Arc::new(FakeService::new(FakeOptions {
                    fast: true,
                    ..Default::default()
                }));
                let mut h =
                    Harness::new(size, locale, service, true).min_height(Page::Router.min_height());
                if dark {
                    h = h.dark();
                }
                let mut state = State::default();
                let add = tr.f("router.eligibility.add", &[("name", &"Krill")]);
                let count = tr.t("router.ingress.col.recent");
                let f = h.until(
                    |ui, cx, out| router::show(ui, cx, &mut state, out),
                    |f| f.has(&add) && f.has(&count) && f.has("0"),
                );
                let what = format!("{size:?} {locale:?} dark={dark}");
                assert_eq!(f.scroll_areas, 1, "{what}");
                assert!(f.fits() && !f.scrolled, "{what}");
                for text in [
                    tr.t("router.banner.turn_on"),
                    tr.t("router.eligibility.none"),
                    add.clone(),
                    tr.t("router.ingress.title"),
                ] {
                    assert!(
                        f.clipped_texts.iter().any(|(t, rect, clip)| t == &text
                            && clip.contains_rect(*rect)
                            && f.rect.contains_rect(*rect)),
                        "{what}: {text} not in view\n{}",
                        f.all_text()
                    );
                }
                for title in [
                    tr.t("router.members.title"),
                    tr.t("router.ingress.title"),
                    tr.t(Page::Router.title_key()),
                ] {
                    assert_eq!(
                        f.texts.iter().filter(|(t, _)| t == &title).count(),
                        1,
                        "{what}: {title} drawn more than once"
                    );
                }
                // In reading order: the members section says why, offers the
                // fix, and only then come the entry points.
                let order = [
                    tr.t("router.members.title"),
                    tr.t("router.eligibility.none"),
                    add.clone(),
                    tr.t("router.ingress.title"),
                ]
                .map(|t| f.find(&t).unwrap().min.y);
                assert!(order.is_sorted(), "{what}: {order:?}");
                // The state is said once: "路由已关闭 —— …", not a second label.
                assert_eq!(
                    f.texts
                        .iter()
                        .filter(|(t, _)| t.contains(&tr.t("router.banner.inactive")))
                        .count(),
                    1,
                    "{what}\n{}",
                    f.all_text()
                );
                let header = f.find(&count).unwrap();
                let numbers: Vec<_> = f
                    .texts
                    .iter()
                    .filter(|(t, r)| {
                        t.chars().all(|c| c.is_ascii_digit() || c == ',')
                            && r.min.y > header.max.y
                            && r.max.x > header.min.x
                    })
                    .collect();
                assert_eq!(numbers.len(), 4, "{what}: {numbers:?}");
                for (n, r) in numbers {
                    assert!(
                        (r.max.x - header.max.x).abs() < 0.5,
                        "{what}: {n} ends at {} under a header ending at {}",
                        r.max.x,
                        header.max.x
                    );
                }
            }
        }
    }
}

/// Nothing in the page's column, Advanced open, reaches past its right edge
/// into the scroll bar's gutter (the reply preview and a section's wrapped
/// caption made the column wider, and the switches and 查看… beside them
/// went under the scroll bar at 960×640).
#[test]
fn advanced_settings_stay_inside_the_column_at_both_sizes() {
    use superba_gui::design::tokens::focus::BLEED;
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let mut h =
                Harness::new(size, locale, members(), true).min_height(Page::Router.min_height());
            let mut state = State::default();
            // 「高级」 is a plain section (user bug 116): scroll from the top
            // to the end of the page.
            h.until(
                |ui, cx, out| router::show(ui, cx, &mut state, out),
                |f| f.has("Backup provider"),
            );
            let end = tr.t("recording.title");
            let mut seen_end = false;
            for _ in 0..30 {
                let f = h.settle(|ui, cx, out| router::show(ui, cx, &mut state, out));
                // The column is clipped to its rectangle plus the focus bleed
                // (table cells to their cell); the page header's controls to
                // the page. Its right edge: the widest clip inside the page.
                let inside = |clip: &egui::Rect| clip.right() < f.rect.right() - 0.5;
                let column = f
                    .clipped_texts
                    .iter()
                    .map(|(_, _, clip)| *clip)
                    .chain(f.outlines.iter().map(|(_, clip)| *clip))
                    .filter(inside)
                    .map(|clip| clip.right())
                    .fold(f32::MIN, f32::max)
                    - BLEED;
                for (rect, clip) in &f.outlines {
                    if inside(clip) && rect.intersects(*clip) {
                        assert!(
                            rect.right() <= column + 0.5,
                            "{size:?} {locale:?}: a control at {rect:?} past the column ({column})"
                        );
                    }
                }
                for (text, rect, clip) in &f.clipped_texts {
                    if inside(clip) && rect.intersects(*clip) {
                        assert!(
                            rect.right() <= column + 0.5,
                            "{size:?} {locale:?}: {text:?} at {rect:?} past the column ({column})"
                        );
                    }
                }
                seen_end |= f
                    .clipped_texts
                    .iter()
                    .any(|(t, r, clip)| t == &end && clip.contains_rect(*r));
                if seen_end {
                    break;
                }
                h.scroll_at(
                    egui::pos2(f.rect.min.x + 100.0, f.rect.center().y),
                    egui::vec2(0.0, -200.0),
                );
            }
            assert!(seen_end, "{size:?} {locale:?}: scrolled to the end");
        }
    }
}

/// The router on with both members: Codex's config points at the local router.
fn router_on(h: &mut Harness) {
    let endpoint: url::Url = "http://127.0.0.1:18431/backend-api/codex".parse().unwrap();
    h.data
        .profiles
        .value
        .as_mut()
        .unwrap()
        .settings
        .router
        .enabled = true;
    let overview = h.data.overview.value.as_mut().unwrap();
    overview.router_base_url = Some(endpoint.clone());
    overview.config.as_mut().unwrap().openai_base_url = Some(endpoint.to_string());
}

/// The two states the page is reviewed in: `off` (no member, the router off;
/// the page guides to a member) and `on` (two members, the router on).
const STATES: [&str; 2] = ["off", "on"];

/// A configured router (the `on` state): a port, so the entry points show
/// their addresses, the token checked, and every conversion direction
/// between the four protocols, so the conversion matrix is drawn.
struct Configured(SystemRouter);

impl RouterBackend for Configured {
    fn snapshot(&self) -> BoxFut<Snapshot> {
        let inner = self.0.snapshot();
        Box::pin(async move {
            let mut s = inner.await?;
            let all = [P::Responses, P::ChatCompletions, P::Messages, P::Gemini];
            s.port = Some(18431);
            s.token_ready = true;
            s.translation = Some(
                all.iter()
                    .flat_map(|&from| {
                        all.iter()
                            .filter(move |&&to| to != from)
                            .map(move |&to| RouterConversion {
                                from,
                                to,
                                enabled: to != P::Gemini,
                            })
                    })
                    .collect(),
            );
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

/// The page's state: in the `on` state with the configured router.
fn page_state(state: &str, h: &Harness) -> State {
    if state == "on" {
        State::with_backend(Arc::new(Configured(SystemRouter::for_service(
            h.service.clone(),
        ))))
    } else {
        State::default()
    }
}

fn harness(state: &str, size: [f32; 2], locale: Locale, dark: bool) -> Harness {
    let service = if state == "on" {
        members()
    } else {
        Arc::new(FakeService::new(FakeOptions {
            fast: true,
            ..Default::default()
        }))
    };
    let mut h = Harness::new(size, locale, service, true).min_height(Page::Router.min_height());
    if dark {
        h = h.dark();
    }
    if state == "on" {
        router_on(&mut h);
    }
    h
}

/// The page has loaded what it shows in `state`: the router's snapshot (the
/// second member; or the way to the first and, in view under it, the entry
/// points' counts).
fn loaded(f: &router_harness::Frame, tr: &Tr, state: &str) -> bool {
    if state == "on" {
        f.has("Backup provider")
    } else {
        f.has(&tr.f("router.eligibility.add", &[("name", &"Krill")]))
            && f.texts.iter().any(|(t, _)| t == "0")
    }
}

/// ui-spec rule 7 on one frame: every text in view is whole sideways (long
/// values are shortened with "…", never cut by their clip), inside the page,
/// and overlaps no other text.
fn assert_tidy(f: &router_harness::Frame, what: &str) {
    let mut shown = Vec::new();
    for (text, rect, clip) in &f.clipped_texts {
        let visible = rect.intersect(*clip);
        if text.trim().is_empty() || visible.width() <= 0.5 || visible.height() <= 0.5 {
            continue;
        }
        assert!(
            rect.min.x >= clip.min.x - 0.5 && rect.max.x <= clip.max.x + 0.5,
            "{what}: {text:?} at {rect:?} is cut sideways by its clip {clip:?}"
        );
        assert!(
            rect.min.x >= f.rect.min.x - 0.5 && rect.max.x <= f.rect.max.x + 0.5,
            "{what}: {text:?} at {rect:?} outside the page {:?}",
            f.rect
        );
        shown.push((text, visible));
    }
    for (i, (a, ra)) in shown.iter().enumerate() {
        for (b, rb) in &shown[i + 1..] {
            let o = ra.intersect(*rb);
            assert!(
                o.width() <= 1.0 || o.height() <= 1.0,
                "{what}: {a:?} at {ra:?} overlaps {b:?} at {rb:?}"
            );
        }
    }
}

/// Every expander of the page open at once — 「高级」, the conversion
/// matrix, the failover rules and each member's line details — at the two
/// review widths, in a window tall enough to show the whole page: no text is
/// cut, sticks out of the page or overlaps another, in either state, theme
/// or language.
#[test]
fn with_everything_open_no_text_is_cut_or_overlaps_at_both_widths() {
    for width in [1280.0, 960.0] {
        for state in STATES {
            for locale in [Locale::En, Locale::ZhCn] {
                let tr = Tr::new(locale);
                for dark in [false, true] {
                    let what = format!("{width} {state} {locale:?} dark={dark}");
                    let mut h = harness(state, [width, 4000.0], locale, dark);
                    let mut page = page_state(state, &h);
                    let f = h.until(
                        |ui, cx, out| router::show(ui, cx, &mut page, out),
                        |f| loaded(f, &tr, state),
                    );
                    assert_tidy(&f, &format!("{what}, closed"));
                    // The only expanders left are each member's line details
                    // (「高级」, 「协议转换」 and the failover rules are plain
                    // sections since user bugs 116 and 117). From the bottom up.
                    let details = tr.t("router.members.details");
                    let mut hits: Vec<egui::Rect> = f
                        .texts
                        .iter()
                        .filter(|(t, _)| t == &details)
                        .map(|(_, r)| *r)
                        .collect();
                    hits.sort_by(|a, b| b.min.y.total_cmp(&a.min.y));
                    assert!(
                        hits.len() == if state == "on" { 2 } else { 0 },
                        "{what}: expanders {hits:?}\n{}",
                        f.all_text()
                    );
                    for r in hits {
                        h.click_at(r.center());
                    }
                    // The clicks, and the bodies done opening.
                    let f = h.frames(30, |ui, cx, out| router::show(ui, cx, &mut page, out));
                    for shown in [
                        tr.t("router.service_label"),
                        tr.t("recording.title"),
                        tr.t("prices.title"),
                        tr.t("router.failover.title"),
                        tr.t("stats_line.title"),
                    ] {
                        assert!(f.has(&shown), "{what}: {shown} not shown\n{}", f.all_text());
                    }
                    if state == "on" {
                        for open in [
                            tr.t("router.line.col.line"),
                            tr.t("router.ingress.col.client_protocol"),
                            "http://127.0.0.1:18431/backend-api/codex".to_owned(),
                        ] {
                            assert!(f.has(&open), "{what}: {open} not shown\n{}", f.all_text());
                        }
                    }
                    assert_tidy(&f, &format!("{what}, open"));
                }
            }
        }
    }
}

/// At the two review sizes, the page in one scrolling column: tidy (rule 7)
/// at the top and on every screen down to the end of 「高级」.
#[test]
fn every_screen_down_to_the_service_is_tidy_at_both_sizes() {
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for state in STATES {
            for locale in [Locale::En, Locale::ZhCn] {
                let tr = Tr::new(locale);
                let what = format!("{size:?} {state} {locale:?}");
                let mut h = harness(state, size, locale, state == "on");
                let mut page = page_state(state, &h);
                let f = h.until(
                    |ui, cx, out| router::show(ui, cx, &mut page, out),
                    |f| loaded(f, &tr, state),
                );
                assert!(f.fits() && !f.scrolled, "{what}");
                assert_eq!(f.scroll_areas, 1, "{what}");
                assert_tidy(&f, &what);
                let mut last = String::new();
                let mut screens = 0;
                for _ in 0..40 {
                    let f = h.settle(|ui, cx, out| router::show(ui, cx, &mut page, out));
                    assert_tidy(&f, &format!("{what}, screen {screens}"));
                    // The end: scrolling no longer moves anything.
                    let now = format!("{:?}", f.texts);
                    if now == last {
                        break;
                    }
                    last = now;
                    screens += 1;
                    h.scroll_at(
                        egui::pos2(f.rect.min.x + 100.0, f.rect.center().y),
                        egui::vec2(0.0, -160.0),
                    );
                }
                assert!(
                    last.contains(&format!("{:?}", tr.t("recording.title"))),
                    "{what}: the end of the page was not reached"
                );
                assert!(screens > 2, "{what}: {screens} screens");
            }
        }
    }
}

/// 「高级」 is a plain section like 「入口」 (user bug 116, overriding ui-spec
/// rule 6 for this page): scrolled to the end, its title with the caption
/// under it, then the reply stats line, the prices and the recording; the
/// service is not down here any more (it is under the status line).
#[test]
fn advanced_is_a_plain_section_at_the_end_in_both_states() {
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for state in STATES {
            for locale in [Locale::En, Locale::ZhCn] {
                let tr = Tr::new(locale);
                let what = format!("{size:?} {state} {locale:?}");
                let mut h = harness(state, size, locale, false);
                let mut page = page_state(state, &h);
                h.until(
                    |ui, cx, out| router::show(ui, cx, &mut page, out),
                    |f| loaded(f, &tr, state),
                );
                let title = tr.t("router.advanced.title");
                let f = h.until_scrolled(
                    |ui, cx, out| router::show(ui, cx, &mut page, out),
                    |f| {
                        f.clipped_texts
                            .iter()
                            .any(|(t, r, clip)| t == &title && clip.contains_rect(*r))
                    },
                );
                let line = f.find(&title).unwrap();
                let caption = f
                    .find(&tr.t("router.advanced.caption"))
                    .unwrap_or_else(|| panic!("{what}: no caption\n{}", f.all_text()));
                assert!(caption.min.y >= line.max.y - 1.0, "{what}");
                // No disclosure: its settings follow when scrolling on.
                for below in [
                    tr.t("stats_line.title"),
                    tr.t("prices.title"),
                    tr.t("recording.title"),
                ] {
                    h.until_scrolled(
                        |ui, cx, out| router::show(ui, cx, &mut page, out),
                        |f| f.has(&below),
                    );
                }
            }
        }
    }
}

/// The off state (no member, the router off) for screen readers: the
/// header's 开启路由… is there under its existing name but disabled, the
/// first "add … as a member" is the enabled way forward, the members section
/// says why, and the status line is one node named by its whole line (the
/// macOS e2e looks the off line up by its text).
#[test]
fn the_off_state_names_the_way_to_a_member_for_screen_readers() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = Arc::new(FakeService::new(FakeOptions {
            fast: true,
            ..Default::default()
        }));
        let mut h = AppHarness::new(service, locale, Page::Router);
        let add = tr.f("router.eligibility.add", &[("name", &"Krill")]);
        let tree = h.wait_for(Some(Role::Button), &add);
        let add = tree.find(Some(Role::Button), &add).unwrap();
        assert!(!tree.node(add).is_disabled());
        let on = tree.find_all(Some(Role::Button), &tr.t("router.banner.turn_on"));
        assert_eq!(on.len(), 1, "one way on");
        assert!(
            tree.node(on[0]).is_disabled(),
            "开启路由… waits for a member"
        );
        assert!(tree.has_containing(&tr.t("router.eligibility.none")));
        // The status line: "路由已关闭 —— 直连 …" as one name.
        let off = tr.t("router.banner.off");
        let local = tr.f("router.banner.local", &[("addr", &"\u{1}")]);
        let (head, tail) = local.split_once('\u{1}').unwrap();
        let lines: Vec<String> = tree
            .walk()
            .into_iter()
            .filter_map(|(id, _)| tree.name(id))
            .filter(|n| n.contains(&tr.t("router.banner.inactive")))
            .collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0] == off || (lines[0].starts_with(head) && lines[0].ends_with(tail)),
            "{lines:?}"
        );
        assert!(tree.unnamed().is_empty(), "{:?}", tree.unnamed());
    }
}
