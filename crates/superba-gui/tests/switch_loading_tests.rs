#![allow(missing_docs)]
//! Loads that follow a choice (user bug 83). The shared state machine: the
//! latest choice wins whatever order the answers come in, an error or a
//! timeout is the failure of its own choice, Retry loads it again, and the
//! loading flag. The pages that switch to another host or provider: Edit Codex
//! files (the new home at once, busy; a loading state instead of the previous
//! home's file; a failure with Retry in place), the Codex settings page (the
//! same for another provider) and Sessions (a host being scanned has a spinner
//! in its segment and a loading card; a failed first scan offers Retry). The
//! loading state is one busy progress indicator named by its text, as
//! AccessKit / UI Automation read it.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::accesskit::{Node, Role};
use superba_core::{
    CodexFileKind, Error, Locale, Profile, ProfileId, SwitchService, TargetId, Timestamp,
};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::sessions;
use superba_gui::pages::settings::model::Model;
use superba_gui::pages::settings::source::{self, Input};
use superba_gui::pages::settings::{self as settings_page, State};
use superba_gui::pages::shared::loading::{Failure, Latest};
use superba_gui::pages::{Action, Cx, Data};
use superba_gui::runtime::ReadTask;
use support::fake::{FakeOptions, FakeService};
use support::fit::{SIZES, render_fit};

const LIMIT: Duration = Duration::from_secs(10);
const LOCAL: &str = "# my settings\nmodel = \"gpt-5.5\"\n";
const DEVBOX: &str = "# devbox\nmodel = \"gpt-5.4\"\n";
const GPU: &str = "# gpu-01\nmodel = \"gpt-5.3\"\n";

// ------------------------------------------------------------ the state machine

/// A load that answers when the test sends its answer.
fn gate<T: Send + 'static>() -> (
    tokio::sync::oneshot::Sender<superba_core::Result<T>>,
    ReadTask<T>,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = superba_gui::runtime::task(async move {
        rx.await
            .map_err(|_| Error::other("the test dropped the gate"))?
    });
    (tx, task)
}

/// Poll until an answer is taken or the load stops (or `max`).
fn wait<K: Clone + PartialEq, T: Send + 'static>(l: &mut Latest<K, T>) -> Option<(K, T)> {
    let start = Instant::now();
    loop {
        if let Some(v) = l.poll() {
            return Some(v);
        }
        if !l.loading() || start.elapsed() > Duration::from_secs(5) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Poll for `ms` and return what arrived (nothing must).
fn quiet<K: Clone + PartialEq, T: Send + 'static>(l: &mut Latest<K, T>, ms: u64) -> Option<(K, T)> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        if let Some(v) = l.poll() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn the_latest_choice_wins_whatever_order_the_answers_come_in() {
    let mut l: Latest<&str, u32> = Latest::default();
    assert!(!l.loading());
    assert_eq!(l.chosen(), None);

    // devbox, then gpu-01 at once; devbox answers first: never used.
    let (devbox_tx, devbox) = gate();
    let (gpu_tx, gpu) = gate();
    l.start("devbox", devbox, LIMIT);
    assert!(l.loading());
    assert_eq!(l.chosen(), Some(&"devbox"));
    l.start("gpu-01", gpu, LIMIT);
    assert!(l.is_chosen(&"gpu-01"));
    devbox_tx.send(Ok(1)).unwrap();
    assert_eq!(quiet(&mut l, 80), None, "an earlier choice's answer");
    assert!(l.loading(), "still waiting for gpu-01");
    gpu_tx.send(Ok(2)).unwrap();
    assert_eq!(wait(&mut l), Some(("gpu-01", 2)));
    assert!(!l.loading());
    assert!(l.failure().is_none());

    // The other order: the latest answers first, the earlier one later.
    let (a_tx, a) = gate();
    let (b_tx, b) = gate();
    l.start("a", a, LIMIT);
    l.start("b", b, LIMIT);
    b_tx.send(Ok(20)).unwrap();
    assert_eq!(wait(&mut l), Some(("b", 20)));
    let _ = a_tx.send(Ok(10));
    assert_eq!(
        quiet(&mut l, 80),
        None,
        "a late answer of an earlier choice"
    );
    assert_eq!(l.chosen(), Some(&"b"));

    // Choosing back and forth: only the last start counts.
    let mut senders = Vec::new();
    for (i, key) in ["x", "y", "x", "y", "x"].into_iter().enumerate() {
        let (tx, t) = gate();
        l.start(key, t, LIMIT);
        senders.push((i as u32, tx));
    }
    for (i, tx) in senders.into_iter().rev() {
        let _ = tx.send(Ok(i));
    }
    assert_eq!(wait(&mut l), Some(("x", 4)), "the last start of x");
    assert_eq!(quiet(&mut l, 80), None);
}

