//! Detail of one host: endpoint, OS / architecture and probe age; its
//! connection with the "switch with this computer" toggle; its Codex (version,
//! path, install kind, update hint); the settings sync summary; sessions; and
//! whether a hot restart is possible. A WSL distribution (user bug 45) shows its
//! WSL version and networking instead of an SSH endpoint, follows this computer
//! by default and has a local-router row (router access for WSL 2 behind NAT).
//! A host Superba does not manage (a remote-control device; user bug 36) gets a
//! read-only pane instead: its kind, why nothing can be done here, and a copy
//! button for the app's host id.

use egui::{Align, Layout, Ui};
use superba_core::{
    AuthShape, CodexInstallKind, CodexVersion, HostKind, HostSyncAction, HostSyncPlan, HostView,
    RemoteOs, RemoteState, Site,
};

use super::State;
use crate::design::icons::Icon;
use crate::design::widgets::{Badge, Button, FormRow, Status, Text, Toggle, link_button};
use crate::pages::shared::hosts as names;
use crate::pages::{Action, Cx, Page, divider, error_text, kv, note};

/// Counts of a sync preview: (local settings, the host takes, need a newer
/// Codex, removals).
pub fn fit_counts(plan: &HostSyncPlan) -> (usize, usize, usize, usize) {
    let mut ok = 0;
    let mut skip = 0;
    let mut remove = 0;
    for r in &plan.rows {
        match r.action {
            HostSyncAction::Write | HostSyncAction::Conflict => ok += 1,
            HostSyncAction::Skip { .. } => skip += 1,
            HostSyncAction::Remove => remove += 1,
            _ => {}
        }
    }
    (ok + skip, ok, skip, remove)
}

/// `Linux x86_64` / `macOS arm64`.
pub fn os_text(state: &RemoteState) -> String {
    let os = match &state.os {
        RemoteOs::Linux => "Linux".to_owned(),
        RemoteOs::Darwin => "macOS".to_owned(),
        RemoteOs::Other(o) => o.clone(),
    };
    match &state.arch {
        Some(a) => format!("{os} {a}"),
        None => os,
    }
}

/// Install kind label.
pub fn kind_text(cx: &Cx<'_>, kind: Option<CodexInstallKind>) -> Option<String> {
    Some(match kind? {
        CodexInstallKind::Standalone => cx.tr.t("hosts.kind.standalone"),
        CodexInstallKind::Npm => cx.tr.t("hosts.kind.npm"),
        k => cx.tr.text(&k.text()),
    })
}

/// What the host's config points at: `Krill · Overseas · API key`, the line
/// host, or the official login.
pub fn connection_text(cx: &Cx<'_>, sites: &[Site], state: &RemoteState) -> String {
    let auth = match &state.auth {
        AuthShape::ApiKey { .. } => Some(cx.tr.t("auth.short.apikey")),
        AuthShape::Pat { .. } => Some(cx.tr.t("auth.short.pat")),
        AuthShape::ChatGpt { .. } => Some(cx.tr.t("auth.short.chatgpt")),
        AuthShape::Missing => None,
        _ => None,
    };
    let base = state
        .openai_base_url
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok());
    let mut parts: Vec<String> = Vec::new();
    if let Some(url) = &base {
        let host = url.host_str().unwrap_or_default();
        let found = sites.iter().find_map(|s| {
            s.lines
                .iter()
                .find(|l| l.origin.host() == host)
                .map(|l| (cx.tr.text(&s.name), cx.tr.text(&l.name)))
        });
        match found {
            Some((site, line)) => {
                parts.push(site);
                parts.push(line);
            }
            None => parts.push(host.to_owned()),
        }
    }
    if let Some(a) = auth {
        parts.push(a);
    }
    if parts.is_empty() {
        cx.tr.t("hosts.connection_unknown")
    } else {
        parts.join(" · ")
    }
}

