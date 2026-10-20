#![allow(missing_docs)]

//! User bug 105: on the Providers page a change of Krill's line did nothing in
//! direct mode. The arrows saved a priority that only the local router and the
//! next switch to the provider read, while Codex kept talking to the applied
//! line; the user had to switch on the Overview. Now every line row says which
//! line is used (使用中 / In use in direct mode; 首选 / Preferred and the
//! router's latest upstream in router mode), its action is the Overview's
//! (切换到此线路 plans the same switch with the same hot-switch countdown;
//! 设为首选 saves the preference a running router reloads at once), moving
//! another line to #1 of the provider in use is that switch, and the section
//! title and caption say what the controls do in the current mode.

mod a11y_harness;
mod router_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::Rect;
use egui::accesskit::Role;
use router_harness::{Frame, Harness};
use superba_core::{
    AuthMode, LineId, Locale, ProfileId, RouterInstallRequest, RouterLineRoute, SwitchService,
    catalog,
};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Action, Data, Page, providers, shared};
use support::fake::{FakeOptions, FakeService};

const SIZES: [[f32; 2]; 3] = [[960.0, 640.0], [1280.0, 800.0], [1440.0, 900.0]];

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn ids(lines: &[&str]) -> Vec<LineId> {
    lines.iter().map(|l| LineId::new(*l)).collect()
}

fn krill(data: &Data) -> superba_core::Profile {
    data.profile(&ProfileId::new("krill")).unwrap().clone()
}

/// The line's name as the page shows it.
fn line_name(tr: &Tr, id: &str) -> String {
    tr.text(&catalog::krill_site().line(&LineId::new(id)).unwrap().name)
}

/// What a frame's actions asked for: switches as (profile, line, auth) and
/// saved line orders.
#[derive(Debug, Default, PartialEq)]
struct Asked {
    switches: Vec<(ProfileId, Option<LineId>, Option<AuthMode>)>,
    saves: Vec<Vec<LineId>>,
}

impl Asked {
    fn take(&mut self, actions: &[Action]) {
        for a in actions {
            match a {
                Action::Switch {
                    profile,
                    line,
                    auth,
                } => self.switches.push((profile.clone(), line.clone(), *auth)),
                Action::SaveProfile { profile, key } => {
                    assert!(key.is_none(), "a line change never saves a key");
                    self.saves.push(profile.line_priority());
                }
                _ => {}
            }
        }
    }
}

/// `text` drawn whole inside its clip (not cut off by the pane or window).
fn visible(frame: &Frame, text: &str) -> Vec<Rect> {
    let found: Vec<Rect> = frame
        .clipped_texts
        .iter()
        .filter(|(t, _, _)| t == text)
        .map(|(_, rect, clip)| {
            assert!(
                clip.expand(0.5).contains_rect(*rect),
                "{text:?} at {rect:?} is clipped by {clip:?}"
            );
            *rect
        })
        .collect();
    assert!(!found.is_empty(), "missing {text:?}:\n{}", frame.all_text());
    found
}

/// The one rectangle of `text` on the row of the line named `line`.
fn on_row(frame: &Frame, text: &str, line: &str) -> Option<Rect> {
    let name = frame
        .texts
        .iter()
        .find(|(t, _)| t == line)
        .unwrap_or_else(|| panic!("no line {line:?}:\n{}", frame.all_text()))
        .1;
    visible(frame, text)
        .into_iter()
        .find(|r| (r.center().y - name.center().y).abs() < 20.0)
}