#[test]
fn an_error_is_the_failure_of_its_choice_and_retry_loads_it_again() {
    let mut l: Latest<&str, u32> = Latest::default();
    let (tx, t) = gate();
    l.start("devbox", t, LIMIT);
    tx.send(Err(Error::Ssh {
        host: "devbox".into(),
        message: "Connection refused".into(),
    }))
    .unwrap();
    assert_eq!(wait(&mut l), None);
    assert!(!l.loading(), "a failed load is over");
    assert!(
        matches!(l.failure(), Some(Failure::Error(Error::Ssh { message, .. })) if message == "Connection refused"),
        "{:?}",
        l.failure()
    );
    // Retry: the same choice loads again; the failure is gone meanwhile.
    let (tx, t) = gate();
    l.start("devbox", t, LIMIT);
    assert!(l.loading());
    assert!(l.failure().is_none());
    tx.send(Ok(7)).unwrap();
    assert_eq!(wait(&mut l), Some(("devbox", 7)));
    // A failure belongs to its choice: choosing another forgets it.
    let (tx, t) = gate();
    l.start("gpu-01", t, LIMIT);
    tx.send(Err(Error::other("no route"))).unwrap();
    assert_eq!(wait(&mut l), None);
    assert!(l.failure().is_some());
    l.choose("devbox");
    assert!(l.failure().is_none());
    assert!(!l.loading());
}

#[test]
fn a_load_past_its_limit_times_out_and_its_late_answer_is_ignored() {
    let mut l: Latest<&str, u32> = Latest::default();
    let (tx, t) = gate();
    let limit = Duration::from_millis(80);
    l.start("slow", t, limit);
    let start = Instant::now();
    assert_eq!(wait(&mut l), None);
    assert!(start.elapsed() >= limit);
    assert!(!l.loading());
    assert!(
        matches!(l.failure(), Some(Failure::TimedOut(d)) if *d == limit),
        "{:?}",
        l.failure()
    );
    let _ = tx.send(Ok(1));
    assert_eq!(quiet(&mut l, 60), None, "the answer came too late");
    assert!(matches!(l.failure(), Some(Failure::TimedOut(_))));

    // Choosing another key without loading drops the running load; choosing
    // the same key keeps it.
    let (tx, t) = gate();
    l.start("x", t, LIMIT);
    l.choose("x");
    assert!(l.loading());
    l.choose("y");
    assert!(!l.loading());
    let _ = tx.send(Ok(5));
    assert_eq!(quiet(&mut l, 60), None);
    // Cancel stops waiting but keeps the choice.
    let (_tx, t) = gate::<u32>();
    l.start("z", t, LIMIT);
    l.cancel();
    assert!(!l.loading());
    assert_eq!(l.chosen(), Some(&"z"));
}

