//! Local UI lock, independent of provider credentials and the router. This is
//! an application privacy lock, not an OS security boundary against this user.
use std::{fs, num::NonZeroU32, path::PathBuf};

use ring::{
    pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};

const ROUNDS: u32 = 600_000;

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Mode {
    #[default]
    EveryOpen,
    PerBoot,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Credential {
    salt: [u8; 16],
    digest: [u8; 32],
}

impl Credential {
    pub fn create(password: &str) -> Result<Self, String> {
        let mut salt = [0; 16];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| "random source unavailable".to_owned())?;
        let mut digest = [0; 32];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(ROUNDS).unwrap(),
            &salt,
            password.as_bytes(),
            &mut digest,
        );
        Ok(Self { salt, digest })
    }

    pub fn verify(&self, password: &str) -> bool {
        pbkdf2::verify(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(ROUNDS).unwrap(),
            &self.salt,
            password.as_bytes(),
            &self.digest,
        )
        .is_ok()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Config {
    version: u32,
    pub mode: Mode,
    pub credential: Option<Credential>,
    revision: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            mode: Mode::EveryOpen,
            credential: None,
            revision: String::new(),
        }
    }
}

impl Config {
    pub fn changed(mode: Mode, credential: Option<Credential>) -> Self {
        Self {
            version: 1,
            mode,
            credential,
            revision: uuid::Uuid::new_v4().to_string(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct BootSession {
    boot_id: String,
    revision: String,
}

#[derive(Clone)]
pub(super) struct Store {
    pub dir: PathBuf,
    pub boot_id: Option<String>,
}

impl Store {
    pub fn new(anchor: PathBuf) -> Self {
        // Linux changes this identifier on every boot. Never approximate it
        // from wall-clock time: a missing identifier falls back to prompting.
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        Self {
            dir: anchor.join("gui-lock"),
            boot_id,
        }
    }

    pub fn load(&self) -> Result<Config, String> {
        match fs::read(self.dir.join("settings.json")) {
            Ok(bytes) => {
                let config: Config = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                if config.version != 1
                    || (config.credential.is_some() && config.revision.is_empty())
                {
                    return Err("unsupported or incomplete password settings".into());
                }
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn save(&self, config: &Config) -> Result<(), String> {
        superba_storage::private_dir(&self.dir).map_err(|e| e.to_string())?;
        // Both files use the existing atomic writer (0600 files on Unix).
        superba_storage::write_json(&self.dir.join("settings.json"), config)
            .map_err(|e| e.to_string())
    }

    pub fn remembered(&self, config: &Config) -> bool {
        if config.mode != Mode::PerBoot || config.credential.is_none() {
            return false;
        }
        let Some(boot) = self.boot_id.as_ref() else {
            return false;
        };
        fs::read(self.dir.join("session.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<BootSession>(&bytes).ok())
            .is_some_and(|s| s.boot_id == *boot && s.revision == config.revision)
    }

    pub fn remember(&self, config: &Config) -> Result<(), String> {
        self.forget()?;
        if config.mode == Mode::PerBoot
            && config.credential.is_some()
            && let Some(boot_id) = &self.boot_id
        {
            superba_storage::write_json(
                &self.dir.join("session.json"),
                &BootSession {
                    boot_id: boot_id.clone(),
                    revision: config.revision.clone(),
                },
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn forget(&self) -> Result<(), String> {
        match fs::remove_file(self.dir.join("session.json")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_salted_and_verified_without_plaintext_storage() {
        let password = "local-test-password-中文";
        let first = Credential::create(password).unwrap();
        let second = Credential::create(password).unwrap();
        assert!(first.verify(password));
        assert!(!first.verify("wrong password"));
        assert_ne!(first.salt, second.salt);
        assert_ne!(first.digest, second.digest);
        assert!(!serde_json::to_string(&first).unwrap().contains(password));
    }

    #[test]
    fn boot_session_survives_app_restart_but_not_boot_or_password_changes() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::new(root.path().to_owned());
        store.boot_id = Some("boot-a".into());
        let credential = Credential::create("test-password").unwrap();
        let config = Config::changed(Mode::PerBoot, Some(credential.clone()));
        store.save(&config).unwrap();
        assert!(!store.remembered(&config));
        store.remember(&config).unwrap();
        assert!(store.clone().remembered(&store.load().unwrap()));
        store.boot_id = Some("boot-b".into());
        assert!(!store.remembered(&config));
        store.boot_id = None;
        assert!(!store.remembered(&config));
        store.boot_id = Some("boot-a".into());
        assert!(!store.remembered(&Config::changed(Mode::PerBoot, Some(credential))));
        store.forget().unwrap();
        assert!(!store.remembered(&config));
    }

    #[test]
    fn every_open_never_uses_session_and_corrupt_settings_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().to_owned());
        assert!(store.load().unwrap().credential.is_none());
        let config = Config::changed(
            Mode::EveryOpen,
            Some(Credential::create("test-password").unwrap()),
        );
        store.save(&config).unwrap();
        store.remember(&config).unwrap();
        assert!(!store.remembered(&config));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.dir.join("settings.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::write(store.dir.join("settings.json"), "broken").unwrap();
        assert!(store.load().is_err());
    }
}
