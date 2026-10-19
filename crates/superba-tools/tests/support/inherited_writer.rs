//! Hold a forked child before exec, where it still owns the parent's
//! close-on-exec file descriptors. The socket controls ordering, not time.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Stdio;

pub struct BeforeExec {
    pub pid: u32,
    release: UnixStream,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl BeforeExec {
    pub fn start() -> Self {
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut command = superba_core::process::command("/bin/true");
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: after fork the callback only transfers stack bytes over
            // the socket (read/write syscalls) and calls getpid. No locks or
            // allocations; the parent always releases it, including on panic.
            unsafe {
                command.pre_exec(move || {
                    child.write_all(&std::process::id().to_ne_bytes())?;
                    child.read_exact(&mut [0])
                });
            }
            assert!(command.spawn().unwrap().wait().unwrap().success());
        });
        let mut pid = [0; 4];
        parent.read_exact(&mut pid).unwrap();
        Self {
            pid: u32::from_ne_bytes(pid),
            release: parent,
            worker: Some(worker),
        }
    }
}

impl Drop for BeforeExec {
    fn drop(&mut self) {
        let _ = self.release.write_all(&[1]);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn has_writer(pid: u32, path: &Path) -> bool {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .flatten()
        .any(|entry| {
            if std::fs::read_link(entry.path()).ok().as_deref() != Some(path) {
                return false;
            }
            let info = std::fs::read_to_string(format!(
                "/proc/{pid}/fdinfo/{}",
                entry.file_name().to_string_lossy()
            ))
            .unwrap();
            info.lines().any(|line| {
                line.strip_prefix("flags:")
                    .and_then(|flags| u32::from_str_radix(flags.trim(), 8).ok())
                    .is_some_and(|flags| flags & 3 != 0)
            })
        })
}