#[test]
fn moving_a_line_first_switches_the_provider_in_use_only_in_direct_mode() {
    let service = fake();
    let mut data = router_harness::data_from(&service);
    let profile = krill(&data);
    let order = ids(&["overseas", "cn-fast", "cdn"]);
    assert_eq!(
        shared::ordered_lines(&catalog::krill_site(), Some(&profile))
            .into_iter()
            .map(|l| l.id)
            .collect::<Vec<_>>(),
        order
    );
    assert_eq!(
        shared::applied_line(&data, &profile),
        Some(LineId::new("overseas"))
    );
    let overview = |line: &str| {
        let mut asked = Asked::default();
        asked.take(&[shared::switch_action(&data, &profile, &LineId::new(line))]);
        asked
    };
    let reorder = |data: &Data, from, to| {
        let mut asked = Asked::default();
        asked.take(&[shared::reorder_action(data, &profile, order.clone(), from, to).unwrap()]);
        asked
    };
    // Direct, the provider in use: a new first line is the Overview's switch
    // to it (#2 up, or #1 down).
    assert_eq!(reorder(&data, 1, 0), overview("cn-fast"));
    assert_eq!(reorder(&data, 0, 1), overview("cn-fast"));
    assert_eq!(
        overview("cn-fast").switches,
        vec![(ProfileId::new("krill"), Some(LineId::new("cn-fast")), None)]
    );
    // Moves among the backups keep the line in use and save the order.
    assert_eq!(
        reorder(&data, 2, 1).saves,
        vec![ids(&["overseas", "cdn", "cn-fast"])]
    );
    assert!(shared::reorder_action(&data, &profile, order.clone(), 0, 0).is_none());

    // The applied line not first (an order saved before this fix): bringing it
    // back to #1 only saves.
    let mut legacy = profile.clone();
    legacy.set_line_priority(ids(&["cdn", "overseas", "cn-fast"]));
    let asked = |from, to| {
        let mut a = Asked::default();
        a.take(&[shared::reorder_action(
            &data,
            &legacy,
            ids(&["cdn", "overseas", "cn-fast"]),
            from,
            to,
        )
        .unwrap()]);
        a
    };
    assert_eq!(
        asked(1, 0).saves,
        vec![ids(&["overseas", "cdn", "cn-fast"])]
    );
    assert!(asked(1, 0).switches.is_empty());

    // Another provider: its order is what a switch to it tries first.
    let mut other = profile.clone();
    other.id = ProfileId::new("krill-2");
    assert_eq!(shared::applied_line(&data, &other), None);
    let mut a = Asked::default();
    a.take(&[shared::reorder_action(&data, &other, order.clone(), 1, 0).unwrap()]);
    assert_eq!(a.saves, vec![ids(&["cn-fast", "overseas", "cdn"])]);

    // Router mode: the order is the router's (a save it reloads), never a switch.
    data.profiles
        .value
        .as_mut()
        .unwrap()
        .settings
        .router
        .enabled = true;
    assert_eq!(shared::applied_line(&data, &profile), None);
    let routed = reorder(&data, 1, 0);
    assert!(routed.switches.is_empty());
    assert_eq!(routed.saves, vec![ids(&["cn-fast", "overseas", "cdn"])]);
}

/// The Providers page in a harness with every action it returned collected.
fn page_with(
    h: &mut Harness,
    state: &mut providers::State,
    asked: &mut Asked,
    done: impl Fn(&Frame) -> bool,
) -> Frame {
    let f = h.until(
        |ui, cx, out| {
            let n = out.len();
            providers::show(ui, cx, state, out);
            asked.take(&out[n..]);
        },
        done,
    );
    let f2 = h.settle(|ui, cx, out| {
        let n = out.len();
        providers::show(ui, cx, state, out);
        asked.take(&out[n..]);
    });
    drop(f);
    f2
}

#[test]
fn direct_rows_mark_the_line_in_use_and_switch_like_the_overview() {
    for size in SIZES {
        for locale in [Locale::ZhCn, Locale::En] {
            let tr = Tr::new(locale);
            let service = fake();
            let mut h = Harness::new(size, locale, service.clone(), true)
                .min_height(Page::Providers.min_height());
            let mut state = providers::State::default();
            let mut asked = Asked::default();
            let use_line = tr.t("connection.lines.use");
            let in_use = tr.t("connection.lines.in_use");
            let (overseas, cn_fast, cdn) = (
                line_name(&tr, "overseas"),
                line_name(&tr, "cn-fast"),
                line_name(&tr, "cdn"),
            );
            let f = page_with(&mut h, &mut state, &mut asked, |f| f.has(&use_line));
            let ctx = format!("{size:?} {locale:?}");
            visible(&f, &tr.t("providers.lines.title_direct"));
            assert!(
                f.has(&tr.t("providers.lines.caption_direct_applied")),
                "{ctx}: the caption says what the controls do:\n{}",
                f.all_text()
            );
            assert!(!f.has(&tr.t("providers.lines.title_router")), "{ctx}");
            assert!(on_row(&f, &in_use, &overseas).is_some(), "{ctx}: in use");
            assert!(on_row(&f, &use_line, &overseas).is_none(), "{ctx}");
            assert!(on_row(&f, &use_line, &cdn).is_some(), "{ctx}");
            let button = on_row(&f, &use_line, &cn_fast).expect("Use this line");

            // The click asks for exactly what the Overview's line popover does.
            asked = Asked::default();
            h.click_at(button.center());
            page_with(&mut h, &mut state, &mut asked, |_| true);
            let mut overview = Asked::default();
            overview.take(&[shared::switch_action(
                &h.data,
                &krill(&h.data),
                &LineId::new("cn-fast"),
            )]);
            assert_eq!(asked, overview, "{ctx}");
            assert_eq!(
                asked.switches,
                vec![(ProfileId::new("krill"), Some(LineId::new("cn-fast")), None)],
                "{ctx}"
            );

            // Once that switch is applied (the line in use, first in the
            // order), the badge is on its row.
            let data = &mut h.data;
            let active = data
                .overview
                .value
                .as_mut()
                .unwrap()
                .active
                .as_mut()
                .unwrap();
            active.line_id = Some(LineId::new("cn-fast"));
            let profile = data
                .profiles
                .value
                .as_mut()
                .unwrap()
                .profiles
                .iter_mut()
                .find(|p| p.id == "krill")
                .unwrap();
            profile.set_line_priority(ids(&["cn-fast", "overseas"]));
            let f = page_with(&mut h, &mut state, &mut asked, |_| true);
            assert!(on_row(&f, &in_use, &cn_fast).is_some(), "{ctx}: moved");
            assert!(on_row(&f, &in_use, &overseas).is_none(), "{ctx}");
            assert!(on_row(&f, &use_line, &overseas).is_some(), "{ctx}");
        }
    }
}

