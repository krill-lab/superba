//! Who Superba runs as, and whether it may change files (plan 12.4, user bug 44).
//!
//! - **Unix**: root through `sudo` is refused (files in the user's home would
//!   end up owned by root). A real root login is allowed, but never gets a
//!   debugging port ([`crate::planner::launch_cdp_mode`]).
//! - **Windows**: an elevated token of the signed-in user is allowed: it is
//!   the same account and the same profile, so Superba's files, the router's
//!   per-user Run entry and the per-user environment variables land where they
//!   belong (with UAC turned off every process is elevated). Refused is another
//!   account than the user signed in at this computer: SYSTEM and the other
//!   service accounts, another user through `runas` or a scheduled task. Its
//!   files would go to that account's profile.
//!
//! The decisions are pure functions of the facts ([`decide_unix`],
//! [`decide_windows`]); [`current`] gathers the facts of this process.
//!
//! "The signed-in user" on Windows is the user of this process's session
//! (`ProcessIdToSessionId` + `WTSQuerySessionInformationW`, so a Remote
//! Desktop session counts as well); a process in session 0 (services, a task
//! that runs "whether the user is logged on or not", an SSH logon) has no
//! desktop of its own and is compared with the user of the active console
//! session (`WTSGetActiveConsoleSessionId`). Accounts are compared by SID
//! (the session's `DOMAIN\user` is resolved with `LookupAccountNameW`), by
//! name when a SID cannot be resolved.

use superba_core::{BlockReason, EnvSource};

#[cfg(windows)]
mod windows;

/// A Windows account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// Its SID (`S-1-5-21-…`), when known.
    pub sid: Option<String>,
    /// `DOMAIN\user` (`NT AUTHORITY\SYSTEM`, `PC\alice`, `AzureAD\Alice`).
    pub name: String,
}

impl Account {
    /// An account with a SID.
    pub fn new(sid: &str, name: &str) -> Self {
        Self {
            sid: Some(sid.to_owned()),
            name: name.to_owned(),
        }
    }
}

/// What [`decide_windows`] looks at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowsFacts {
    /// The user of this process's token; `None` when the token cannot be read.
    pub process: Option<Account>,
    /// The token is elevated (UAC off, "Run as administrator").
    pub elevated: bool,
    /// This process's session (`0` = the services' session, no desktop).
    pub session_id: Option<u32>,
    /// The user signed in to that session, if any.
    pub session_user: Option<Account>,
    /// The user signed in at the active console session, if any.
    pub console_user: Option<Account>,
}

/// Who Superba runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunAs {
    /// The signed-in user (Windows: unelevated; Unix: anyone but root via sudo).
    User,
    /// Windows: the signed-in user with an elevated token. Allowed: same
    /// account, same profile.
    ElevatedUser,
    /// Unix: root through `sudo`. Refused.
    Sudo,
    /// Windows: another account than the signed-in user. Refused.
    OtherUser {
        /// The account Superba runs as.
        account: String,
        /// The signed-in user, when there is one.
        desktop_user: Option<String>,
    },
}

impl RunAs {
    /// Why writes are refused, or `None` when they are allowed.
    pub fn block_reason(&self) -> Option<BlockReason> {
        match self {
            RunAs::User | RunAs::ElevatedUser => None,
            RunAs::Sudo => Some(BlockReason::Elevated),
            RunAs::OtherUser {
                account,
                desktop_user,
            } => Some(BlockReason::OtherUser {
                account: account.clone(),
                desktop_user: desktop_user.clone(),
            }),
        }
    }

    /// Writes are refused.
    pub fn refused(&self) -> bool {
        self.block_reason().is_some()
    }
}

/// Unix: `sudo_user` = `SUDO_USER` is set, `root` = the effective user is root.
pub fn decide_unix(sudo_user: bool, root: bool) -> RunAs {
    if sudo_user && root {
        RunAs::Sudo
    } else {
        RunAs::User
    }
}

/// Windows: the decision for `facts` (see the module docs).
pub fn decide_windows(facts: &WindowsFacts) -> RunAs {
    let Some(process) = &facts.process else {
        // Our own token could not be read: refuse rather than write somewhere unknown.
        return RunAs::OtherUser {
            account: "(unknown account)".into(),
            desktop_user: desktop_user(facts).map(|d| d.name.clone()),
        };
    };
    let desktop = desktop_user(facts);
    let service = process.sid.as_deref().is_some_and(is_service_account);
    let other = desktop.is_some_and(|d| !same_account(process, d));
    if service || other {
        return RunAs::OtherUser {
            account: process.name.clone(),
            desktop_user: desktop.map(|d| d.name.clone()),
        };
    }
    if facts.elevated {
        RunAs::ElevatedUser
    } else {
        RunAs::User
    }
}

/// The user at the desktop this process belongs to: its own session's user
/// (an interactive session), else the active console session's user.
pub fn desktop_user(facts: &WindowsFacts) -> Option<&Account> {
    let own = match facts.session_id {
        Some(id) if id != 0 => facts.session_user.as_ref(),
        _ => None,
    };
    own.or(facts.console_user.as_ref())
}

/// SYSTEM, LOCAL SERVICE, NETWORK SERVICE and the virtual service accounts
/// (`NT SERVICE\…`, IIS app pools, Hyper-V VMs, window manager and font driver
/// hosts): never a person at a desktop.
pub fn is_service_account(sid: &str) -> bool {
    const WELL_KNOWN: [&str; 3] = ["S-1-5-18", "S-1-5-19", "S-1-5-20"];
    const VIRTUAL: [&str; 5] = [
        "S-1-5-80-",
        "S-1-5-82-",
        "S-1-5-83-",
        "S-1-5-90-",
        "S-1-5-96-",
    ];
    let sid = sid.trim();
    WELL_KNOWN.iter().any(|w| sid.eq_ignore_ascii_case(w))
        || VIRTUAL
            .iter()
            .any(|p| sid.len() > p.len() && sid[..p.len()].eq_ignore_ascii_case(p))
}

/// The same account: equal SIDs when both are known, else equal names
/// (ignoring case; a name without a domain matches on the user part).
pub fn same_account(a: &Account, b: &Account) -> bool {
    if let (Some(x), Some(y)) = (&a.sid, &b.sid) {
        return x.trim().eq_ignore_ascii_case(y.trim());
    }
    let split = |n: &str| match n.trim().rsplit_once('\\') {
        Some((domain, user)) => (Some(domain.to_owned()), user.to_owned()),
        None => (None, n.trim().to_owned()),
    };
    let ((da, ua), (db, ub)) = (split(&a.name), split(&b.name));
    let domains = match (da, db) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(&y),
        _ => true,
    };
    !ua.is_empty() && domains && ua.eq_ignore_ascii_case(&ub)
}

/// Who this process runs as. Unix reads `SUDO_USER` from `env` (injected in
/// tests) and the effective user id; Windows reads the process token and the
/// sessions once per process.
pub fn current(env: &dyn EnvSource) -> RunAs {
    #[cfg(unix)]
    {
        decide_unix(
            env.var_non_empty("SUDO_USER").is_some(),
            crate::planner::effective_root(),
        )
    }
    #[cfg(windows)]
    {
        let _ = env;
        decide_windows(windows::facts())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = env;
        RunAs::User
    }
}

/// Windows: the facts [`current`] decided on (for logs and diagnostics);
/// `None` elsewhere.
pub fn windows_facts() -> Option<WindowsFacts> {
    #[cfg(windows)]
    {
        Some(windows::facts().clone())
    }
    #[cfg(not(windows))]
    {
        None
    }
}