#[tokio::test]
async fn failures_read_in_both_languages() {
    let service = files_fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let timed_out = Failure::TimedOut(Duration::from_secs(90));
    with_cx(&data, &svc, Locale::En, |cx| {
        assert_eq!(
            timed_out.text(cx),
            "No answer within 90 s. Check the connection, then retry."
        );
    });
    with_cx(&data, &svc, Locale::ZhCn, |cx| {
        assert_eq!(timed_out.text(cx), "90 秒内没有回应。请检查连接后重试。");
        let ssh = Failure::Error(Error::Ssh {
            host: "devbox".into(),
            message: "Connection refused".into(),
        });
        assert!(
            ssh.text(cx).contains("Connection refused"),
            "{}",
            ssh.text(cx)
        );
    });
}

// ------------------------------------------------------------------ helpers

fn files_fake() -> Arc<FakeService> {
    let f = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    f.set_codex_file(TargetId::local(), CodexFileKind::ConfigToml, Some(LOCAL));
    f
}

async fn data_of(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data.hosts.finish(service.hosts().await);
    data.sessions.finish(service.sessions().await);
    data
}

/// The managed host shown as `name` (devbox, gpu-01).
fn host(data: &Data, name: &str) -> TargetId {
    data.hosts
        .value
        .as_ref()
        .unwrap()
        .iter()
        .find(|h| h.host.label == name)
        .unwrap_or_else(|| panic!("no host {name}"))
        .host
        .target_id
        .clone()
}

fn with_cx<R>(
    data: &Data,
    service: &Arc<dyn SwitchService>,
    locale: Locale,
    f: impl FnOnce(&Cx<'_>) -> R,
) -> R {
    let tr = Tr::new(locale);
    let fonts = fonts::FontReport::default();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &fonts,
        service,
        log: &[],
    };
    f(&cx)
}

/// Poll the files editor until its background work is done.
fn settle_files(state: &mut State, data: &Data, svc: &Arc<dyn SwitchService>) {
    let start = Instant::now();
    while with_cx(data, svc, Locale::En, |cx| {
        state.files.poll(cx, &mut Vec::new())
    }) {
        assert!(start.elapsed() < Duration::from_secs(5), "the read hangs");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Poll the files editor for `ms` (answers that arrive are applied).
fn poll_files_for(state: &mut State, data: &Data, svc: &Arc<dyn SwitchService>, ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        with_cx(data, svc, Locale::En, |cx| {
            state.files.poll(cx, &mut Vec::new())
        });
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The page's accessibility tree after two frames at 1280x800.
fn a11y_nodes(
    data: &Data,
    locale: Locale,
    mut show: impl FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
) -> Vec<Node> {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let report = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&report));
    style::install(&ctx, Some(egui::Theme::Light));
    let tr = Tr::new(locale);
    let service = support::idle_service();
    let mut nodes = Vec::new();
    for _ in 0..2 {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &report,
                    service: &service,
                    log: &[],
                };
                show(ui, &cx, &mut Vec::new());
            },
        );
        let mut out = out;
        nodes = out
            .platform_output
            .accesskit_update
            .take()
            .map(|u| u.nodes.into_iter().map(|(_, n)| n).collect())
            .unwrap_or_default();
        out.drop_without_applying_deltas();
    }
    nodes
}

/// The busy progress indicator named `name`.
fn busy_progress<'a>(nodes: &'a [Node], name: &str) -> Option<&'a Node> {
    nodes
        .iter()
        .find(|n| n.role() == Role::ProgressIndicator && n.label() == Some(name) && n.is_busy())
}

// -------------------------------------------------------- Edit Codex files