/// Router mode on the fake (Krill a member) and in the harness data.
fn router_mode(service: &FakeService, h: &mut Harness) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        service
            .router_install(RouterInstallRequest {
                members: vec![ProfileId::new("krill")],
            })
            .await
            .unwrap();
        let mut settings = service.profile_set().await.unwrap().settings;
        settings.router.enabled = true;
        service.save_settings(settings).await.unwrap();
    });
    h.reload(service);
}

#[test]
fn router_rows_show_preferred_and_current_and_prefer_saves_without_a_switch() {
    for size in SIZES {
        for locale in [Locale::ZhCn, Locale::En] {
            let tr = Tr::new(locale);
            let service = fake();
            service.set_last_route(
                ProfileId::new("krill"),
                RouterLineRoute {
                    line_id: "cn-fast".into(),
                    failed_lines: vec!["overseas".into()],
                },
            );
            let mut h = Harness::new(size, locale, service.clone(), true)
                .min_height(Page::Providers.min_height());
            router_mode(&service, &mut h);
            let mut state = providers::State::default();
            let mut asked = Asked::default();
            let ctx = format!("{size:?} {locale:?}");
            let (overseas, cn_fast, cdn) = (
                line_name(&tr, "overseas"),
                line_name(&tr, "cn-fast"),
                line_name(&tr, "cdn"),
            );
            // The router's latest upstream, with the line that failed before it.
            let current = tr.f(
                "router.preference.failed_over",
                &[("line", &cn_fast), ("failed", &overseas)],
            );
            let f = page_with(&mut h, &mut state, &mut asked, |f| f.has(&current));
            visible(&f, &tr.t("providers.lines.title_router"));
            assert!(
                f.has(&tr.t("providers.lines.caption_router")),
                "{ctx}:\n{}",
                f.all_text()
            );
            let prefer = tr.t("connection.lines.prefer");
            assert!(
                on_row(&f, &tr.t("connection.lines.preferred"), &overseas).is_some(),
                "{ctx}: #1 is preferred"
            );
            assert!(
                !f.has(&tr.t("connection.lines.use")),
                "{ctx}: no switch here"
            );
            assert!(on_row(&f, &prefer, &cn_fast).is_some(), "{ctx}");
            // The current line's host is marked.
            let marked = format!(
                "{} · {}",
                catalog::krill_site()
                    .line(&LineId::new("cn-fast"))
                    .unwrap()
                    .origin
                    .host(),
                tr.t("connection.lines.current")
            );
            assert!(f.has(&marked), "{ctx}: {marked:?}\n{}", f.all_text());

            // 设为首选 saves the preference (rline: a running router reloads
            // it), the same as the Overview's line popover, and plans no switch.
            let button = on_row(&f, &prefer, &cdn).expect("Set as preferred");
            asked = Asked::default();
            h.click_at(button.center());
            page_with(&mut h, &mut state, &mut asked, |_| true);
            let mut overview = Asked::default();
            overview.take(&[shared::switch_action(
                &h.data,
                &krill(&h.data),
                &LineId::new("cdn"),
            )]);
            assert_eq!(asked, overview, "{ctx}");
            assert_eq!(
                asked.saves,
                vec![ids(&["cdn", "overseas", "cn-fast"])],
                "{ctx}"
            );
            assert!(asked.switches.is_empty(), "{ctx}");
        }
    }
}