/// Newest stable Codex known from the version data.
pub fn latest_stable(state: &State) -> Option<CodexVersion> {
    let v = state.versions.value.as_ref()?;
    v.app_codex
        .candidates
        .iter()
        .filter(|c| !c.release.prerelease)
        .map(|c| c.release.version.clone())
        .max()
        .or_else(|| v.cli.iter().filter_map(|r| r.latest.clone()).max())
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(h) = state.current().cloned() else {
        return;
    };
    if !h.host.is_managed() {
        read_only(ui, cx, &h, out);
        return;
    }
    let id = h.host.target_id.clone();
    let connected = h.host.connected == Some(true);
    let probed = h.state.is_some();
    let wsl = h.host.kind == HostKind::Wsl;
    // Title.
    ui.horizontal(|ui| {
        ui.label(Text::title(names::title(cx.tr, &h.host), cx.pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut sources = Vec::new();
            if h.host.is_app_known() && !wsl {
                sources.push(cx.tr.t("hosts.app_record"));
            }
            let (status, key) = if connected {
                (Status::Ok, "hosts.connected")
            } else {
                (Status::Idle, "hosts.not_connected")
            };
            let mut text = if wsl && !connected {
                names::kind(cx.tr, h.host.kind)
            } else {
                cx.tr.t(key)
            };
            if !sources.is_empty() {
                text = format!("{text} · {}", sources.join(" · "));
            }
            ui.add(Badge::new(text).status(status));
        });
    });
    // Endpoint · OS · probe age, on the center line of the probe button.
    FormRow::unlabeled().show(ui, |ui| {
        let mut parts = vec![names::wsl_facts(cx.tr, &h.host).unwrap_or_else(|| endpoint(&h))];
        if let Some(s) = &h.state {
            parts.push(os_text(s));
            parts.push(cx.tr.f("hosts.probed", &[("age", &cx.ago(s.probed_at))]));
        }
        ui.label(Text::mono_muted(parts.join(" · "), cx.pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let probing = state.probing(&id);
            if ui
                .add(
                    Button::ghost(cx.tr.t("hosts.probe"))
                        .small()
                        .loading(probing)
                        .enabled(!probing),
                )
                .clicked()
            {
                state.probe(cx, id.clone());
            }
        });
    });
    if let Some(e) = &h.probe_error {
        note(
            ui,
            cx,
            Status::Warn,
            cx.tr
                .f("hosts.probe_failed", &[("detail", &error_text(cx.tr, e))]),
        );
    }
    ui.add_space(4.0);
    divider(ui, cx);
    ui.add_space(4.0);

    // Connection + follow toggle.
    kv(ui, cx, &cx.tr.t("hosts.connection"), |ui| {
        let text = match &h.state {
            Some(s) => connection_text(cx, &cx.data.sites, s),
            None => cx.tr.t("hosts.not_probed"),
        };
        ui.label(Text::small(text, cx.pal).color(cx.pal.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut on = h.settings.follows(h.host.kind);
            let tip = cx.tr.t(if wsl {
                "hosts.wsl.follow_tip"
            } else {
                "hosts.follow_tip"
            });
            let r = ui
                .add(Toggle::new(&mut on, cx.tr.t("hosts.follow")))
                .on_hover_text(tip.clone());
            ui.label(Text::caption(cx.tr.t("hosts.follow"), cx.pal))
                .on_hover_text(tip);
            if r.changed() {
                state.set_follow(cx, id.clone(), on);
            }
        });
    });
    // Codex.
    let latest = latest_stable(state);
    kv(ui, cx, &cx.tr.t("hosts.codex"), |ui| {
        let Some(s) = &h.state else {
            ui.label(Text::small(cx.tr.t("hosts.not_probed"), cx.pal));
            return;
        };
        let mut parts = vec![
            s.codex_version
                .as_ref()
                .map_or_else(|| cx.tr.t("hosts.codex_missing"), |v| v.to_string()),
        ];
        if let Some(k) = kind_text(cx, s.codex_install) {
            parts.push(k);
        }
        if let Some(p) = s.codex_path.as_ref().or(s.codex_resolved_path.as_ref()) {
            parts.push(p.clone());
        }
        ui.label(Text::mono(parts.join(" · "), cx.pal));
        if let (Some(have), Some(new)) = (&s.codex_version, &latest)
            && new > have
        {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if link_button(
                    ui,
                    &cx.tr.f("hosts.update_hint", &[("version", new)]),
                    false,
                )
                .clicked()
                {
                    out.push(Action::PageCommand {
                        page: Page::Updates,
                        command: format!("focus:{id}"),
                    });
                }
            });
        }
    });
    ui.add_space(4.0);
    divider(ui, cx);
    ui.add_space(4.0);

    // Settings sync.
    let plan_state: Option<(bool, Option<String>)> = state
        .plans
        .get(&id)
        .map(|p| (p.loading(), p.error.as_ref().map(|e| error_text(cx.tr, e))));
    kv(ui, cx, &cx.tr.t("hosts.sync.title"), |ui| {
        let text = match &h.settings.last_sync {
            Some(last) => cx.tr.f(
                "hosts.sync.last",
                &[
                    (
                        "time",
                        &crate::pages::router::requests::date_cell(last.at, cx.now),
                    ),
                    ("n", &h.pending_sync.unwrap_or(0)),
                ],
            ),
            None => cx.tr.t("hosts.sync.never"),
        };
        ui.label(Text::small(text, cx.pal).color(cx.pal.text));
    });
    let fit = state
        .plans
        .get(&id)
        .and_then(|p| p.value.as_ref())
        .map(fit_counts);
    kv(ui, cx, "", |ui| {
        match (fit, plan_state) {
            (Some((local, ok, skip, _)), _) => {
                ui.label(Text::small(
                    cx.tr.f(
                        "hosts.sync.fit",
                        &[("local", &local), ("ok", &ok), ("skip", &skip)],
                    ),
                    cx.pal,
                ));
            }
            (None, Some((true, _))) => {
                ui.label(Text::small(cx.tr.t("hosts.sync.checking"), cx.pal));
            }
            (None, Some((_, Some(e)))) => {
                ui.label(Text::small(e, cx.pal).color(cx.pal.red));
            }
            _ => {
                ui.label(Text::small(
                    cx.tr.t(if probed {
                        "hosts.sync.not_checked"
                    } else {
                        "hosts.sync.needs_probe"
                    }),
                    cx.pal,
                ));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = ui
                .add(
                    Button::new(cx.tr.t("hosts.sync.button"))
                        .small()
                        .enabled(probed && !cx.busy),
                )
                .on_disabled_hover_text(cx.tr.t("hosts.sync.needs_probe"));
            if r.clicked() {
                state.open_sync(cx, id.clone());
            }
        });
    });
    if let Some(p) = state.plans.get(&id).and_then(|p| p.value.as_ref()) {
        for n in &p.notes {
            note(ui, cx, Status::Idle, cx.localize(&n.text));
        }
    }
    ui.add_space(4.0);
    divider(ui, cx);
    ui.add_space(4.0);

    // Sessions.
    kv(ui, cx, &cx.tr.t("hosts.sessions_label"), |ui| {
        if link_button(ui, &cx.tr.t("hosts.sessions.open"), false).clicked() {
            out.push(Action::PageCommand {
                page: Page::Sessions,
                command: format!("host:{id}"),
            });
        }
    });
    if wsl {
        router_row(ui, cx, state, &h);
    }
    // Hot restart.
    kv(ui, cx, &cx.tr.t("hosts.hot_label"), |ui| {
        let (status, key) = match (wsl, h.hot_restart) {
            (true, true) => (Status::Ok, "hosts.wsl.hot_ok"),
            (true, false) => (Status::Idle, "hosts.wsl.hot_next"),
            (false, true) => (Status::Ok, "hosts.hot.ok"),
            (false, false) => (Status::Warn, "hosts.hot.reconnect"),
        };
        ui.add(Badge::new(cx.tr.t(key)).status(status));
    });
    if let Some(s) = &h.state
        && let Some(kib) = s.disk_free_kib
        && kib < 600 * 1024
    {
        note(
            ui,
            cx,
            Status::Warn,
            cx.tr.f("hosts.disk_low", &[("mb", &(kib / 1024))]),
        );
    }
    if let Some(s) = &h.state
        && s.shared_home_suspected()
    {
        note(ui, cx, Status::Warn, cx.tr.t("hosts.shared_home"));
    }
}