#[tokio::test]
async fn edit_codex_files_shows_the_new_home_at_once_and_never_the_previous_file() {
    let service = files_fake();
    let data = data_of(&service).await;
    let devbox = host(&data, "devbox");
    service.set_codex_file(devbox.clone(), CodexFileKind::ConfigToml, Some(DEVBOX));
    // devbox does not answer until released.
    service.hold_codex_file(devbox.clone(), true);
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = State::default();
    with_cx(&data, &svc, Locale::En, |cx| {
        state
            .files
            .open_file(cx, TargetId::local(), CodexFileKind::ConfigToml)
    });
    settle_files(&mut state, &data, &svc);
    assert_eq!(state.files.buffer, LOCAL);

    // Another home: chosen at once, the local file is no longer shown.
    with_cx(&data, &svc, Locale::En, |cx| {
        state
            .files
            .open_file(cx, devbox.clone(), CodexFileKind::ConfigToml)
    });
    assert_eq!(state.files.target(), devbox);
    assert!(state.files.loading());
    assert!(state.files.switching());
    assert!(
        state.files.doc.is_none(),
        "the previous home's file is gone"
    );
    assert!(state.files.buffer.is_empty());
    for (locale, loading) in [
        (Locale::ZhCn, "正在读取 devbox 的 config.toml…"),
        (Locale::En, "Reading config.toml on devbox…"),
    ] {
        let report = render_fit(
            SIZES[0],
            locale,
            egui::Theme::Light,
            &data,
            |ui, cx, out| {
                settings_page::show(ui, cx, &mut state, out);
            },
        );
        let all = report.all_text();
        assert!(report.whole(loading), "{all}");
        assert!(!all.contains("# my settings"), "the local file: {all}");
        // The dropdown shows the new home.
        assert!(report.whole("devbox"), "{all}");
        assert!(report.fits(), "{locale:?}");
        // AccessKit / UI Automation: one busy progress indicator named by the
        // text, and the busy "Where" dropdown with the new home as its value.
        let nodes = a11y_nodes(&data, locale, |ui, cx, out| {
            settings_page::show(ui, cx, &mut state, out);
        });
        let progress: Vec<_> = nodes
            .iter()
            .filter(|n| n.role() == Role::ProgressIndicator || n.is_busy())
            .collect();
        assert!(busy_progress(&nodes, loading).is_some(), "{progress:#?}");
        let combo = nodes
            .iter()
            .find(|n| n.role() == Role::ComboBox && n.value() == Some("devbox"))
            .expect("the Where dropdown");
        assert!(combo.is_busy());
    }

    // devbox answers: its file, and nothing busy any more.
    service.hold_codex_file(devbox.clone(), false);
    settle_files(&mut state, &data, &svc);
    assert_eq!(state.files.buffer, DEVBOX);
    assert!(!state.files.switching());
    let nodes = a11y_nodes(&data, Locale::En, |ui, cx, out| {
        settings_page::show(ui, cx, &mut state, out);
    });
    assert!(!nodes.iter().any(|n| n.is_busy()), "nothing loads any more");
}

#[tokio::test]
async fn edit_codex_files_shows_only_the_last_chosen_home() {
    let service = files_fake();
    let data = data_of(&service).await;
    let (devbox, gpu) = (host(&data, "devbox"), host(&data, "gpu-01"));
    service.set_codex_file(devbox.clone(), CodexFileKind::ConfigToml, Some(DEVBOX));
    service.set_codex_file(gpu.clone(), CodexFileKind::ConfigToml, Some(GPU));
    let svc: Arc<dyn SwitchService> = service.clone();
    let open = |state: &mut State, target: &TargetId| {
        with_cx(&data, &svc, Locale::En, |cx| {
            state
                .files
                .open_file(cx, target.clone(), CodexFileKind::ConfigToml)
        });
    };

    // devbox is slow, gpu-01 quick: gpu-01 answers first and stays when
    // devbox answers later.
    service.hold_codex_file(devbox.clone(), true);
    let mut state = State::default();
    open(&mut state, &TargetId::local());
    settle_files(&mut state, &data, &svc);
    open(&mut state, &devbox);
    open(&mut state, &gpu);
    settle_files(&mut state, &data, &svc);
    assert_eq!(state.files.buffer, GPU);
    service.hold_codex_file(devbox.clone(), false);
    poll_files_for(&mut state, &data, &svc, 150);
    assert_eq!(state.files.buffer, GPU, "devbox answered later");
    assert_eq!(state.files.doc.as_ref().map(|d| &d.target), Some(&gpu));

    // devbox quick, gpu-01 slow, chosen in that order: devbox's answer arrives
    // while gpu-01 is chosen and is never shown.
    service.set_codex_file_delay(devbox.clone(), Duration::from_millis(20));
    service.hold_codex_file(gpu.clone(), true);
    let mut state = State::default();
    open(&mut state, &TargetId::local());
    settle_files(&mut state, &data, &svc);
    open(&mut state, &devbox);
    open(&mut state, &gpu);
    poll_files_for(&mut state, &data, &svc, 150);
    assert!(state.files.doc.is_none(), "devbox's answer is not gpu-01's");
    assert!(state.files.loading());
    assert_eq!(state.files.target(), gpu);
    service.hold_codex_file(gpu.clone(), false);
    settle_files(&mut state, &data, &svc);
    assert_eq!(state.files.buffer, GPU);
}