/// The whole app on the fake: the row's action goes through the shell's switch
/// flow (plan, the hot-switch countdown with its 立即应用 / Cancel, execute) like
/// a line picked on the Overview, and the page then shows the new line in use. (The CDN line: the
/// fake's Krill uses PAT, which 国内极速 blocks; that plan is refused with
/// its alternative, as on the Overview.)
#[test]
fn use_this_line_runs_the_switch_flow_and_the_badge_follows() {
    let service = fake();
    let tr = Tr::new(Locale::ZhCn);
    let mut h = AppHarness::new(service.clone(), Locale::ZhCn, Page::Providers);
    let named = |key: &str, line: &str| tr.f(key, &[("line", &line_name(&tr, line))]);
    // The rows come with the profiles; the 使用中 badge needs the Overview's
    // active state, which can arrive a few frames later (a slower runner).
    let t = h.until("the rows and the line in use", |t| {
        t.find(
            Some(Role::Button),
            &named("connection.lines.use_named", "cdn"),
        )
        .is_some()
            && t.has(&named("connection.lines.in_use_named", "overseas"))
    });
    assert!(
        t.find(
            Some(Role::Button),
            &named("connection.lines.use_named", "overseas")
        )
        .is_none()
    );
    h.click(
        Some(Role::Button),
        &named("connection.lines.use_named", "cdn"),
    );
    // The Overview's flow: the planned hot switch armed with its countdown (a
    // new line of the provider in use is announced as applying its changes,
    // as when the line is picked on the Overview), to the new line.
    let now = tr.t("banner.reapply.now");
    let t = h.wait_for(Some(Role::Button), &now);
    assert!(
        t.has_containing(&format!("「Krill · {} ·", line_name(&tr, "cdn"))),
        "{}",
        t.dump()
    );
    assert!(t.has(&tr.t("action.cancel")), "{}", t.dump());
    h.click(Some(Role::Button), &now);
    let t = h.until("the switch is applied and the page follows", |t| {
        t.has(&named("connection.lines.in_use_named", "cdn"))
    });
    assert!(
        t.find(
            Some(Role::Button),
            &named("connection.lines.use_named", "overseas")
        )
        .is_some(),
        "{}",
        t.dump()
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let active = rt.block_on(service.overview()).unwrap().active.unwrap();
    assert_eq!(active.line_id, Some(LineId::new("cdn")));
}

/// Router mode in the whole app: 设为首选 reorders the provider's lines (the
/// preferred badge moves) through a profile save, with no switch flow.
#[test]
fn set_as_preferred_reorders_without_a_switch_in_the_app() {
    let service = fake();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        service
            .router_install(RouterInstallRequest {
                members: vec![ProfileId::new("krill")],
            })
            .await
            .unwrap();
        let mut settings = service.profile_set().await.unwrap().settings;
        settings.router.enabled = true;
        service.save_settings(settings).await.unwrap();
    });
    let tr = Tr::new(Locale::ZhCn);
    let mut h = AppHarness::new(service.clone(), Locale::ZhCn, Page::Providers);
    let named = |key: &str, line: &str| tr.f(key, &[("line", &line_name(&tr, line))]);
    let t = h.wait_for(
        Some(Role::Button),
        &named("connection.lines.prefer_named", "cdn"),
    );
    assert!(t.has(&named("connection.lines.preferred_named", "overseas")));
    h.click(
        Some(Role::Button),
        &named("connection.lines.prefer_named", "cdn"),
    );
    let t = h.until("the order is saved and shown", |t| {
        t.has(&named("connection.lines.preferred_named", "cdn"))
    });
    assert!(
        t.find(
            Some(Role::Button),
            &named("connection.lines.prefer_named", "overseas")
        )
        .is_some()
    );
    for key in ["banner.switch_now", "banner.reapply.now"] {
        assert!(t.find(Some(Role::Button), &tr.t(key)).is_none(), "{key}");
    }
    let set = rt.block_on(service.profile_set()).unwrap();
    assert_eq!(
        set.profile(&ProfileId::new("krill"))
            .unwrap()
            .line_priority(),
        ids(&["cdn", "overseas", "cn-fast"])
    );
}
