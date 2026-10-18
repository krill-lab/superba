//! `PATH`: whether a program's folder is on it, the line a Unix shell needs
//! (shown, never written: Superba does not edit shell start-up files), and on
//! Windows the user `PATH` in `HKCU\Environment`, which Superba extends
//! itself (per user, no administrator rights) and takes back on removal.

use std::path::{Path, PathBuf};

use superba_core::{EnvSource, PathHint, ShellKind};

/// The directories of `PATH` in `env`.
pub fn path_dirs(env: &dyn EnvSource) -> Vec<PathBuf> {
    env.var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

fn same_dir(a: &Path, b: &Path, windows: bool) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_owned();
        if windows { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

/// `dir` is one of `dirs`.
pub fn contains(dirs: &[PathBuf], dir: &Path, windows: bool) -> bool {
    dirs.iter().any(|d| same_dir(d, dir, windows))
}

/// The start-up file and line a Unix shell needs to find programs in `dir`;
/// `None` when `dir` is already on `PATH`.
pub fn unix_hint(env: &dyn EnvSource, home: &Path, dir: &Path, macos: bool) -> Option<PathHint> {
    if contains(&path_dirs(env), dir, false) {
        return None;
    }
    let shown = match dir.strip_prefix(home) {
        Ok(rel) => format!("$HOME/{}", rel.display()),
        Err(_) => dir.display().to_string(),
    };
    let shell = ShellKind::from_shell_path(&env.var("SHELL").unwrap_or_default());
    let (file, line) = match shell {
        ShellKind::Zsh => ("~/.zshrc", format!("export PATH=\"{shown}:$PATH\"")),
        ShellKind::Bash if macos => ("~/.bash_profile", format!("export PATH=\"{shown}:$PATH\"")),
        ShellKind::Bash => ("~/.bashrc", format!("export PATH=\"{shown}:$PATH\"")),
        ShellKind::Fish => (
            "~/.config/fish/config.fish",
            format!("fish_add_path \"{shown}\""),
        ),
        _ => ("~/.profile", format!("export PATH=\"{shown}:$PATH\"")),
    };
    Some(PathHint {
        dir: dir.to_path_buf(),
        line,
        file: file.into(),
    })
}

/// A `;`-separated Windows path list with `entry` appended (`None` when it is
/// already there, compared case-insensitively and with `%USERPROFILE%`
/// expanded by `expand`).
pub fn list_with(list: &str, entry: &str, expand: &dyn Fn(&str) -> String) -> Option<String> {
    let want = expand(entry);
    let present = list
        .split(';')
        .filter(|p| !p.trim().is_empty())
        .any(|p| same_dir(Path::new(&expand(p.trim())), Path::new(&want), true));
    if present {
        return None;
    }
    let trimmed = list.trim_end_matches(';');
    Some(if trimmed.is_empty() {
        entry.to_owned()
    } else {
        format!("{trimmed};{entry}")
    })
}

/// A `;`-separated Windows path list without `entry` (`None` when it is not
/// there).
pub fn list_without(list: &str, entry: &str, expand: &dyn Fn(&str) -> String) -> Option<String> {
    let want = expand(entry);
    let parts: Vec<&str> = list.split(';').collect();
    let kept: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|p| !same_dir(Path::new(&expand(p.trim())), Path::new(&want), true))
        .collect();
    (kept.len() != parts.len()).then(|| kept.join(";"))
}

/// Windows: the user `PATH` (`HKCU\Environment\Path`).
#[cfg(windows)]
pub mod user {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, RegType};
    use winreg::{RegKey, RegValue};

    use superba_core::{Error, Result};

    fn io(e: std::io::Error) -> Error {
        Error::io(r"HKCU\Environment", &e)
    }

    fn expand(s: &str) -> String {
        let mut out = s.to_owned();
        for var in [
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "HOMEDRIVE",
            "HOMEPATH",
        ] {
            if let Ok(v) = std::env::var(var) {
                let pat = format!("%{var}%");
                while let Some(i) = out.to_ascii_uppercase().find(&pat) {
                    out.replace_range(i..i + pat.len(), &v);
                }
            }
        }
        out
    }

    fn read() -> Result<(String, RegType)> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", KEY_READ)
            .map_err(io)?;
        match key.get_raw_value("Path") {
            Ok(v) => {
                let text: Vec<u16> = v
                    .bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|c| *c != 0)
                    .collect();
                Ok((String::from_utf16_lossy(&text), v.vtype))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok((String::new(), RegType::REG_EXPAND_SZ))
            }
            Err(e) => Err(io(e)),
        }
    }

    fn write(value: &str, vtype: RegType) -> Result<()> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", KEY_READ | KEY_SET_VALUE)
            .map_err(io)?;
        let mut bytes: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
        bytes.extend_from_slice(&[0, 0]);
        // Keep REG_EXPAND_SZ: the user PATH usually holds %USERPROFILE% entries.
        let vtype = if matches!(vtype, RegType::REG_SZ) && !value.contains('%') {
            RegType::REG_SZ
        } else {
            RegType::REG_EXPAND_SZ
        };
        key.set_raw_value("Path", &RegValue { bytes, vtype })
            .map_err(io)?;
        broadcast();
        Ok(())
    }

    /// The user `PATH` has `dir`.
    pub fn has(dir: &str) -> bool {
        read().is_ok_and(|(list, _)| super::list_with(&list, dir, &expand).is_none())
    }

    /// Add `dir` at the end of the user `PATH`; `false` when it was there.
    pub fn add(dir: &str) -> Result<bool> {
        let (list, vtype) = read()?;
        match super::list_with(&list, dir, &expand) {
            Some(new) => write(&new, vtype).map(|()| true),
            None => Ok(false),
        }
    }

    /// Remove `dir` from the user `PATH`; `false` when it was not there.
    pub fn remove(dir: &str) -> Result<bool> {
        let (list, vtype) = read()?;
        match super::list_without(&list, dir, &expand) {
            Some(new) => write(&new, vtype).map(|()| true),
            None => Ok(false),
        }
    }

    /// Tell Explorer (and so new terminals) that the environment changed.
    /// Best effort, 5 s per window.
    #[allow(unsafe_code)]
    fn broadcast() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        };
        let name: Vec<u16> = "Environment\0".encode_utf16().collect();
        let mut result: usize = 0;
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the
        // call; `result` is a valid out pointer; HWND_BROADCAST is a
        // documented sentinel.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                name.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
        }
    }
}
