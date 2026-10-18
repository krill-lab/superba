//! The router's stable port (user bug 65): the per-data-directory candidates,
//! the choice outside the OS ephemeral ranges, and the bind check.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};

use pretty_assertions::assert_eq;
use superba_router::port::{
    IANA_DYNAMIC, LINUX_DEFAULT_EPHEMERAL, STABLE_PORTS, bindable, candidates, choose,
    ephemeral_ranges, parse_port_range, pick, preferred,
};

#[test]
fn the_window_is_outside_every_default_ephemeral_range() {
    for range in [IANA_DYNAMIC, LINUX_DEFAULT_EPHEMERAL] {
        assert!(
            STABLE_PORTS.end() < range.start(),
            "{STABLE_PORTS:?} overlaps {range:?}"
        );
    }
    // This machine's range (Linux reads ip_local_port_range).
    for range in ephemeral_ranges() {
        assert!(range.start() <= range.end(), "{range:?}");
    }
}

#[test]
fn candidates_visit_the_whole_window_in_a_fixed_order_per_seed() {
    let seed = "/home/alice/.local/share/superba";
    let order = candidates(seed);
    assert_eq!(order.len(), STABLE_PORTS.len());
    assert!(order.iter().all(|p| STABLE_PORTS.contains(p)));
    assert_eq!(order.iter().collect::<BTreeSet<_>>().len(), order.len());
    // Deterministic: the same seed always gives the same order (the first
    // configuration after a reinstall gets the same port again).
    assert_eq!(candidates(seed), order);
    assert_eq!(preferred(seed), order[0]);
    // Spread out, not side by side: a blocked block of ports (Windows keeps
    // ranges of 100 for Hyper-V) does not hold all the next candidates.
    assert!(order[0].abs_diff(order[1]) > 100, "{:?}", &order[..4]);
    // Other users / data directories mostly start elsewhere.
    let firsts: BTreeSet<u16> = (0..50)
        .map(|i| preferred(&format!("/home/user{i}/.local/share/superba")))
        .collect();
    assert!(firsts.len() > 40, "{firsts:?}");
}

#[test]
fn choose_takes_the_first_free_port_and_prefers_ports_outside_the_ephemeral_ranges() {
    let seed = "C:\\Users\\bob\\AppData\\Roaming\\Superba";
    let order = candidates(seed);
    let none: Vec<std::ops::RangeInclusive<u16>> = Vec::new();
    // Nothing in the way: the preferred port.
    assert_eq!(choose(seed, &none, &[], |_| true), Some(order[0]));
    // Busy candidates are skipped, in order.
    let busy = [order[0], order[1]];
    assert_eq!(
        choose(seed, &none, &[], |p| !busy.contains(&p)),
        Some(order[2])
    );
    // The port being left is never chosen again.
    assert_eq!(choose(seed, &none, &[order[0]], |_| true), Some(order[1]));
    // A widened ephemeral range over part of the window: ports outside it
    // first, even when the preferred one is free.
    let ephemeral = [order[0]..=order[0], order[1]..=order[1]];
    assert_eq!(choose(seed, &ephemeral, &[], |_| true), Some(order[2]));
    // Over the whole window: the candidates inside it, still in order.
    let everything = 1024..=65_535;
    assert_eq!(
        choose(seed, std::slice::from_ref(&everything), &[], |_| true),
        Some(order[0])
    );
    // Every port taken.
    assert_eq!(choose(seed, &none, &[], |_| false), None);
}

#[test]
fn ip_local_port_range_is_parsed_strictly() {
    assert_eq!(parse_port_range("32768\t60999\n"), Some(32_768..=60_999));
    assert_eq!(parse_port_range(" 1024 65535 "), Some(1_024..=65_535));
    assert_eq!(parse_port_range("60999 32768"), None, "reversed");
    assert_eq!(parse_port_range("32768"), None);
    assert_eq!(parse_port_range("1 2 3"), None);
    assert_eq!(parse_port_range("a b"), None);
    assert_eq!(parse_port_range("32768 70000"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_reads_the_configured_ephemeral_range() {
    let text = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap();
    assert_eq!(ephemeral_ranges(), vec![parse_port_range(&text).unwrap()]);
}

#[test]
fn bindable_sees_a_port_another_program_listens_on() {
    let held = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
    let port = held.local_addr().unwrap().port();
    assert!(!bindable(port, &[]));
    drop(held);
    // An address this computer does not have (TEST-NET-1) does not count: the
    // router binds a WSL address once it exists.
    let free = pick("bindable-test", &[], &[]).expect("a free port");
    assert!(bindable(free, &[Ipv4Addr::new(192, 0, 2, 1)]));
}

#[test]
fn pick_returns_a_free_port_of_the_window_outside_the_ephemeral_ranges() {
    let seed = "pick-test";
    let port = pick(seed, &[], &[]).expect("a free port");
    assert!(STABLE_PORTS.contains(&port));
    assert!(!ephemeral_ranges().iter().any(|r| r.contains(&port)));
    // Taken by another program: the next candidate.
    let held = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).unwrap();
    let next = pick(seed, &[], &[]).expect("another free port");
    assert_ne!(next, port);
    drop(held);
    assert_ne!(pick(seed, &[port], &[]), Some(port), "a port being left");
}
