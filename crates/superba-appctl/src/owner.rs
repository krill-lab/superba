//! Which process listens on a loopback TCP port (plan 12.1 item 4: verify the CDP
//! listener is the app main process before connecting).
//!
//! Linux reads `/proc/net/tcp{,6}` and `/proc/<pid>/fd`; macOS parses `lsof`;
//! Windows uses `GetExtendedTcpTable`. The parsers are pure and compiled
//! everywhere so they are tested on Linux.
//!
//! Note (verified on Linux, app 26.1007): the app-server child inherits the CDP
//! listening socket, so several pids may hold it; the check is that the app main
//! pid is one of them and that the socket is bound to loopback only.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use superba_core::Timestamp;

use crate::codexbin::{Caches, lock};

/// A listening socket found for a port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    /// Bound address.
    pub addr: IpAddr,
    /// Port.
    pub port: u16,
    /// Socket inode (Linux) or 0.
    pub inode: u64,
    /// Owning pids when known.
    pub pids: Vec<u32>,
}

impl Listener {
    /// True if bound to a loopback address.
    pub fn is_loopback(&self) -> bool {
        match self.addr {
            IpAddr::V4(a) => a.is_loopback(),
            IpAddr::V6(a) => {
                a.is_loopback() || a.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
            }
        }
    }
}

/// Result of an ownership check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerCheck {
    /// The expected pid holds a loopback-only listener on the port.
    Verified,
    /// Nobody listens on the port.
    NotListening,
    /// Listening, but not (only) on loopback.
    NotLoopback,
    /// Listening, but the expected pid is not among the owners.
    Mismatch {
        /// Owners that were found.
        owners: Vec<u32>,
    },
    /// This platform/permission level cannot tell.
    Unknown,
}

fn parse_hex_ipv4(hex: &str) -> Option<Ipv4Addr> {
    let v = u32::from_str_radix(hex, 16).ok()?;
    // /proc/net/tcp prints the address as a host-order (little-endian) u32.
    Some(Ipv4Addr::from(v.swap_bytes()))
}

fn parse_hex_ipv6(hex: &str) -> Option<Ipv6Addr> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for word in 0..4 {
        let w = u32::from_str_radix(&hex[word * 8..word * 8 + 8], 16).ok()?;
        bytes[word * 4..word * 4 + 4].copy_from_slice(&w.swap_bytes().to_be_bytes());
    }
    Some(Ipv6Addr::from(bytes))
}

/// Parse `/proc/net/tcp` or `/proc/net/tcp6` and return LISTEN sockets on `port`.
pub fn parse_proc_net_tcp(text: &str, port: u16) -> Vec<Listener> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 || cols[3] != "0A" {
            continue;
        }
        let Some((addr_hex, port_hex)) = cols[1].split_once(':') else {
            continue;
        };
        if u16::from_str_radix(port_hex, 16).ok() != Some(port) {
            continue;
        }
        let addr = if addr_hex.len() == 8 {
            parse_hex_ipv4(addr_hex).map(IpAddr::V4)
        } else {
            parse_hex_ipv6(addr_hex).map(IpAddr::V6)
        };
        let (Some(addr), Ok(inode)) = (addr, cols[9].parse::<u64>()) else {
            continue;
        };
        out.push(Listener {
            addr,
            port,
            inode,
            pids: Vec::new(),
        });
    }
    out
}

fn split_host_port(s: &str) -> Option<(IpAddr, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.split('%').next().unwrap_or(host);
    let addr = if host == "*" {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        host.parse().ok()?
    };
    Some((addr, port.parse().ok()?))
}

