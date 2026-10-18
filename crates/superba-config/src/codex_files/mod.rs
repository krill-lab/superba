//! Editing Codex's `config.toml` and `auth.json` by hand (GitHub issue 9).
//!
//! Pure parts, shared by the GUI's editor and `spb config edit` through
//! `superba_switch::codex_files`:
//!
//! - [`validate`]: syntax (line and column), `config.toml` against the
//!   settings catalog of every Codex reading the home, the shape of
//!   `auth.json`, partly changed hidden secrets;
//! - [`format()`]: `toml_edit` tidying that keeps every comment and value,
//!   `serde_json` pretty printing for `auth.json`;
//! - [`Masker`]: `auth.json` secrets hidden behind placeholders until revealed;
//! - [`diff_lines`] / [`hunks`] / [`unified`]: the save and restore previews;
//! - [`owned_changes`] / [`credential_change`]: what an edit changes in what
//!   Superba manages (GH#5).
//!
//! [`EditorStore`] keeps the encrypted backups the editor takes before every
//! save, lists them together with the pre-images of Superba's switches and
//! the originals, and records the last save per home (the source of a change
//! the background check finds, and "in effect", GH#4). [`write_file`] replaces
//! a local file atomically when it still holds what the editor opened.

mod diff;
mod format;
mod json;
mod mask;
mod store;
mod validate;

pub use diff::{DiffLine, DiffStat, DiffTag, Hunk, diff_lines, hunks, stat, unified};
pub use format::{format, format_json, format_toml, uses_crlf};
pub use mask::{MASK_CHAR, Masker, has_mask, is_claude_secret_name, is_secret_name};
pub use store::{EditRecord, EditorStore, KEEP_PER_FILE, write_file};
pub use validate::{CheckContext, clean, line_col, validate};

use superba_core::{AuthDrift, KeyChange, OwnedKey};

use crate::toml_keys::{ConfigDoc, reprs_equal};

/// The keys Superba manages that `new` changes compared with `old` (both
/// `config.toml` texts): `before` = the value in `old`, `after` = the value
/// in `new` (`None` = absent). Empty when either text does not parse.
pub fn owned_changes(old: &str, new: &str, owned: &[OwnedKey]) -> Vec<KeyChange> {
    let (Ok(a), Ok(b)) = (
        ConfigDoc::parse(old.as_bytes()),
        ConfigDoc::parse(new.as_bytes()),
    ) else {
        return Vec::new();
    };
    owned
        .iter()
        .filter_map(|o| {
            let (before, after) = (a.get_repr(&o.path), b.get_repr(&o.path));
            let same = match (before.as_deref(), after.as_deref()) {
                (Some(x), Some(y)) => reprs_equal(x, y),
                (None, None) => true,
                _ => false,
            };
            (!same).then(|| KeyChange {
                path: o.path.clone(),
                before,
                after,
            })
        })
        .collect()
}

/// The credential of `auth.json` changes from `old` to `new` (`None` =
/// absent): API key, PAT, `auth_mode` or the ChatGPT login (reformatting and
/// other fields do not count). Shapes only, never a key.
pub fn credential_change(old: Option<&[u8]>, new: Option<&[u8]>) -> Option<AuthDrift> {
    (crate::authjson::credential_digest(old) != crate::authjson::credential_digest(new)).then(
        || AuthDrift {
            written: crate::authjson::drift_shape(old),
            live: crate::authjson::drift_shape(new),
        },
    )
}
