//! Linux: a private D-Bus session bus with a minimal StatusNotifierWatcher
//! (the part every tray host provides), for the tray tests and the
//! screenshots: start `dbus-daemon`, own `org.kde.StatusNotifierWatcher`,
//! read an item's dbusmenu (`GetLayout`) and click its entries (`Event`) the
//! way KDE Plasma or the GNOME AppIndicator extension do.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

/// `dbus-daemon` is installed (tests skip without it).
pub fn have_dbus_daemon() -> bool {
    Command::new("dbus-daemon")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A private session bus; stopped on drop.
pub struct Bus {
    child: Child,
    /// Its address (`DBUS_SESSION_BUS_ADDRESS`).
    pub address: String,
    _dir: PathBuf,
}

impl Bus {
    /// Start `dbus-daemon --session` listening in `dir`.
    pub fn start(dir: &Path) -> Bus {
        let socket = dir.join("bus");
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon starts");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .expect("dbus-daemon prints its address");
        Bus {
            child,
            address: line.trim().to_owned(),
            _dir: dir.to_path_buf(),
        }
    }

    /// A new connection to this bus.
    pub fn connect(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .expect("connect to the private bus")
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The StatusNotifierWatcher: remembers the items that register.
#[derive(Clone, Default)]
pub struct Watcher {
    items: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        // An item registers its bus name, or an object path on its unique name.
        let name = if service.starts_with('/') {
            header.sender().map(|s| s.to_string()).unwrap_or_default()
        } else {
            service.to_owned()
        };
        self.items.lock().unwrap().push(name.clone());
        let _ = Self::status_notifier_item_registered(&emitter, &name).await;
    }

    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// A running watcher; it leaves the bus on drop (hosts going away).
pub struct Host {
    conn: Connection,
    watcher: Watcher,
}

impl Host {
    /// Own `org.kde.StatusNotifierWatcher` on `bus`.
    pub fn start(bus: &Bus) -> Host {
        Self::on_address(&bus.address)
    }

    /// Own `org.kde.StatusNotifierWatcher` on the bus at `address`.
    pub fn on_address(address: &str) -> Host {
        let watcher = Watcher::default();
        let conn = zbus::blocking::connection::Builder::address(address)
            .unwrap()
            .serve_at("/StatusNotifierWatcher", watcher.clone())
            .unwrap()
            .name("org.kde.StatusNotifierWatcher")
            .unwrap()
            .build()
            .expect("the watcher owns its name");
        Host { conn, watcher }
    }

    /// Items registered so far (bus names).
    pub fn items(&self) -> Vec<String> {
        self.watcher.items.lock().unwrap().clone()
    }

    /// Wait for the `n`-th item (1-based) to register; its bus name.
    pub fn wait_item(&self, n: usize, timeout: Duration) -> String {
        let start = Instant::now();
        loop {
            if let Some(name) = self.items().get(n - 1) {
                return name.clone();
            }
            assert!(start.elapsed() < timeout, "no tray item registered");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The connection (to read menus and click).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// One dbusmenu entry.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuNode {
    /// dbusmenu id.
    pub id: i32,
    /// Label as sent (`_` mnemonics escaped).
    pub label: String,
    /// Clickable.
    pub enabled: bool,
    /// A separator.
    pub separator: bool,
    /// Check mark state of a check item.
    pub checked: Option<bool>,
    /// Submenu entries.
    pub children: Vec<MenuNode>,
}

impl MenuNode {
    /// The entry labelled `label` (depth first).
    pub fn find(&self, label: &str) -> Option<&MenuNode> {
        if self.label == label {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(label))
    }

    /// The menu as indented text (evidence).
    pub fn dump(&self) -> String {
        fn walk(n: &MenuNode, depth: usize, out: &mut String) {
            for c in &n.children {
                let mark = match (c.separator, c.checked) {
                    (true, _) => "----------".to_owned(),
                    (_, Some(true)) => format!("[x] {}", c.label),
                    (_, Some(false)) => format!("[ ] {}", c.label),
                    _ => c.label.clone(),
                };
                let state = if c.enabled || c.separator {
                    ""
                } else {
                    "  (disabled)"
                };
                out.push_str(&format!("{}{mark}{state}\n", "  ".repeat(depth)));
                walk(c, depth + 1, out);
            }
        }
        let mut out = String::new();
        walk(self, 0, &mut out);
        out
    }
}

type Props = HashMap<String, OwnedValue>;

fn node(id: i32, props: &Props, children: &[OwnedValue]) -> MenuNode {
    let get = |k: &str| props.get(k).and_then(|v| v.try_clone().ok());
    MenuNode {
        id,
        label: get("label")
            .and_then(|v| String::try_from(v).ok())
            .unwrap_or_default(),
        enabled: get("enabled")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(true),
        separator: get("type")
            .and_then(|v| String::try_from(v).ok())
            .is_some_and(|t| t == "separator"),
        checked: get("toggle-type")
            .and_then(|v| String::try_from(v).ok())
            .filter(|t| t == "checkmark")
            .map(|_| {
                get("toggle-state")
                    .and_then(|v| i32::try_from(v).ok())
                    .unwrap_or(0)
                    == 1
            }),
        children: children.iter().map(child).collect(),
    }
}

fn child(value: &OwnedValue) -> MenuNode {
    let value: Value<'_> = value.try_clone().unwrap().into();
    let (id, props, children): (i32, Props, Vec<OwnedValue>) =
        value.try_into().expect("a dbusmenu layout entry");
    node(id, &props, &children)
}

/// The item's whole menu (`com.canonical.dbusmenu.GetLayout`) and its revision.
pub fn menu(conn: &Connection, item: &str) -> (u32, MenuNode) {
    let reply = conn
        .call_method(
            Some(item),
            "/MenuBar",
            Some("com.canonical.dbusmenu"),
            "GetLayout",
            &(0i32, -1i32, Vec::<String>::new()),
        )
        .expect("GetLayout");
    let (revision, (id, props, children)): (u32, (i32, Props, Vec<OwnedValue>)) =
        reply.body().deserialize().expect("a dbusmenu layout");
    (revision, node(id, &props, &children))
}

/// Click entry `id` as a host does (`Event(id, "clicked", 0, 0)`).
pub fn click(conn: &Connection, item: &str, id: i32) {
    conn.call_method(
        Some(item),
        "/MenuBar",
        Some("com.canonical.dbusmenu"),
        "Event",
        &(id, "clicked", Value::from(0i32), 0u32),
    )
    .expect("Event");
}

/// A left click on the icon (`org.kde.StatusNotifierItem.Activate`).
pub fn activate(conn: &Connection, item: &str) {
    conn.call_method(
        Some(item),
        "/StatusNotifierItem",
        Some("org.kde.StatusNotifierItem"),
        "Activate",
        &(0i32, 0i32),
    )
    .expect("Activate");
}

/// A StatusNotifierItem tooltip: icon name, icon pixmaps (width, height,
/// ARGB32), title, description.
type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

/// The item's tooltip description (`ToolTip` property).
pub fn tooltip(conn: &Connection, item: &str) -> String {
    let reply = conn
        .call_method(
            Some(item),
            "/StatusNotifierItem",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.kde.StatusNotifierItem", "ToolTip"),
        )
        .expect("ToolTip");
    let value: OwnedValue = reply.body().deserialize().expect("a variant");
    let (_, _, _, description): ToolTip = Value::from(value).try_into().expect("a tooltip");
    description
}