/// Parse `lsof -nP -iTCP:<port> -sTCP:LISTEN -Fpn` output (`p<pid>` / `n<addr>:<port>`).
pub fn parse_lsof(text: &str, port: u16) -> Vec<Listener> {
    let mut out: Vec<Listener> = Vec::new();
    let mut pid: Option<u32> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.trim().parse().ok();
        } else if let Some(n) = line.strip_prefix('n') {
            let Some((addr, p)) = split_host_port(n.trim()) else {
                continue;
            };
            if p != port {
                continue;
            }
            match out.iter_mut().find(|l| l.addr == addr) {
                Some(l) => {
                    if let Some(pid) = pid
                        && !l.pids.contains(&pid)
                    {
                        l.pids.push(pid);
                    }
                }
                None => out.push(Listener {
                    addr,
                    port,
                    inode: 0,
                    pids: pid.into_iter().collect(),
                }),
            }
        }
    }
    out
}

/// Parse a native Windows OWNER_PID TCP table represented as DWORDs. The table
/// starts with a row count; IPv4 rows have six DWORDs and IPv6 rows have fourteen.
/// Addresses and ports retain network byte order, unlike the count/state/PID.
/// The Windows adapter checks these layouts against the SDK types at compile time.
pub fn parse_windows_tcp_table(words: &[u32], ipv6: bool, port: u16) -> Option<Vec<Listener>> {
    let (&count, rows) = words.split_first()?;
    let width = if ipv6 { 14 } else { 6 };
    let rows = rows.get(..(count as usize).checked_mul(width)?)?;
    let mut out = Vec::new();
    for row in rows.chunks_exact(width) {
        let (state, network_port, pid) = if ipv6 {
            (row[12], row[5], row[13])
        } else {
            (row[0], row[2], row[5])
        };
        if state != 2 || u16::from_be(network_port as u16) != port {
            continue;
        }
        let addr = if ipv6 {
            let mut bytes = [0; 16];
            for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(&row[..4]) {
                chunk.copy_from_slice(&word.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        } else {
            IpAddr::V4(Ipv4Addr::from(row[1].to_ne_bytes()))
        };
        out.push(Listener {
            addr,
            port,
            inode: 0,
            pids: vec![pid],
        });
    }
    Some(out)
}

/// Decide ownership from listeners: verified only if every listener is loopback and
/// `pid` holds at least one of them. No pid information yields [`OwnerCheck::Unknown`].
pub fn decide(listeners: &[Listener], pid: u32) -> OwnerCheck {
    if listeners.is_empty() {
        return OwnerCheck::NotListening;
    }
    if listeners.iter().any(|l| !l.is_loopback()) {
        return OwnerCheck::NotLoopback;
    }
    let owners: Vec<u32> = listeners.iter().flat_map(|l| l.pids.clone()).collect();
    if owners.is_empty() {
        return OwnerCheck::Unknown;
    }
    if owners.contains(&pid) {
        OwnerCheck::Verified
    } else {
        OwnerCheck::Mismatch { owners }
    }
}

/// Linux can rule out the candidate even when the actual owner's fds are hidden:
/// a complete, readable candidate fd list must contain one of the socket inodes.
/// `None` means the fd list was unreadable or incomplete, so absence proves nothing.
pub fn decide_with_socket_inodes(
    listeners: &[Listener],
    pid: u32,
    candidate_inodes: Option<&[u64]>,
) -> OwnerCheck {
    match (decide(listeners, pid), candidate_inodes) {
        (OwnerCheck::Unknown, Some(inodes)) => {
            if listeners.iter().any(|l| inodes.contains(&l.inode)) {
                OwnerCheck::Verified
            } else {
                OwnerCheck::Mismatch { owners: Vec::new() }
            }
        }
        (check, _) => check,
    }
}

/// A listener check is about one app main process: (port, pid, start time).
pub type OwnerKey = (u16, u32, Option<Timestamp>);

/// A remembered listener check ([`Caches::owner_check`]).
#[derive(Clone, Debug)]
pub struct OwnerMemo {
    check: OwnerCheck,
    at: Instant,
    /// Checks in a row with this (negative) answer.
    attempts: u32,
}

/// A negative answer (nothing listens yet, a foreign listener, cannot tell) is
/// checked again at most this many times on the polled paths, ...
pub const OWNER_RECHECKS: u32 = 3;
/// ... each after this many seconds times the attempts so far. Afterwards it
/// stands until an explicit check ([`Caches::record_owner`]) or
/// [`Caches::forget_owners`].
pub const OWNER_RECHECK_STEP: Duration = Duration::from_secs(2);

impl Caches {
    /// The listener check for an app main process, as the polled paths (the
    /// overview's session, the bridge probe) need it. A process that owns its CDP
    /// listener keeps it for its lifetime, so a verified answer is reused for as
    /// long as that process (pid and start time) lives; a negative one is checked
    /// again a few times with growing pauses (an app that is still starting opens
    /// its listener a moment later), then kept. On macOS a check runs `lsof`, so
    /// this keeps the polled paths from starting a process every few seconds.
    pub fn owner_check(
        &self,
        port: u16,
        pid: u32,
        started_at: Option<Timestamp>,
        check: impl FnOnce() -> OwnerCheck,
    ) -> OwnerCheck {
        let key = (port, pid, started_at);
        if let Some(memo) = lock(&self.owners).get(&key)
            && (memo.check == OwnerCheck::Verified
                || memo.attempts >= OWNER_RECHECKS
                || memo.at.elapsed() < OWNER_RECHECK_STEP * memo.attempts)
        {
            return memo.check.clone();
        }
        let fresh = check();
        self.remember_owner(key, fresh.clone());
        fresh
    }

    /// Store an answer an explicit (uncached) check got, so the polled paths see
    /// it (launch and restart wait for the listener themselves).
    pub fn record_owner(
        &self,
        port: u16,
        pid: u32,
        started_at: Option<Timestamp>,
        check: OwnerCheck,
    ) {
        let key = (port, pid, started_at);
        lock(&self.owners).remove(&key);
        self.remember_owner(key, check);
    }

    /// Forget every remembered listener check.
    pub fn forget_owners(&self) {
        lock(&self.owners).clear();
    }

    fn remember_owner(&self, key: OwnerKey, check: OwnerCheck) {
        let mut owners = lock(&self.owners);
        if owners.len() >= 64 {
            owners.clear();
        }
        let attempts = match owners.get(&key) {
            Some(memo) if memo.check == check && check != OwnerCheck::Verified => memo.attempts + 1,
            _ => 1,
        };
        owners.insert(
            key,
            OwnerMemo {
                check,
                at: Instant::now(),
                attempts,
            },
        );
    }
}

/// Listeners on `port` with owning pids, for this OS. `None` if the platform
/// mechanism is unavailable.
pub fn listeners(port: u16, candidate_pids: &[u32]) -> Option<Vec<Listener>> {
    imp::listeners(port, candidate_pids)
}

/// Check that `pid` owns a loopback-only listener on `port`.
pub fn check_owner(port: u16, pid: u32) -> OwnerCheck {
    match listeners(port, &[pid]) {
        Some(ls) => {
            #[cfg(target_os = "linux")]
            {
                decide_with_socket_inodes(&ls, pid, imp::socket_inodes_of(pid).as_deref())
            }
            #[cfg(not(target_os = "linux"))]
            {
                decide(&ls, pid)
            }
        }
        None => OwnerCheck::Unknown,
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Listener, parse_proc_net_tcp};

    pub(super) fn socket_inodes_of(pid: u32) -> Option<Vec<u64>> {
        let rd = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
        let mut inodes = Vec::new();
        for entry in rd {
            let entry = entry.ok()?;
            let target = match std::fs::read_link(entry.path()) {
                Ok(target) => target,
                // A descriptor closed during enumeration cannot still own the socket.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            if let Some(inode) = target
                .to_string_lossy()
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse().ok())
            {
                inodes.push(inode);
            }
        }
        Some(inodes)
    }

    pub(super) fn listeners(port: u16, candidate_pids: &[u32]) -> Option<Vec<Listener>> {
        let mut ls = Vec::new();
        let mut any_table = false;
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            if let Ok(text) = std::fs::read_to_string(table) {
                any_table = true;
                ls.extend(parse_proc_net_tcp(&text, port));
            }
        }
        if !any_table {
            return None;
        }
        if ls.is_empty() {
            return Some(ls);
        }
        // Candidates first (cheap); fall back to every readable process.
        let mut pids: Vec<u32> = candidate_pids.to_vec();
        let mut scanned_all = false;
        loop {
            for pid in &pids {
                let Some(inodes) = socket_inodes_of(*pid) else {
                    continue;
                };
                for l in ls.iter_mut() {
                    if inodes.contains(&l.inode) && !l.pids.contains(pid) {
                        l.pids.push(*pid);
                    }
                }
            }
            if ls.iter().all(|l| !l.pids.is_empty()) || scanned_all {
                break;
            }
            scanned_all = true;
            pids = std::fs::read_dir("/proc")
                .ok()?
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
                .filter(|p| !candidate_pids.contains(p))
                .collect();
        }
        Some(ls)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    //! macOS: `lsof` ships with the OS; `-Fpn` gives machine-readable pid/name lines.
    use super::{Listener, parse_lsof};

    pub(super) fn listeners(port: u16, _candidate_pids: &[u32]) -> Option<Vec<Listener>> {
        let out = superba_core::process::command("/usr/sbin/lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"])
            .output()
            .ok()?;
        // lsof exits 1 when nothing matches.
        Some(parse_lsof(&String::from_utf8_lossy(&out.stdout), port))
    }
}

#[cfg(windows)]
mod imp {
    //! Windows: native listener tables avoid localized text and executable lookup.
    //! Only the API call is platform-specific; decoding is exercised on Linux.
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    use super::{Listener, parse_windows_tcp_table};

    // Both OWNER_PID layouts contain only DWORDs / byte arrays, with no padding
    // between rows or between the count and the first row.
    const _: () = {
        assert!(std::mem::size_of::<MIB_TCPROW_OWNER_PID>() == 6 * 4);
        assert!(std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>() == 14 * 4);
        assert!(std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table) == 4);
        assert!(std::mem::offset_of!(MIB_TCP6TABLE_OWNER_PID, table) == 4);
    };

    fn tcp_table(family: u16) -> Option<Vec<u32>> {
        // DWORD storage supplies the alignment required by both SDK table types.
        let mut words = Vec::<u32>::new();
        for _ in 0..4 {
            let mut bytes = u32::try_from(words.len().checked_mul(4)?).ok()?;
            let ptr = if words.is_empty() {
                std::ptr::null_mut()
            } else {
                words.as_mut_ptr().cast()
            };
            // SAFETY: ptr is null for the size query or points to `bytes` writable
            // bytes with DWORD alignment. The call is synchronous and retains no pointer.
            let status = unsafe {
                GetExtendedTcpTable(
                    ptr,
                    &mut bytes,
                    0,
                    u32::from(family),
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            if status == ERROR_INSUFFICIENT_BUFFER {
                words.resize((bytes as usize).div_ceil(4), 0);
                continue; // The table can grow between the size query and the read.
            }
            if status != NO_ERROR || bytes as usize > words.len() * 4 || !bytes.is_multiple_of(4) {
                return None;
            }
            words.truncate(bytes as usize / 4);
            return Some(words);
        }
        None
    }

    pub(super) fn listeners(port: u16, _candidate_pids: &[u32]) -> Option<Vec<Listener>> {
        let mut all = Vec::new();
        for (family, ipv6) in [(AF_INET, false), (AF_INET6, true)] {
            all.extend(parse_windows_tcp_table(&tcp_table(family)?, ipv6, port)?);
        }
        Some(all)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::Listener;

    pub(super) fn listeners(_port: u16, _candidate_pids: &[u32]) -> Option<Vec<Listener>> {
        None
    }
}
