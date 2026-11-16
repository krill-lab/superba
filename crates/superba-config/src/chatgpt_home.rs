//! Disposable private Codex homes for the ChatGPT sign-in and limits helpers
//! (PR #20). Each helper `codex app-server` runs with `CODEX_HOME` set to one
//! of these directories (0700, under `secrets/chatgpt-sessions/`); a login
//! exists there in plain form only while its helper runs, and the directory
//! is removed when the [`SessionHome`] is dropped. Leftovers of a crashed run
//! are swept when the next one starts.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use superba_core::{DataDir, Error, Result, SecretBytes};

use crate::fsx;

/// Leftover homes older than this are removed when a new one is created.
const STALE_AFTER: Duration = Duration::from_secs(30 * 60);

/// One private helper home; removed on drop.
#[derive(Debug)]
pub struct SessionHome {
    path: PathBuf,
}

impl SessionHome {
    /// Create a fresh home in `data`'s sessions directory, with `auth` as its
    /// `auth.json` (0600) when given.
    pub fn create(data: &DataDir, auth: Option<&SecretBytes>) -> Result<Self> {
        let root = data.chatgpt_sessions_dir();
        fsx::ensure_private_dir(&root)?;
        sweep(&root);
        let path = root.join(uuid::Uuid::new_v4().simple().to_string());
        fsx::ensure_private_dir(&path)?;
        let home = Self { path };
        if let Some(auth) = auth {
            fsx::write_atomic(&home.auth_path(), auth.expose(), fsx::Mode::Private)?;
        }
        Ok(home)
    }

    /// The directory to pass as `CODEX_HOME`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn auth_path(&self) -> PathBuf {
        self.path.join("auth.json")
    }

    /// The login Codex left in the home (`None` when it saved none).
    pub fn auth(&self) -> Result<Option<SecretBytes>> {
        Ok(fsx::read_optional(&self.auth_path())?.map(SecretBytes::new))
    }

    /// Remove the home now (also done on drop); a file a just-stopped
    /// helper still holds open on Windows is retried briefly.
    pub fn remove(self) -> Result<()> {
        let path = self.path.clone();
        drop(self);
        if path.exists() {
            return Err(Error::other(format!(
                "could not remove {}; it is removed at the next start",
                path.display()
            )));
        }
        Ok(())
    }
}

impl Drop for SessionHome {
    fn drop(&mut self) {
        for wait in [0u64, 50, 200, 800] {
            if wait > 0 {
                std::thread::sleep(Duration::from_millis(wait));
            }
            if std::fs::remove_dir_all(&self.path).is_ok() || !self.path.exists() {
                return;
            }
        }
        tracing::debug!(path = %self.path.display(), "helper home left for the next sweep");
    }
}

/// Remove homes a crashed run left behind.
fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_helper_home_is_private_holds_the_login_and_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::new(tmp.path().join("data"));
        let auth = SecretBytes::new(br#"{"tokens":{"refresh_token":"rt"}}"#.to_vec());
        let home = SessionHome::create(&data, Some(&auth)).unwrap();
        let path = home.path().to_path_buf();
        assert!(path.starts_with(data.chatgpt_sessions_dir()));
        assert_eq!(home.auth().unwrap().unwrap(), auth);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o700);
            assert_eq!(mode(&path.join("auth.json")), 0o600);
        }
        home.remove().unwrap();
        assert!(!path.exists());
        let empty = SessionHome::create(&data, None).unwrap();
        assert!(empty.auth().unwrap().is_none());
        let path = empty.path().to_path_buf();
        drop(empty);
        assert!(!path.exists());
    }
}