/// How a WSL distribution reaches the local router, with the router access
/// button for WSL 2 behind NAT and the last access result.
fn router_row(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, h: &HostView) {
    let id = h.host.target_id.clone();
    let path = h
        .host
        .wsl
        .as_ref()
        .map_or(superba_core::RouterPath::HostAddress, |d| d.router_path());
    let report = state.access_reports.get(&id).cloned();
    kv(ui, cx, &cx.tr.t("hosts.wsl.router_label"), |ui| {
        let text = match (&report, path) {
            (Some(r), _) if r.reachable => r.address.clone().unwrap_or_default(),
            (_, superba_core::RouterPath::Loopback) => cx.tr.t("hosts.wsl.router_loopback"),
            (_, superba_core::RouterPath::Unreachable) => cx.tr.t("hosts.wsl.router_none"),
            _ => cx.tr.t("hosts.wsl.router_nat"),
        };
        ui.label(Text::small(text, cx.pal).color(cx.pal.text));
        if path == superba_core::RouterPath::HostAddress {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let busy = state.accessing(&id);
                let label = if busy {
                    cx.tr.t("hosts.wsl.router_checking")
                } else {
                    cx.tr.t("hosts.wsl.router_access")
                };
                if ui
                    .add(
                        Button::new(label)
                            .small()
                            .loading(busy)
                            .enabled(!busy && !cx.busy),
                    )
                    .clicked()
                {
                    state.router_access(cx, id.clone());
                }
            });
        }
    });
    if let Some(r) = &report {
        for n in &r.notes {
            let status = if n.severity == superba_core::Severity::Warning {
                Status::Warn
            } else {
                Status::Idle
            };
            note(ui, cx, status, cx.localize(&n.text));
        }
    }
}

