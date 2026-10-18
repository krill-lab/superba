//! A minimal StatusNotifierItem host for the tray screenshots and manual
//! checks on Linux (`tools/screenshots/tray_shots.py`): owns
//! `org.kde.StatusNotifierWatcher` on the bus at ADDRESS, prints the items
//! that register, reads an item's menu and clicks its entries the way a
//! desktop's tray does.
//!
//! ```text
//! sni_host serve ADDRESS                 # until killed; one line per item
//! sni_host menu ADDRESS ITEM             # the menu as indented text
//! sni_host click ADDRESS ITEM LABEL      # click the entry labelled LABEL
//! sni_host activate ADDRESS ITEM         # a left click on the icon
//! sni_host tooltip ADDRESS ITEM
//! ```

#[cfg(target_os = "linux")]
#[path = "../tests/support/sni_host.rs"]
mod sni_host;

#[cfg(target_os = "linux")]
fn main() {
    use std::io::Write;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = || -> ! {
        eprintln!("usage: sni_host serve|menu|click|activate|tooltip ADDRESS [ITEM [LABEL]]");
        std::process::exit(2)
    };
    let (Some(cmd), Some(address)) = (args.first(), args.get(1)) else {
        usage()
    };
    let bus = || {
        zbus::blocking::connection::Builder::address(address.as_str())
            .expect("a bus address")
            .build()
            .expect("connect")
    };
    match (cmd.as_str(), args.get(2), args.get(3)) {
        ("serve", None, None) => {
            let host = sni_host::Host::on_address(address);
            let mut seen = 0;
            loop {
                let items = host.items();
                for item in &items[seen..] {
                    println!("{item}");
                    let _ = std::io::stdout().flush();
                }
                seen = items.len();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        ("menu", Some(item), None) => print!("{}", sni_host::menu(&bus(), item).1.dump()),
        ("click", Some(item), Some(label)) => {
            let conn = bus();
            let (_, root) = sni_host::menu(&conn, item);
            let node = root.find(label).unwrap_or_else(|| {
                eprintln!("no entry {label:?} in\n{}", root.dump());
                std::process::exit(1)
            });
            sni_host::click(&conn, item, node.id);
        }
        ("activate", Some(item), None) => sni_host::activate(&bus(), item),
        ("tooltip", Some(item), None) => println!("{}", sni_host::tooltip(&bus(), item)),
        _ => usage(),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("sni_host: Linux only");
    std::process::exit(2);
}
