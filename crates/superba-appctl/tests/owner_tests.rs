//! Listener ownership parsing and decisions.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use superba_appctl::owner::{
    Listener, OwnerCheck, decide, decide_with_socket_inodes, parse_lsof, parse_proc_net_tcp,
    parse_windows_tcp_table,
};

const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:AECD 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1012        0 9123456 1 0000000000000000 100 0 0 10 0
   1: 00000000:AECD 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1012        0 9123457 1 0000000000000000 100 0 0 10 0
   2: 0100007F:AECD 0100007F:D2F0 01 00000000:00000000 00:00000000 00000000  1012        0 9123458 1 0000000000000000 20 4 30 10 -1
   3: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1012        0 777 1 0000000000000000 100 0 0 10 0
";

const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:AECD 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1012        0 555 1 0000000000000000 100 0 0 10 0
   1: 0000000000000000FFFF00000100007F:AECD 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1012        0 556 1 0000000000000000 100 0 0 10 0
";

#[test]
fn proc_net_tcp_ipv4() {
    let ls = parse_proc_net_tcp(TCP, 0xAECD);
    assert_eq!(ls.len(), 2, "{ls:?}");
    assert_eq!(ls[0].addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(ls[0].inode, 9123456);
    assert!(ls[0].is_loopback());
    assert_eq!(ls[1].addr, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert!(!ls[1].is_loopback());
    assert_eq!(parse_proc_net_tcp(TCP, 8080).len(), 1);
    assert!(parse_proc_net_tcp(TCP, 1).is_empty());
    assert!(parse_proc_net_tcp("garbage\nmore garbage", 1).is_empty());
}

#[test]
fn proc_net_tcp_ipv6() {
    let ls = parse_proc_net_tcp(TCP6, 0xAECD);
    assert_eq!(ls.len(), 2, "{ls:?}");
    assert_eq!(ls[0].addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert!(ls[0].is_loopback());
    // ::ffff:127.0.0.1
    assert!(ls[1].is_loopback(), "{:?}", ls[1].addr);
}

#[test]
fn lsof_output() {
    let text = "p4242\nf103\nn127.0.0.1:9222\np4300\nf103\nn127.0.0.1:9222\n";
    let ls = parse_lsof(text, 9222);
    assert_eq!(ls.len(), 1);
    assert_eq!(ls[0].pids, vec![4242, 4300]);
    assert!(ls[0].is_loopback());
    let ls = parse_lsof("p1\nn*:9222\np2\nn[::1]:9222\n", 9222);
    assert_eq!(ls.len(), 2);
    assert!(!ls[0].is_loopback());
    assert!(ls[1].is_loopback());
}

#[test]
fn windows_ipv4_listener_table() {
    let addr = u32::from_ne_bytes([127, 0, 0, 1]);
    let port = u32::from(9222_u16.to_be());
    let words = [
        4,
        2,
        addr,
        port,
        0,
        0,
        1234,
        5,
        addr,
        port,
        0,
        0,
        1234, // Established, not listening.
        2,
        addr,
        u32::from(135_u16.to_be()),
        0,
        0,
        900,
        2,
        0,
        port,
        0,
        0,
        5678,
    ];
    let ls = parse_windows_tcp_table(&words, false, 9222).unwrap();
    assert_eq!(ls.len(), 2);
    assert_eq!(ls[0].addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(ls[0].pids, vec![1234]);
    assert_eq!(ls[1].addr, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(ls[1].pids, vec![5678]);
    assert_eq!(decide(&ls, 1234), OwnerCheck::NotLoopback);
    assert!(
        parse_windows_tcp_table(&words, false, 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn windows_ipv6_listener_table_and_network_byte_order() {
    let row = |addr: Ipv6Addr, pid: u32| {
        let mut words: Vec<u32> = addr
            .octets()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_ne_bytes(*chunk))
            .collect();
        // Only the low 16 bits of the port DWORD are significant.
        words.extend([
            7,
            0xabcd_0000 | u32::from(9222_u16.to_be()),
            0,
            0,
            0,
            0,
            0,
            0,
            2,
            pid,
        ]);
        words
    };
    let mut words = vec![2];
    words.extend(row(Ipv6Addr::LOCALHOST, 1234));
    words.extend(row(Ipv4Addr::LOCALHOST.to_ipv6_mapped(), 4321));
    let ls = parse_windows_tcp_table(&words, true, 9222).unwrap();
    assert_eq!(ls.len(), 2);
    assert_eq!(ls[0].addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(ls[0].pids, vec![1234]);
    assert_eq!(ls[1].pids, vec![4321]);
    assert!(ls.iter().all(Listener::is_loopback));
}

#[test]
fn windows_listener_tables_reject_truncation() {
    for ipv6 in [false, true] {
        assert_eq!(parse_windows_tcp_table(&[0], ipv6, 1), Some(vec![]));
        assert!(parse_windows_tcp_table(&[], ipv6, 1).is_none());
        assert!(parse_windows_tcp_table(&[1, 2, 0], ipv6, 1).is_none());
        assert!(parse_windows_tcp_table(&[u32::MAX], ipv6, 1).is_none());
    }
}

#[test]
fn readable_candidate_fds_rule_out_hidden_listener_owners() {
    let listeners = parse_proc_net_tcp(TCP, 0xAECD);
    let loopback = &listeners[..1];
    assert_eq!(
        decide_with_socket_inodes(loopback, 5, None),
        OwnerCheck::Unknown
    );
    for inodes in [&[][..], &[777][..]] {
        assert_eq!(
            decide_with_socket_inodes(loopback, 5, Some(inodes)),
            OwnerCheck::Mismatch { owners: vec![] }
        );
    }
    assert_eq!(
        decide_with_socket_inodes(loopback, 5, Some(&[9123456])),
        OwnerCheck::Verified
    );
    assert_eq!(
        decide_with_socket_inodes(&listeners, 5, Some(&[9123456])),
        OwnerCheck::NotLoopback
    );
    assert_eq!(
        decide_with_socket_inodes(&[], 5, Some(&[])),
        OwnerCheck::NotListening
    );
}

#[test]
fn decisions() {
    let lo = |pids: Vec<u32>| Listener {
        addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 1,
        inode: 1,
        pids,
    };
    assert_eq!(decide(&[], 5), OwnerCheck::NotListening);
    assert_eq!(decide(&[lo(vec![5, 6])], 5), OwnerCheck::Verified);
    assert_eq!(
        decide(&[lo(vec![6])], 5),
        OwnerCheck::Mismatch { owners: vec![6] }
    );
    assert_eq!(decide(&[lo(vec![])], 5), OwnerCheck::Unknown);
    let any = Listener {
        addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        ..lo(vec![5])
    };
    assert_eq!(decide(&[lo(vec![5]), any], 5), OwnerCheck::NotLoopback);
}

#[cfg(target_os = "linux")]
#[test]
fn live_listener_owned_by_this_process() {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    assert_eq!(
        superba_appctl::owner::check_owner(port, std::process::id()),
        OwnerCheck::Verified
    );
    assert!(matches!(
        superba_appctl::owner::check_owner(port, 1),
        OwnerCheck::Mismatch { .. }
    ));
    drop(l);
    assert_eq!(
        superba_appctl::owner::check_owner(port, std::process::id()),
        OwnerCheck::NotListening
    );
}