#[tokio::test]
async fn a_home_that_cannot_be_read_says_why_in_place_with_retry() {
    let service = files_fake();
    let data = data_of(&service).await;
    let devbox = host(&data, "devbox");
    service.fail_codex_file(devbox.clone(), Some("Connection refused"));
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = State::default();
    with_cx(&data, &svc, Locale::En, |cx| {
        state
            .files
            .open_file(cx, devbox.clone(), CodexFileKind::ConfigToml)
    });
    settle_files(&mut state, &data, &svc);
    assert!(state.files.load_failure().is_some());
    assert!(!state.files.loading());
    for (locale, title, retry) in [
        (Locale::ZhCn, "无法读取 devbox 的 config.toml", "重试"),
        (
            Locale::En,
            "config.toml on devbox could not be read",
            "Retry",
        ),
    ] {
        let report = render_fit(SIZES[0], locale, egui::Theme::Dark, &data, |ui, cx, out| {
            settings_page::show(ui, cx, &mut state, out);
        });
        let all = report.all_text();
        assert!(report.whole(title), "{all}");
        assert!(all.contains("Connection refused"), "{all}");
        assert!(report.whole(retry), "{all}");
        assert!(report.fits(), "{locale:?}");
    }
    // Retry reads it again.
    service.fail_codex_file(devbox.clone(), None);
    service.set_codex_file(devbox.clone(), CodexFileKind::ConfigToml, Some(DEVBOX));
    service.set_codex_file_delay(devbox.clone(), Duration::from_millis(200));
    let h = support::ui::Harness::new([1280.0, 800.0], egui::Theme::Light);
    let tr = Tr::new(Locale::En);
    let clicked = h.click_text("Retry", &mut |ui| {
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(ui.ctx()),
            data: &data,
            busy: false,
            now: Timestamp::now(),
            fonts: &h.fonts,
            service: &svc,
            log: &[],
        };
        settings_page::show(ui, &cx, &mut state, &mut Vec::new());
    });
    assert!(clicked, "no Retry button");
    assert!(state.files.loading(), "Retry reads the file again");
    assert!(state.files.load_failure().is_none());
    settle_files(&mut state, &data, &svc);
    assert_eq!(state.files.buffer, DEVBOX);

    // Reading the shown file again fails: it stays, with the reason.
    service.fail_codex_file(devbox.clone(), Some("Connection reset"));
    with_cx(&data, &svc, Locale::En, |cx| state.files.reload(cx));
    settle_files(&mut state, &data, &svc);
    assert_eq!(state.files.buffer, DEVBOX);
    assert!(state.files.load_failure().is_some());
    let report = render_fit(
        SIZES[0],
        Locale::ZhCn,
        egui::Theme::Light,
        &data,
        |ui, cx, out| {
            settings_page::show(ui, cx, &mut state, out);
        },
    );
    let all = report.all_text();
    assert!(all.contains("# devbox"), "{all}");
    assert!(all.contains("无法重新读取 config.toml"), "{all}");
    assert!(all.contains("Connection reset"), "{all}");
}

// ------------------------------------------------ the Codex settings page

