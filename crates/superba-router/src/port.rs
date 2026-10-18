//! The router's loopback port (user bug 65).
//!
//! Codex's `config.toml` names the port (`openai_base_url`), so it has to stay
//! usable across restarts of the router, of the computer and of Superba. A port
//! the OS hands out for `bind(:0)` comes from its ephemeral range, which every
//! program's outgoing connections draw from as well: after a restart another
//! program could hold it, and the router stayed down until the service
//! manager's retries happened to win.
//!
//! A new configuration therefore gets a stable port outside every OS ephemeral
//! range: [`STABLE_PORTS`] (17000–19999) is below the IANA dynamic range that
//! Windows and macOS use (49152–65535) and below Linux's default
//! (32768–60999; the configured `/proc/sys/net/ipv4/ip_local_port_range` is
//! read). The first candidate is derived from the data directory (per user and
//! per Superba data directory, so two users or two data directories rarely
//! meet), the others follow in a fixed order across the whole window; a
//! candidate is taken when the router could bind it now. The choice is stored
//! in `config.json` (`listen_port`) and kept: it moves only when another
//! program holds it while the router is down
//! ([`crate::manager::RouterManager::move_off_taken_port`]).
//!
//! Windows' dynamic range is not read (that would need `netsh`); its documented
//! default is used. Ports Windows keeps for Hyper-V / WSL (`netsh int ipv4 show
//! excludedportrange`) refuse a bind, so a candidate among them is skipped.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::ops::RangeInclusive;

use sha2::{Digest, Sha256};

/// Where the router's port is chosen: below every OS ephemeral range by
/// default.
pub const STABLE_PORTS: RangeInclusive<u16> = 17_000..=19_999;

/// Step between candidates (coprime to the size of [`STABLE_PORTS`], so the
/// candidates visit the whole window, spread out rather than side by side).
const STRIDE: u32 = 1_009;

/// The IANA dynamic range: Windows (since Vista) and macOS by default.
pub const IANA_DYNAMIC: RangeInclusive<u16> = 49_152..=65_535;

/// Linux's default `ip_local_port_range`.
pub const LINUX_DEFAULT_EPHEMERAL: RangeInclusive<u16> = 32_768..=60_999;

/// Every port of [`STABLE_PORTS`], in the order they are tried for `seed`:
/// the per-seed first choice, then the others with a fixed stride.
pub fn candidates(seed: &str) -> Vec<u16> {
    let start = u32::from(*STABLE_PORTS.start());
    let size = u32::from(*STABLE_PORTS.end()) - start + 1;
    let first = seed_hash(seed) % size;
    (0..size)
        .map(|k| {
            let offset = (first + k * STRIDE) % size;
            // In range: `start + offset <= STABLE_PORTS.end()`.
            u16::try_from(start + offset).unwrap_or(*STABLE_PORTS.start())
        })
        .collect()
}

/// The port a new configuration gets for `seed` when nothing is in the way.
pub fn preferred(seed: &str) -> u16 {
    candidates(seed)[0]
}

fn seed_hash(seed: &str) -> u32 {
    let digest = Sha256::new()
        .chain_update(b"superba-router-port-v1\0")
        .chain_update(seed.as_bytes())
        .finalize();
    u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]])
}

/// The first of `seed`'s [`candidates`] that is not in `avoid` and that
/// `is_free` accepts, preferring ports outside every range of `ephemeral`
/// (those inside are tried only when every port outside is taken, e.g. on a
/// Linux whose ephemeral range was widened over the whole window). `None` when
/// every port is taken.
pub fn choose(
    seed: &str,
    ephemeral: &[RangeInclusive<u16>],
    avoid: &[u16],
    mut is_free: impl FnMut(u16) -> bool,
) -> Option<u16> {
    let (outside, inside): (Vec<u16>, Vec<u16>) = candidates(seed)
        .into_iter()
        .filter(|p| !avoid.contains(p))
        .partition(|p| !ephemeral.iter().any(|r| r.contains(p)));
    outside.into_iter().chain(inside).find(|p| is_free(*p))
}

/// The ranges this OS hands out ephemeral ports from (outgoing connections,
/// `bind(:0)`).
pub fn ephemeral_ranges() -> Vec<RangeInclusive<u16>> {
    vec![os_ephemeral_range()]
}

#[cfg(target_os = "linux")]
fn os_ephemeral_range() -> RangeInclusive<u16> {
    std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|text| parse_port_range(&text))
        .unwrap_or(LINUX_DEFAULT_EPHEMERAL)
}

#[cfg(target_os = "macos")]
fn os_ephemeral_range() -> RangeInclusive<u16> {
    match (
        sysctl_port(c"net.inet.ip.portrange.first"),
        sysctl_port(c"net.inet.ip.portrange.last"),
    ) {
        (Some(first), Some(last)) if first <= last => first..=last,
        _ => IANA_DYNAMIC,
    }
}

/// An integer `sysctl` value that is a port number.
#[cfg(target_os = "macos")]
fn sysctl_port(name: &std::ffi::CStr) -> Option<u16> {
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: `name` is NUL-terminated; `value` and `len` are valid for writes
    // and `len` holds the size of `value`; no new value is set (null, 0).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<libc::c_int>() {
        return None;
    }
    u16::try_from(value).ok()
}

/// Windows' documented default dynamic range (49152–65535 since Vista; reading
/// the configured one needs `netsh int ipv4 show dynamicport tcp`).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn os_ephemeral_range() -> RangeInclusive<u16> {
    IANA_DYNAMIC
}

/// Parse `ip_local_port_range` (`"32768\t60999\n"`).
pub fn parse_port_range(text: &str) -> Option<RangeInclusive<u16>> {
    let mut parts = text.split_whitespace().map(str::parse::<u16>);
    let (Some(Ok(first)), Some(Ok(last)), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    (first <= last).then_some(first..=last)
}

/// Whether the router could listen on `port` now: on `127.0.0.1` and on each of
/// the WSL adapter addresses `wsl` (one this computer does not have at the
/// moment does not count: the router binds it once it exists).
pub fn bindable(port: u16, wsl: &[Ipv4Addr]) -> bool {
    let free = |ip: Ipv4Addr| match TcpListener::bind(SocketAddr::from((ip, port))) {
        Ok(_) => true,
        Err(e) => ip != Ipv4Addr::LOCALHOST && e.kind() == ErrorKind::AddrNotAvailable,
    };
    free(Ipv4Addr::LOCALHOST) && wsl.iter().all(|ip| free(*ip))
}

/// A stable port for the router of `seed` (see the module docs), not one of
/// `avoid`, that it could bind now on `127.0.0.1` and the WSL addresses `wsl`.
pub fn pick(seed: &str, avoid: &[u16], wsl: &[Ipv4Addr]) -> Option<u16> {
    choose(seed, &ephemeral_ranges(), avoid, |p| bindable(p, wsl))
}