/// Why Superba cannot manage a host of this kind.
pub fn read_only_text(cx: &Cx<'_>, kind: HostKind) -> String {
    cx.tr.t(match kind {
        HostKind::RemoteControl => "hosts.read_only.remote_control",
        HostKind::Wsl => "hosts.read_only.wsl",
        _ => "hosts.read_only.other",
    })
}

/// A host Superba does not manage: its name and kind, why nothing can be done
/// here, and the app's host id behind a copy button (no probe, follow, sync,
/// install or sessions).
fn read_only(ui: &mut Ui, cx: &Cx<'_>, h: &HostView, out: &mut Vec<Action>) {
    let kind = names::kind(cx.tr, h.host.kind);
    let tip = names::id_tip(cx.tr, &h.host);
    ui.horizontal(|ui| {
        let title = ui.label(Text::title(names::title(cx.tr, &h.host), cx.pal));
        if let Some(tip) = &tip {
            title.on_hover_text(tip);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let text = format!("{kind} · {}", cx.tr.t("hosts.app_record"));
            ui.add(Badge::new(text).status(Status::Idle));
        });
    });
    ui.add_space(4.0);
    note(ui, cx, Status::Idle, read_only_text(cx, h.host.kind));
    ui.add_space(4.0);
    divider(ui, cx);
    ui.add_space(4.0);
    kv(ui, cx, &cx.tr.t("hosts.kind_label"), |ui| {
        ui.label(
            Text::small(cx.tr.f("hosts.read_only.label", &[("kind", &kind)]), cx.pal)
                .color(cx.pal.text),
        );
        if let Some(id) = &h.host.host_id {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut r = ui.add(
                    Button::ghost(cx.tr.t("hosts.copy_id"))
                        .icon(Icon::Copy)
                        .small(),
                );
                if let Some(tip) = &tip {
                    r = r.on_hover_text(tip);
                }
                if r.clicked() {
                    out.push(Action::Copy(id.clone()));
                }
            });
        }
    });
}

/// `alias` or `user@host:port`.
pub fn endpoint(h: &HostView) -> String {
    let e = &h.host.ssh;
    let mut s = String::new();
    if let Some(u) = &e.user {
        s.push_str(u);
        s.push('@');
    }
    s.push_str(
        e.host
            .as_deref()
            .or(e.alias.as_deref())
            .unwrap_or(&h.host.label),
    );
    if let Some(p) = e.port {
        s.push_str(&format!(":{p}"));
    }
    s
}