/// A second key-based provider next to Krill.
fn with_relay(data: &mut Data) -> ProfileId {
    let set = data.profiles.value.as_mut().unwrap();
    let krill = set
        .profiles
        .iter()
        .find(|p| p.id.as_str() == "krill")
        .unwrap()
        .clone();
    let relay = Profile {
        id: ProfileId::new("relay"),
        name: "Relay CN".into(),
        ..krill
    };
    set.profiles.push(relay);
    ProfileId::new("relay")
}

/// A loader that builds the model from the shipped catalog (once a held
/// profile is released), or fails for a profile marked broken.
#[derive(Clone, Default)]
struct TestLoader {
    /// Profiles whose model is not built until released.
    held: Arc<Mutex<HashMap<Option<ProfileId>, bool>>>,
    broken: Arc<Mutex<Option<ProfileId>>>,
    calls: Arc<Mutex<Vec<Option<ProfileId>>>>,
}

impl TestLoader {
    fn loader(&self) -> settings_page::Loader {
        let me = self.clone();
        let store = superba_catalog::CatalogStore::builtin().unwrap();
        Arc::new(move |cx: &Cx<'_>, profile: Option<ProfileId>| {
            me.calls.lock().unwrap().push(profile.clone());
            let held = me.held.clone();
            let key = profile.clone();
            let broken = me
                .broken
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|b| profile.as_ref() == Some(b));
            let input = Input {
                overview: cx.data.overview.value.clone(),
                profiles: cx.data.profiles.value.clone(),
                profile,
            };
            let store = store.clone();
            superba_gui::runtime::task(async move {
                while held.lock().unwrap().get(&key).copied().unwrap_or(false) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                if broken {
                    return Err(Error::other("the settings catalog cannot be loaded"));
                }
                source::build(
                    &store,
                    &input,
                    Some(&toml::Table::new()),
                    superba_catalog::Os::Linux,
                )
            })
        })
    }
}

/// Draw the page (polling it) until `done` or 5 s.
fn drive_settings(
    data: &Data,
    svc: &Arc<dyn SwitchService>,
    state: &mut State,
    mut done: impl FnMut(&State) -> bool,
) {
    let start = Instant::now();
    loop {
        let ctx = egui::Context::default();
        let tr = Tr::new(Locale::En);
        let fonts_report = fonts::FontReport::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let cx = Cx {
                tr: &tr,
                pal: Palette::of(ui.ctx()),
                data,
                busy: false,
                now: Timestamp::now(),
                fonts: &fonts_report,
                service: svc,
                log: &[],
            };
            settings_page::show(ui, &cx, state, &mut Vec::new());
        });
        out.drop_without_applying_deltas();
        if done(state) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "never done: {:?}",
            state.load_failure()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shown_profile(state: &State) -> Option<String> {
    state
        .model()
        .and_then(|m: &Model| m.profile_id.as_ref())
        .map(ToString::to_string)
}

