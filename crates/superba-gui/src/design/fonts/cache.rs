//! Disposable, bounded cache of fonts converted by CoreText on this machine.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use skrifa::{FontRef, MetadataProvider, raw::TableProvider};

use super::CjkFace;

const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FACE_BYTES: u64 = 48 * 1024 * 1024;
// Bump when the CoreText encoder changes its output format.
const VERSION: &str = "ct-v1";

#[derive(Clone)]
pub(super) struct FontCache {
    directory: PathBuf,
    limit: u64,
}

impl FontCache {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            limit: MAX_BYTES,
        }
    }

    fn key(&self, face: &CjkFace) -> Option<PathBuf> {
        let meta = fs::metadata(&face.path).ok()?;
        let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        let path = fs::canonicalize(&face.path).ok()?;
        let hash = Sha256::digest(path.as_os_str().as_encoded_bytes());
        let hash: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        Some(self.directory.join(format!(
            "{VERSION}-{hash}-{}-{}-{}-{}.ttf",
            modified.as_nanos(),
            meta.len(),
            face.index,
            face.weight,
        )))
    }

    pub(super) fn read(&self, face: &CjkFace) -> Option<Vec<u8>> {
        let key = self.key(face)?;
        let file = File::open(&key).ok()?;
        let len = file.metadata().ok()?.len();
        if len == 0 || len > MAX_FACE_BYTES {
            return None;
        }
        let mut bytes = Vec::with_capacity(len as usize);
        file.take(MAX_FACE_BYTES + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > MAX_FACE_BYTES || !valid_font(&bytes) {
            let _ = fs::remove_file(key);
            return None;
        }
        Some(bytes)
    }

    /// The key is captured before conversion. Never label an old result with a
    /// new source's metadata if the OS replaced the collection during conversion.
    pub(super) fn convert(
        &self,
        face: &CjkFace,
        convert: impl FnOnce() -> Option<Vec<u8>>,
    ) -> Option<Vec<u8>> {
        let before = self.key(face);
        let bytes = convert()?;
        if let Some(key) = before
            && self.key(face).as_ref() == Some(&key)
        {
            // An unwritable/full cache never prevents font rendering.
            if let Err(error) = self.write(&key, &bytes) {
                tracing::debug!(%error, "Could not cache converted CJK font");
            }
        }
        Some(bytes)
    }

    fn write(&self, key: &Path, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.len() as u64 > MAX_FACE_BYTES.min(self.limit) {
            return Ok(());
        }
        fs::create_dir_all(&self.directory)?;
        let temp = self
            .directory
            .join(format!("{VERSION}-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(bytes)?;
            // Atomic publication: another startup cannot read a partial font.
            drop(file);
            fs::rename(&temp, key)
        })();
        let _ = fs::remove_file(temp);
        self.prune();
        result
    }

    /// Old entries are evicted first, including partial writes. Removing an
    /// active temporary file under pressure only prevents caching that result;
    /// its in-memory font is still usable.
    pub(super) fn prune(&self) {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        let mut fonts = Vec::new();
        let mut total = 0_u64;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let modified = meta.modified().unwrap_or(UNIX_EPOCH);
            let temporary = path.extension().is_some_and(|e| e == "tmp");
            if temporary
                && SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or_default()
                    > Duration::from_secs(3600)
                && fs::remove_file(&path).is_ok()
            {
                continue;
            }
            if temporary || path.extension().is_some_and(|e| e == "ttf") {
                total = total.saturating_add(meta.len());
                fonts.push((modified, path, meta.len()));
            }
        }
        fonts.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        for (_, path, len) in fonts {
            if total <= self.limit {
                break;
            }
            if fs::remove_file(path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }
}

fn valid_font(bytes: &[u8]) -> bool {
    let Ok(font) = FontRef::new(bytes) else {
        return false;
    };
    // The encoder always writes a static TrueType. Check the lazy tables and an
    // actual outline before egui sees data from an interrupted/corrupt cache.
    (|| {
        font.head().ok()?;
        font.hhea().ok()?;
        font.hmtx().ok()?;
        font.maxp().ok()?;
        font.loca(None).ok()?;
        font.glyf().ok()?;
        font.glyph_metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        )
        .bounds(
            font.charmap()
                .map('中')
                .or_else(|| font.charmap().map('A'))?,
        )?;
        Some(())
    })()
    .is_some()
}

#[cfg(test)]
#[path = "../../../tests/unit/font_cache.rs"]
mod tests;
