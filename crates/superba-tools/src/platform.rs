//! The computer a tool is installed on: OS, processor, C library and the
//! processor features some vendors build separate binaries for.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    /// macOS.
    Macos,
    /// Linux (also WSL).
    Linux,
    /// Windows.
    Windows,
}

impl Os {
    /// The OS this build runs on.
    pub fn current() -> Self {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }
}

/// Processor architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    /// x86-64.
    X64,
    /// ARM64.
    Arm64,
}

/// What a binary must match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    /// OS.
    pub os: Os,
    /// Processor (the machine's, also when this process is translated).
    pub arch: Arch,
    /// Linux with musl libc (Alpine).
    pub musl: bool,
    /// x86-64 without AVX2 (opencode's `-baseline` builds).
    pub baseline: bool,
}

impl Platform {
    /// This computer.
    pub fn detect() -> Self {
        let os = Os::current();
        let arch = if cfg!(target_arch = "aarch64") || (os == Os::Macos && rosetta()) {
            Arch::Arm64
        } else {
            Arch::X64
        };
        Self {
            os,
            arch,
            musl: os == Os::Linux && linux_is_musl(Path::new("/")),
            baseline: arch == Arch::X64 && !avx2(),
        }
    }

    /// A fixed platform (tests and descriptors).
    pub fn new(os: Os, arch: Arch) -> Self {
        Self {
            os,
            arch,
            musl: false,
            baseline: false,
        }
    }

    /// `x64` / `arm64`.
    pub fn arch_name(&self) -> &'static str {
        match self.arch {
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
        }
    }

    /// `.exe` on Windows.
    pub fn exe_suffix(&self) -> &'static str {
        if self.os == Os::Windows { ".exe" } else { "" }
    }

    /// `name` with the executable suffix of this OS.
    pub fn exe(&self, name: &str) -> String {
        format!("{name}{}", self.exe_suffix())
    }
}

/// An x86-64 process translated by Rosetta 2 on an Apple silicon Mac.
fn rosetta() -> bool {
    #[cfg(target_os = "macos")]
    {
        superba_core::process::command("/usr/sbin/sysctl")
            .args(["-n", "sysctl.proc_translated"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The processor has AVX2 (always true off x86-64).
fn avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        true
    }
}

/// A Linux root (`/`) whose C library is musl: the musl loader
/// `/lib/ld-musl-*.so.1` exists and glibc's does not.
pub fn linux_is_musl(root: &Path) -> bool {
    let lib = root.join("lib");
    let Ok(entries) = std::fs::read_dir(&lib) else {
        return false;
    };
    let musl = entries.flatten().any(|e| {
        let n = e.file_name();
        let n = n.to_string_lossy();
        n.starts_with("ld-musl-") || n.starts_with("libc.musl-")
    });
    let glibc = ["lib64/ld-linux-x86-64.so.2", "lib/ld-linux-aarch64.so.1"]
        .iter()
        .any(|p| root.join(p).exists())
        || std::fs::read_dir(&lib).is_ok_and(|mut d| {
            d.any(|e| {
                e.is_ok_and(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n.starts_with("ld-linux") || n == "x86_64-linux-gnu" || n == "aarch64-linux-gnu"
                })
            })
        });
    musl && !glibc
}
