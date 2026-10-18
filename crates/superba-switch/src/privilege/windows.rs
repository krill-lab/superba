//! The Windows facts of [`super::WindowsFacts`]: this process's token (user,
//! elevation), its session and the sessions' users. Read once per process: the
//! token's user never changes, and a signed-in user changing under a running
//! Superba is not worth a lookup before every write (a domain account lookup
//! may even ask a domain controller). Every call fails soft: an unreadable
//! value is `None`, and [`super::decide_windows`] decides on what is known.

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    GetTokenInformation, LookupAccountNameW, LookupAccountSidW, PSID, SID_NAME_USE,
    TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
};
use windows_sys::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTS_CURRENT_SERVER_HANDLE, WTSDomainName, WTSFreeMemory,
    WTSGetActiveConsoleSessionId, WTSQuerySessionInformationW, WTSUserName,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcessToken,
};
use windows_sys::core::PWSTR;

use super::{Account, WindowsFacts};

/// "No console session" (`WTSGetActiveConsoleSessionId` while sessions switch).
const NO_SESSION: u32 = 0xFFFF_FFFF;

/// The facts of this process (gathered on first use).
pub(super) fn facts() -> &'static WindowsFacts {
    static FACTS: OnceLock<WindowsFacts> = OnceLock::new();
    FACTS.get_or_init(|| {
        let facts = gather();
        tracing::info!(?facts, "account facts");
        facts
    })
}

fn gather() -> WindowsFacts {
    let token = ProcessToken::open();
    let process = token.as_ref().and_then(ProcessToken::user);
    let elevated = token.as_ref().is_some_and(ProcessToken::elevated);
    let mut session = 0u32;
    // SAFETY: `session` is a valid output location; the call has no other inputs.
    let session_id = (unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } != 0)
        .then_some(session);
    let own_user = session_id.filter(|&s| s != 0).and_then(session_user);
    // SAFETY: no arguments, no preconditions.
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    let console_user = (console != NO_SESSION)
        .then_some(console)
        .and_then(session_user);
    WindowsFacts {
        process,
        elevated,
        session_id,
        session_user: own_user,
        console_user,
    }
}

/// This process's token (closed on drop).
struct ProcessToken(HANDLE);

impl ProcessToken {
    fn open() -> Option<Self> {
        let mut token = std::ptr::null_mut();
        // SAFETY: the pseudo handle of the current process needs no closing;
        // `token` receives a handle we own and close in `Drop`.
        let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if ok == 0 {
            tracing::warn!(error = %std::io::Error::last_os_error(), "cannot open the process token");
            return None;
        }
        Some(Self(token))
    }

    fn elevated(&self) -> bool {
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0;
        // SAFETY: the buffer is a TOKEN_ELEVATION of the size passed; the token is open.
        let ok = unsafe {
            GetTokenInformation(
                self.0,
                TokenElevation,
                (&raw mut elevation).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
        };
        ok != 0 && elevation.TokenIsElevated != 0
    }

    fn user(&self) -> Option<Account> {
        let mut needed = 0u32;
        // SAFETY: a size query with no buffer; it fails with the size needed.
        unsafe { GetTokenInformation(self.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return None;
        }
        // u64 elements: the buffer is aligned for TOKEN_USER (pointer alignment).
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buf` holds at least `needed` bytes and is suitably aligned.
        let ok = unsafe {
            GetTokenInformation(
                self.0,
                TokenUser,
                buf.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points
        // into the same buffer, which lives until the end of this function.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let text = sid_string(sid)?;
        let name = sid_name(sid).unwrap_or_else(|| text.clone());
        Some(Account {
            sid: Some(text),
            name,
        })
    }
}

impl Drop for ProcessToken {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcessToken and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// `S-1-5-…` of a SID.
fn sid_string(sid: PSID) -> Option<String> {
    let mut out: PWSTR = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID; `out` receives a LocalAlloc'ed string we free.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 || out.is_null() {
        return None;
    }
    // SAFETY: `out` is a NUL-terminated UTF-16 string.
    let text = unsafe { wide_to_string(out) };
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
    unsafe { LocalFree(out.cast()) };
    Some(text)
}

/// `DOMAIN\user` of a SID.
fn sid_name(sid: PSID) -> Option<String> {
    let (mut name, mut domain) = ([0u16; 256], [0u16; 256]);
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut kind: SID_NAME_USE = 0;
    // SAFETY: both buffers have the lengths passed; `sid` is a valid SID.
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    (ok != 0).then(|| {
        join_name(
            &String::from_utf16_lossy(&domain[..domain_len as usize]),
            &String::from_utf16_lossy(&name[..name_len as usize]),
        )
    })
}

/// The SID string of the account `DOMAIN\user`.
fn name_sid(name: &str) -> Option<String> {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut sid = vec![0u64; 16]; // 128 bytes: SECURITY_MAX_SID_SIZE is 68
    let mut sid_len = (sid.len() * 8) as u32;
    let mut domain = [0u16; 256];
    let mut domain_len = domain.len() as u32;
    let mut kind: SID_NAME_USE = 0;
    // SAFETY: every buffer has the length passed; `wide` is NUL-terminated.
    let ok = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            wide.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    (ok != 0)
        .then(|| sid_string(sid.as_mut_ptr().cast()))
        .flatten()
}

/// The user signed in to `session`, if any.
fn session_user(session: u32) -> Option<Account> {
    let user = session_string(session, WTSUserName)?;
    if user.is_empty() {
        return None;
    }
    let domain = session_string(session, WTSDomainName).unwrap_or_default();
    let name = join_name(&domain, &user);
    Some(Account {
        sid: name_sid(&name),
        name,
    })
}

fn session_string(session: u32, class: i32) -> Option<String> {
    let mut buf: PWSTR = std::ptr::null_mut();
    let mut bytes = 0u32;
    // SAFETY: `buf` receives a WTS-allocated string we free with WTSFreeMemory.
    let ok = unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session,
            class,
            &mut buf,
            &mut bytes,
        )
    };
    if ok == 0 || buf.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated UTF-16 string, freed right after.
    let text = unsafe { wide_to_string(buf) };
    // SAFETY: allocated by WTSQuerySessionInformationW.
    unsafe { WTSFreeMemory(buf.cast()) };
    Some(text)
}

fn join_name(domain: &str, user: &str) -> String {
    if domain.is_empty() {
        user.to_owned()
    } else {
        format!("{domain}\\{user}")
    }
}

/// # Safety
/// `p` must point to a NUL-terminated UTF-16 string.
unsafe fn wide_to_string(p: *const u16) -> String {
    let mut len = 0;
    // SAFETY: the caller guarantees a terminating NUL.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` elements before the NUL are readable.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}
