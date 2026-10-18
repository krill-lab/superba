#![allow(missing_docs)]
#![cfg(target_os = "linux")]

//! Item 70 on Linux: the StatusNotifierItem through ksni, against a private
//! session bus and a minimal StatusNotifierWatcher (what KDE Plasma, the
//! GNOME AppIndicator extension, Xfce, ... provide). Skipped without
//! `dbus-daemon`.
//!
//! One test (the session bus address is a process-wide environment
//! variable): no host → no tray (closing quits); a host → the item
//! registers with the menu of the model (labels, check marks, submenus,
//! disabled lines), clicks and Activate come back as tray events, a new menu
//! and tooltip reach the host, and a host that goes away and comes back is
//! reported.

#[path = "support/sni_host.rs"]
mod sni_host;

use std::time::{Duration, Instant};

use sni_host::{Bus, Host, MenuNode};
use superba_core::{LineId, ProfileId};
use superba_gui::tray::{
    HideSupport, Tray, TrayAction, TrayEvent, TrayItem, TrayMenu, TrayStatus, Unavailable,
};

fn model() -> TrayMenu {
    let action =
        |label: &str, action: TrayAction, enabled: bool, checked: Option<bool>| TrayItem::Action {
            label: label.into(),
            action,
            enabled,
            checked,
        };
    let switch = |line: &str| TrayAction::Switch {
        profile: ProfileId::new("krill"),
        line: Some(LineId::new(line)),
    };
    TrayMenu {
        items: vec![
            action("Open Superba", TrayAction::Open, true, None),
            TrayItem::Separator,
            TrayItem::Label("Krill · Overseas".into()),
            TrayItem::Submenu {
                label: "Switch to".into(),
                enabled: true,
                items: vec![
                    TrayItem::Submenu {
                        label: "Krill (in use)".into(),
                        enabled: true,
                        items: vec![
                            action("China Express", switch("cn-fast"), true, Some(false)),
                            action("Overseas", switch("overseas"), false, Some(true)),
                        ],
                    },
                    action(
                        "my_relay",
                        TrayAction::Switch {
                            profile: ProfileId::new("relay"),
                            line: None,
                        },
                        true,
                        Some(false),
                    ),
                ],
            },
            TrayItem::Separator,
            action("Quit Superba", TrayAction::Quit, true, None),
        ],
    }
}

fn wait_events(tray: &mut Tray, want: &TrayEvent) -> Vec<TrayEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        seen.extend(tray.take_events());
        if seen.contains(want) {
            return seen;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{want:?} not seen; got {seen:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn labels(n: &MenuNode) -> Vec<String> {
    n.children.iter().map(|c| c.label.clone()).collect()
}

#[test]
fn the_status_notifier_item_on_a_session_bus() {
    if !sni_host::have_dbus_daemon() {
        eprintln!("skipped: no dbus-daemon");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bus = Bus::start(dir.path());
    // SAFETY: the only test of this binary; set before any thread of ours
    // reads the environment.
    unsafe {
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.address);
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var(superba_gui::tray::ENV_SUPERBA_TRAY);
    }
    let ctx = egui::Context::default();

    // No StatusNotifierWatcher: no tray, closing quits.
    let tray = Tray::start(&ctx, model(), "Superba · Krill".into());
    assert_eq!(tray.status(), &TrayStatus::Unavailable(Unavailable::NoHost));
    drop(tray);

    // A host: the item registers and serves the menu of the model.
    let host = Host::start(&bus);
    let mut tray = Tray::start(&ctx, model(), "Superba · Krill · Overseas".into());
    assert_eq!(tray.status(), &TrayStatus::Shown(HideSupport::InPlace));
    let item = host.wait_item(1, Duration::from_secs(10));
    assert!(item.starts_with("org.kde.StatusNotifierItem-"), "{item}");
    let (revision, root) = sni_host::menu(host.conn(), &item);
    println!("{}", root.dump());
    assert_eq!(
        labels(&root),
        [
            "Open Superba",
            "",
            "Krill · Overseas",
            "Switch to",
            "",
            "Quit Superba"
        ]
    );
    assert!(root.children[1].separator && root.children[4].separator);
    // The line of information is not clickable.
    assert!(!root.children[2].enabled);
    let switch = &root.children[3];
    let krill = &switch.children[0];
    assert_eq!(krill.label, "Krill (in use)");
    assert_eq!(
        krill
            .children
            .iter()
            .map(|c| (c.label.as_str(), c.enabled, c.checked))
            .collect::<Vec<_>>(),
        [
            ("China Express", true, Some(false)),
            ("Overseas", false, Some(true))
        ]
    );
    // dbusmenu underscores mark access keys: a name's own underscore is doubled.
    assert_eq!(switch.children[1].label, "my__relay");
    assert_eq!(
        sni_host::tooltip(host.conn(), &item),
        "Superba · Krill · Overseas"
    );

    // Clicks come back as actions; Activate (left click) opens the window.
    let quit = root.find("Quit Superba").unwrap().id;
    sni_host::click(host.conn(), &item, quit);
    wait_events(&mut tray, &TrayEvent::Action(TrayAction::Quit));
    let express = root.find("China Express").unwrap().id;
    sni_host::click(host.conn(), &item, express);
    wait_events(
        &mut tray,
        &TrayEvent::Action(TrayAction::Switch {
            profile: ProfileId::new("krill"),
            line: Some(LineId::new("cn-fast")),
        }),
    );
    sni_host::activate(host.conn(), &item);
    wait_events(&mut tray, &TrayEvent::Activate);

    // A new menu and tooltip reach the host.
    let mut next = model();
    next.items[2] = TrayItem::Label("Krill · China Express".into());
    tray.update(next, "Superba · Krill · China Express".into());
    let start = Instant::now();
    loop {
        // A changed label is a property update (the layout revision may stay).
        let (rev, root) = sni_host::menu(host.conn(), &item);
        if rev >= revision && root.children[2].label == "Krill · China Express" {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "menu not updated"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        sni_host::tooltip(host.conn(), &item),
        "Superba · Krill · China Express"
    );

    // The host goes away (the extension turned off) and comes back.
    drop(host);
    wait_events(&mut tray, &TrayEvent::HostLost);
    assert_eq!(tray.status(), &TrayStatus::Unavailable(Unavailable::NoHost));
    let host = Host::start(&bus);
    wait_events(&mut tray, &TrayEvent::HostBack);
    assert_eq!(tray.status(), &TrayStatus::Shown(HideSupport::InPlace));
    // The item registered again with the new host.
    let again = host.wait_item(1, Duration::from_secs(10));
    assert_eq!(again, item);
    drop(tray);
}