#[tokio::test]
async fn another_provider_shows_at_once_with_a_loading_state_and_the_last_choice_wins() {
    let service = files_fake();
    let mut data = data_of(&service).await;
    let relay = with_relay(&mut data);
    let krill = ProfileId::new("krill");
    let svc: Arc<dyn SwitchService> = service.clone();
    let loads = TestLoader::default();
    let mut state = State::default();
    state.set_loader(loads.loader());
    drive_settings(&data, &svc, &mut state, |s| s.model().is_some());
    assert_eq!(shown_profile(&state).as_deref(), Some("krill"));

    loads.held.lock().unwrap().insert(Some(relay.clone()), true);
    with_cx(&data, &svc, Locale::En, |cx| {
        state.select_profile(cx, relay.clone())
    });
    assert!(
        state.model().is_none(),
        "Krill's settings are not shown as Relay's"
    );
    assert!(state.switching());
    assert_eq!(state.chosen_profile(), Some(&relay));
    for (locale, loading) in [
        (Locale::ZhCn, "正在加载 Relay CN 的 Codex 设置…"),
        (Locale::En, "Loading the Codex settings of Relay CN…"),
    ] {
        let report = render_fit(
            SIZES[0],
            locale,
            egui::Theme::Light,
            &data,
            |ui, cx, out| {
                settings_page::show(ui, cx, &mut state, out);
            },
        );
        let all = report.all_text();
        assert!(report.whole(loading), "{all}");
        // The picker shows the new provider; no rows of the old model.
        assert!(all.contains("Relay CN"), "{all}");
        assert!(!all.contains("approval_policy"), "a row of Krill: {all}");
        assert!(report.fits(), "{locale:?}");
        let nodes = a11y_nodes(&data, locale, |ui, cx, out| {
            settings_page::show(ui, cx, &mut state, out);
        });
        assert!(busy_progress(&nodes, loading).is_some(), "{nodes:#?}");
        assert!(
            nodes.iter().any(|n| n.role() == Role::ComboBox
                && n.is_busy()
                && n.value().is_some_and(|v| v.contains("Relay CN"))),
            "the busy provider picker"
        );
    }

    // Back to Krill while Relay is still being built: Krill wins, Relay's
    // later model is never shown.
    with_cx(&data, &svc, Locale::En, |cx| {
        state.select_profile(cx, krill.clone())
    });
    drive_settings(&data, &svc, &mut state, |s| s.model().is_some());
    assert_eq!(shown_profile(&state).as_deref(), Some("krill"));
    loads.held.lock().unwrap().clear();
    std::thread::sleep(Duration::from_millis(300));
    drive_settings(&data, &svc, &mut state, |_| true);
    assert_eq!(
        shown_profile(&state).as_deref(),
        Some("krill"),
        "Relay came later"
    );
    assert_eq!(
        loads.calls.lock().unwrap().as_slice(),
        &[None, Some(relay.clone()), Some(krill.clone())]
    );
}

#[tokio::test]
async fn a_provider_whose_settings_cannot_load_says_why_with_retry() {
    let service = files_fake();
    let mut data = data_of(&service).await;
    let relay = with_relay(&mut data);
    let svc: Arc<dyn SwitchService> = service.clone();
    let loads = TestLoader::default();
    let mut state = State::default();
    state.set_loader(loads.loader());
    drive_settings(&data, &svc, &mut state, |s| s.model().is_some());
    *loads.broken.lock().unwrap() = Some(relay.clone());
    with_cx(&data, &svc, Locale::En, |cx| {
        state.select_profile(cx, relay.clone())
    });
    drive_settings(&data, &svc, &mut state, |s| s.load_failure().is_some());
    assert!(state.model().is_none());
    for (locale, title, retry) in [
        (Locale::ZhCn, "无法加载 Relay CN 的 Codex 设置", "重试"),
        (
            Locale::En,
            "The Codex settings of Relay CN could not be loaded",
            "Retry",
        ),
    ] {
        let report = render_fit(SIZES[1], locale, egui::Theme::Dark, &data, |ui, cx, out| {
            settings_page::show(ui, cx, &mut state, out);
        });
        let all = report.all_text();
        assert!(report.whole(title), "{all}");
        assert!(
            all.contains("the settings catalog cannot be loaded"),
            "{all}"
        );
        assert!(report.whole(retry), "{all}");
        assert!(
            all.contains("Relay CN"),
            "the picker keeps the choice: {all}"
        );
    }
    // Retry builds it again.
    *loads.broken.lock().unwrap() = None;
    let h = support::ui::Harness::new([1280.0, 800.0], egui::Theme::Light);
    let tr = Tr::new(Locale::En);
    let clicked = h.click_text("Retry", &mut |ui| {
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(ui.ctx()),
            data: &data,
            busy: false,
            now: Timestamp::now(),
            fonts: &h.fonts,
            service: &svc,
            log: &[],
        };
        settings_page::show(ui, &cx, &mut state, &mut Vec::new());
    });
    assert!(clicked, "no Retry button");
    drive_settings(&data, &svc, &mut state, |s| s.model().is_some());
    assert_eq!(shown_profile(&state).as_deref(), Some("relay"));
    assert_eq!(loads.calls.lock().unwrap().len(), 3);

    // Reading Relay's settings again fails: they stay, with a notice.
    *loads.broken.lock().unwrap() = Some(relay.clone());
    let clicked = h.click_text("Reload", &mut |ui| {
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(ui.ctx()),
            data: &data,
            busy: false,
            now: Timestamp::now(),
            fonts: &h.fonts,
            service: &svc,
            log: &[],
        };
        settings_page::show(ui, &cx, &mut state, &mut Vec::new());
    });
    assert!(clicked, "no Reload button");
    drive_settings(&data, &svc, &mut state, |s| s.load_failure().is_some());
    assert_eq!(shown_profile(&state).as_deref(), Some("relay"));
    let report = render_fit(
        SIZES[0],
        Locale::ZhCn,
        egui::Theme::Light,
        &data,
        |ui, cx, out| {
            settings_page::show(ui, cx, &mut state, out);
        },
    );
    let all = report.all_text();
    assert!(all.contains("无法重新读取 Codex 设置"), "{all}");
    assert!(report.fits());
}

