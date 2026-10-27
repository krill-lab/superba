//! How the process uses the allocator (user bug 125).
//!
//! **glibc (Linux)** gives every thread that allocates while the others hold
//! the lock an arena of its own, up to eight per CPU core, and an arena only
//! shrinks from its top. Superba's background threads (the backend runtime,
//! its blocking pool, the window's worker threads) each read a process list,
//! a request log or a rollout now and then and free it again; every one of
//! them kept 25-90 MB of freed memory in its own arena (about 480 MB freed
//! but held after every page was opened on a 64-core machine, 700 MB in all
//! after ten idle minutes). [`ARENAS`] arenas share that memory instead
//! (measured: 600 MB after every page down to 320 MB, flat while idle).
//!
//! Windows and macOS have one process heap and need nothing here.

/// The most malloc arenas glibc may create for this process.
pub const ARENAS: i32 = 2;

/// Limit glibc's malloc to [`ARENAS`] arenas, unless the user set
/// `MALLOC_ARENA_MAX` (or a glibc tunable) themselves. Call first in `main`,
/// before any thread starts. Does nothing on other platforms.
pub fn limit_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let user_set = std::env::var_os("MALLOC_ARENA_MAX").is_some()
            || std::env::var("GLIBC_TUNABLES").is_ok_and(|t| t.contains("arena_max"));
        if !user_set {
            // SAFETY: mallopt only changes allocator parameters; it takes the
            // allocator's own locks and is called before other threads exist.
            unsafe {
                libc::mallopt(libc::M_ARENA_MAX, ARENAS);
            }
        }
    }
}