// ------------------------------------------------------------------ Sessions

#[tokio::test]
async fn a_host_being_scanned_spins_in_the_switcher_and_a_failed_scan_offers_retry() {
    let service = files_fake();
    let data = data_of(&service).await;
    let gpu = host(&data, "gpu-01");
    let svc: Arc<dyn SwitchService> = service.clone();
    // gpu-01 does not answer until released, then fails.
    service.hold_host_sessions(&gpu, true);
    service.fail_host_sessions(&gpu, true);
    let mut state = sessions::State::default();
    with_cx(&data, &svc, Locale::En, |cx| {
        state.select(cx, Some(gpu.clone()))
    });
    let loading = "Scanning sessions on gpu-01…";
    let report = render_fit(
        SIZES[0],
        Locale::En,
        egui::Theme::Light,
        &data,
        |ui, cx, out| {
            sessions::show(ui, cx, &mut state, out);
        },
    );
    assert!(report.whole(loading), "{}", report.all_text());
    let nodes = a11y_nodes(&data, Locale::En, |ui, cx, out| {
        sessions::show(ui, cx, &mut state, out);
    });
    assert!(busy_progress(&nodes, loading).is_some(), "{nodes:#?}");
    let segment = nodes
        .iter()
        .find(|n| n.role() == Role::RadioButton && n.label() == Some("gpu-01"))
        .expect("the gpu-01 segment");
    assert!(segment.is_busy(), "the segment of the host being scanned");
    assert!(
        nodes
            .iter()
            .filter(|n| n.role() == Role::RadioButton && n.is_busy())
            .count()
            == 1,
        "only gpu-01 is scanned"
    );

    // The scan fails: the reason in place with Retry, which scans again.
    service.hold_host_sessions(&gpu, false);
    let start = Instant::now();
    let h = support::ui::Harness::new([1280.0, 800.0], egui::Theme::Light);
    let tr = Tr::new(Locale::En);
    let draw = |ui: &mut egui::Ui, state: &mut sessions::State| {
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(ui.ctx()),
            data: &data,
            busy: false,
            now: Timestamp::now(),
            fonts: &h.fonts,
            service: &svc,
            log: &[],
        };
        sessions::show(ui, &cx, state, &mut Vec::new());
    };
    loop {
        let texts = h.settle(&mut |ui| draw(ui, &mut state));
        if texts
            .iter()
            .any(|(t, _)| t.starts_with("Can't reach gpu-01"))
        {
            assert!(texts.iter().any(|(t, _)| t == "Retry"), "{texts:?}");
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{texts:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(service.host_scans().len(), 1);
    assert!(h.click_text("Retry", &mut |ui| draw(ui, &mut state)));
    assert_eq!(service.host_scans().len(), 2, "Retry scans again");
}
