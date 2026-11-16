//! Codex data location (GitHub issue 15): Codex's conversation rollouts
//! (`sessions/`, `archived_sessions/`) on another local disk.
//!
//! The Codex home itself never moves. Each data directory is copied into the
//! chosen folder, verified file by file (size, modification time, SHA-256 of
//! the copy read back), and then replaced in the Codex home by a directory
//! junction (Windows; no administrator rights needed) or a symbolic link
//! (macOS, Linux) with the same name. Every Codex consumer — the desktop app,
//! the CLI, IDE extensions, terminals opened before the move, the app's "Run
//! Codex in WSL" mode (which forwards the Windows `CODEX_HOME`) — keeps using
//! the same `CODEX_HOME` and the same paths, so nothing can disagree about
//! where the data is, and nothing that stores absolute rollout paths (Codex's
//! `threads.rollout_path`, Superba's `state.json` keyed by home, migration
//! ledgers) needs rewriting. Both directories always move together: Codex
//! archives a conversation with a plain rename between them.
//!
//! Safety: the caller refuses while Codex runs and holds `state.lock`. A
//! journal in Superba's data directory is written before anything is created.
//! The old data is only renamed aside (same directory) until the link is in
//! place and the data read through it matches what was copied; the record is
//! the commit point. The old copy is deleted only after the commit and only
//! when it still matches the new copy; otherwise it is kept and reported.
//! A failure before the commit (or a crash, through [`recover`]) restores the
//! old placement and removes only the copies this move created.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use superba_core::data::{
    CODEX_DATA_DIRS, CodexDataDir, CodexDataLocation, CodexDataMoveItem, CodexDataMoveReport,
    CodexDataPlan, CodexDirPlacement, DataMovePhase, DataMoveProgress, DataMoveProgressSink,
};
use superba_core::paths::{canonical, display_path, plain_path};
use superba_core::{CodexHome, DataDir, EnvSource, Error, LocalizedText, Result};

use crate::atomic::{private_dir, read_json, sync_dir, write_json};
use crate::relocation::{Entry, check_space, copy_file_verified, inventory_except};
use crate::validation::{is_link, planned_path, validate_directory};

/// Superba's record of moved Codex data, per Codex home (in its data dir).
pub const RECORD_FILE: &str = "codex-data.json";
/// The journal of a move in progress (in Superba's data dir).
pub const JOURNAL_FILE: &str = "codex-data-move.json";
/// Written into the destination folder: names the Codex home it serves, so a
/// link stays recognisable as Superba's even without the record.
pub const MARKER_FILE: &str = ".superba-codex-data";
const STAGE_PREFIX: &str = ".superba-codex-stage-";
const ASIDE_PREFIX: &str = ".superba-codex-old-";
const LINK_PREFIX: &str = ".superba-codex-link-";

/// Free bytes on the volume holding a path.
pub type SpaceFn = Arc<dyn Fn(&Path) -> io::Result<u64> + Send + Sync>;
/// Create a directory link at the second path pointing to the first.
pub type LinkFn = Arc<dyn Fn(&Path, &Path) -> io::Result<()> + Send + Sync>;

/// The file system operations a test replaces.
#[derive(Clone)]
pub struct CodexDataFs {
    /// Free space (fs4 in production).
    pub available: SpaceFn,
    /// Link creation ([`create_dir_link`] in production).
    pub link: LinkFn,
}

impl Default for CodexDataFs {
    fn default() -> Self {
        Self {
            available: Arc::new(|path| fs4::available_space(path)),
            link: Arc::new(create_dir_link),
        }
    }
}

impl std::fmt::Debug for CodexDataFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodexDataFs")
    }
}

/// Create a directory junction (Windows) or symbolic link (elsewhere) at
/// `link` pointing to the absolute directory `target`.
pub fn create_dir_link(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        junction::create(target, link)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Remove a link made by [`create_dir_link`], never what it points to.
pub fn remove_dir_link(link: &Path) -> io::Result<()> {
    // RemoveDirectoryW deletes a junction itself; on Unix a symlink to a
    // directory is removed with unlink.
    #[cfg(windows)]
    {
        fs::remove_dir(link)
    }
    #[cfg(not(windows))]
    {
        fs::remove_file(link)
    }
}

#[cfg(windows)]
mod junction {
    //! A mount-point reparse point (what `mklink /J` creates). Unlike symbolic
    //! links, junctions need neither administrator rights nor Developer Mode.
    //! Not exercised by CI on Linux; checked on Windows by the storage tests.
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    /// MAXIMUM_REPARSE_DATA_BUFFER_SIZE.
    const MAX_BUFFER: usize = 16 * 1024;

    pub(super) fn create(target: &Path, link: &Path) -> io::Result<()> {
        // `\\?\D:\x` (canonical form) and `D:\x` both become `\??\D:\x`.
        let target = superba_core::paths::plain_path(target);
        let text = target.as_os_str().to_string_lossy().replace('/', "\\");
        let b = text.as_bytes();
        if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a junction needs an absolute drive path",
            ));
        }
        let print: Vec<u16> = text.encode_utf16().collect();
        let substitute: Vec<u16> = r"\??\"
            .encode_utf16()
            .chain(print.iter().copied())
            .collect();
        let sub_bytes = substitute.len() * 2;
        let print_bytes = print.len() * 2;
        // Mount point header (4 x u16) + both names with their NULs.
        let data_len = 8 + sub_bytes + 2 + print_bytes + 2;
        if 8 + data_len > MAX_BUFFER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the junction target path is too long",
            ));
        }
        let mut buffer = Vec::with_capacity(8 + data_len);
        buffer.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buffer.extend((data_len as u16).to_le_bytes());
        buffer.extend(0u16.to_le_bytes());
        buffer.extend(0u16.to_le_bytes());
        buffer.extend((sub_bytes as u16).to_le_bytes());
        buffer.extend(((sub_bytes + 2) as u16).to_le_bytes());
        buffer.extend((print_bytes as u16).to_le_bytes());
        for unit in substitute.iter().chain(&[0]).chain(&print).chain(&[0]) {
            buffer.extend(unit.to_le_bytes());
        }
        std::fs::create_dir(link)?;
        let result = set_reparse_point(link, &buffer);
        if result.is_err() {
            let _ = std::fs::remove_dir(link);
        }
        result
    }

    fn set_reparse_point(link: &Path, buffer: &[u8]) -> io::Result<()> {
        let wide: Vec<u16> = link.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is NUL-terminated; the handle is closed below.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut returned = 0u32;
        // SAFETY: `buffer` holds a complete REPARSE_DATA_BUFFER of its length.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        let error = (ok == 0).then(io::Error::last_os_error);
        // SAFETY: a valid handle from CreateFileW, closed once.
        unsafe { CloseHandle(handle) };
        error.map_or(Ok(()), Err)
    }
}

// ---------------------------------------------------------------------------
// Record and marker
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RecordFile {
    #[serde(default)]
    schema: u32,
    /// Keyed by the canonical Codex home.
    #[serde(default)]
    homes: BTreeMap<String, HomeRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct HomeRecord {
    /// The folder holding the data (`None`: moved back into the home).
    #[serde(default)]
    directory: Option<PathBuf>,
    /// Id of the move that wrote this record.
    #[serde(default)]
    id: String,
    /// When (milliseconds since the epoch).
    #[serde(default)]
    moved_at_ms: i64,
    /// Old copies the move could not remove.
    #[serde(default)]
    leftovers: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Marker {
    schema: u32,
    codex_home: PathBuf,
    id: String,
}

fn home_key(root: &Path) -> String {
    let root = canonical(root).unwrap_or_else(|_| root.to_path_buf());
    root.to_string_lossy().into_owned()
}

fn read_record(data: &DataDir) -> Result<RecordFile> {
    Ok(read_json::<RecordFile>(&data.root().join(RECORD_FILE))?.unwrap_or_default())
}

fn update_record(data: &DataDir, home: &Path, f: impl FnOnce(&mut HomeRecord)) -> Result<()> {
    let mut file = read_record(data)?;
    file.schema = 1;
    let entry = file.homes.entry(home_key(home)).or_default();
    f(entry);
    write_json(&data.root().join(RECORD_FILE), &file)
}

fn read_marker(dir: &Path) -> Option<Marker> {
    read_json::<Marker>(&dir.join(MARKER_FILE)).ok().flatten()
}

fn marker_for(dir: &Path, home: &Path) -> bool {
    read_marker(dir).is_some_and(|m| home_key(&m.codex_home) == home_key(home))
}

/// Same file system object, or the same text (case-insensitive on Windows).
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if let (Ok(x), Ok(y)) = (canonical(a), canonical(b))
        && x == y
    {
        return true;
    }
    let (x, y) = (display_path(&plain_path(a)), display_path(&plain_path(b)));
    if cfg!(windows) {
        x.eq_ignore_ascii_case(&y)
    } else {
        x == y
    }
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

fn link_target(path: &Path) -> PathBuf {
    let target = fs::read_link(path)
        .map(|t| plain_path(&t))
        .unwrap_or_default();
    if target.is_relative() {
        path.parent().map(|p| p.join(&target)).unwrap_or(target)
    } else {
        target
    }
}

fn placement(
    path: &Path,
    name: &str,
    home: &Path,
    record: Option<&HomeRecord>,
) -> CodexDirPlacement {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => CodexDirPlacement::Missing,
        Err(_) => CodexDirPlacement::Other,
        Ok(meta) if is_link(&meta) => {
            let target = link_target(path);
            let reachable = fs::metadata(path).is_ok_and(|m| m.is_dir());
            let by_record = record
                .and_then(|r| r.directory.as_ref())
                .is_some_and(|d| same_path(&d.join(name), &target));
            let by_marker = target
                .file_name()
                .is_some_and(|n| n == std::ffi::OsStr::new(name))
                && target.parent().is_some_and(|p| marker_for(p, home));
            CodexDirPlacement::Linked {
                target,
                managed: by_record || by_marker,
                reachable,
            }
        }
        Ok(meta) if meta.is_dir() => CodexDirPlacement::InHome,
        Ok(_) => CodexDirPlacement::Other,
    }
}

fn measure(root: &Path) -> (u64, u64) {
    let (mut bytes, mut files, mut skipped) = (0u64, 0u64, 0u64);
    crate::usage::walk(
        root,
        &mut |_, n| {
            bytes = bytes.saturating_add(n);
            files += 1;
        },
        &mut skipped,
    );
    (bytes, files)
}

/// Where the data of `home` is, with sizes (walks the directories).
pub fn inspect(home: &CodexHome, data: &DataDir) -> CodexDataLocation {
    locate_with(home, data, true)
}

/// [`inspect`] without sizes (no directory walk; for doctor checks).
pub fn locate(home: &CodexHome, data: &DataDir) -> CodexDataLocation {
    locate_with(home, data, false)
}

fn locate_with(home: &CodexHome, data: &DataDir, sizes: bool) -> CodexDataLocation {
    let record = read_record(data).unwrap_or_default();
    let entry = record.homes.get(&home_key(home.root()));
    let dirs: Vec<CodexDataDir> = CODEX_DATA_DIRS
        .iter()
        .map(|name| {
            let path = home.root().join(name);
            let placement = placement(&path, name, home.root(), entry);
            let (bytes, files) = match &placement {
                _ if !sizes => (0, 0),
                CodexDirPlacement::InHome => measure(&path),
                CodexDirPlacement::Linked {
                    reachable: true, ..
                } => canonical(&path).map(|p| measure(&p)).unwrap_or_default(),
                _ => (0, 0),
            };
            CodexDataDir {
                name: (*name).to_owned(),
                path,
                placement,
                bytes,
                files,
            }
        })
        .collect();
    let directory = dirs.iter().find_map(|d| match &d.placement {
        CodexDirPlacement::Linked {
            target,
            managed: true,
            ..
        } => target.parent().map(Path::to_path_buf),
        _ => None,
    });
    CodexDataLocation {
        codex_home: home.root().to_path_buf(),
        directory,
        dirs,
        pending: data.root().join(JOURNAL_FILE).exists(),
        leftovers: entry.map(|e| e.leftovers.clone()).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// Everything a plan needs from the caller.
#[derive(Clone, Copy)]
pub struct Scope<'a> {
    /// The local Codex home.
    pub home: &'a CodexHome,
    /// Superba's data directory (record, journal; must not overlap).
    pub data: &'a DataDir,
    /// Environment (system folder checks on Windows).
    pub env: &'a dyn EnvSource,
    /// File system hooks.
    pub fs: &'a CodexDataFs,
}

#[derive(Clone, Debug)]
struct Item {
    name: String,
    /// The real directory holding the data now (`None`: not created yet).
    from: Option<PathBuf>,
    /// `<home>/<name>` is a link now.
    from_link: bool,
    /// The real directory after the move.
    to: PathBuf,
    bytes: u64,
    files: u64,
}

struct Planned {
    plan: CodexDataPlan,
    blockers: Vec<Error>,
    items: Vec<Item>,
    /// The planned destination folder (canonical form), `None` = the home.
    root: Option<PathBuf>,
}

fn has_data_name(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(n) => CODEX_DATA_DIRS.iter().any(|d| {
            let n = n.to_string_lossy();
            if cfg!(windows) {
                n.eq_ignore_ascii_case(d)
            } else {
                n == *d
            }
        }),
        _ => false,
    })
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn real(path: &Path) -> PathBuf {
    canonical(path).unwrap_or_else(|_| {
        planned_path(path)
            .map(|p| p.0)
            .unwrap_or_else(|_| path.to_path_buf())
    })
}

/// An identifier of the volume holding `path` (nearest existing ancestor).
fn volume(path: &Path) -> Option<String> {
    let (_, existing) = planned_path(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&existing).ok().map(|m| m.dev().to_string())
    }
    #[cfg(not(unix))]
    {
        let text = display_path(&existing);
        text.get(..2).map(str::to_ascii_lowercase)
    }
}

fn plan_inner(scope: Scope<'_>, destination: Option<&Path>) -> Result<Planned> {
    let home = scope.home.root();
    let location = inspect(scope.home, scope.data);
    let mut blockers = Vec::new();
    let mut notes = Vec::new();
    if location.pending {
        blockers.push(Error::invalid("data.codex.pending"));
    }
    for dir in &location.dirs {
        match &dir.placement {
            CodexDirPlacement::Other => blockers.push(Error::invalid("data.codex.other")),
            CodexDirPlacement::Linked { managed: false, .. } => {
                blockers.push(Error::invalid("data.codex.unmanaged"));
            }
            CodexDirPlacement::Linked {
                reachable: false, ..
            } => blockers.push(Error::invalid("data.codex.unreachable")),
            _ => {}
        }
    }
    let root = match destination {
        None => None,
        Some(d) => match validate_directory(d, scope.env) {
            Ok(full) => Some(full),
            Err(e) => {
                blockers.push(e);
                None
            }
        },
    };
    if let Some(root) = &root {
        let home_real = real(home);
        if has_data_name(root) {
            blockers.push(Error::invalid("data.codex.name"));
        }
        if overlaps(root, &home_real)
            || overlaps(root, &real(scope.data.root()))
            || overlaps(root, &real(scope.data.anchor()))
        {
            blockers.push(Error::invalid("data.codex.overlap"));
        }
    }
    let mut items = Vec::new();
    if destination.is_none() || root.is_some() {
        for dir in &location.dirs {
            let (from, from_link) = match &dir.placement {
                CodexDirPlacement::Missing => (None, false),
                CodexDirPlacement::InHome => (Some(dir.path.clone()), false),
                CodexDirPlacement::Linked {
                    target,
                    managed: true,
                    reachable: true,
                } => (Some(real(target)), true),
                // Blocked above.
                _ => continue,
            };
            let to = match &root {
                Some(root) => root.join(&dir.name),
                None => dir.path.clone(),
            };
            let already = match &root {
                Some(_) => from.as_deref().is_some_and(|f| same_path(f, &to)),
                None => !from_link,
            };
            if already {
                continue;
            }
            items.push(Item {
                name: dir.name.clone(),
                from,
                from_link,
                to,
                bytes: dir.bytes,
                files: dir.files,
            });
        }
        if items.is_empty() && blockers.is_empty() {
            blockers.push(Error::invalid(if destination.is_none() {
                "data.codex.not_moved"
            } else {
                "data.codex.same"
            }));
        }
    }
    // The destination must hold nothing but Superba's own move of this home.
    if let Some(root) = &root
        && root.exists()
    {
        let ours = location
            .directory
            .as_deref()
            .is_some_and(|d| same_path(d, root));
        let foreign = match fs::read_dir(root) {
            Ok(entries) => entries.flatten().any(|e| {
                let name = e.file_name();
                !(ours
                    && (name == MARKER_FILE
                        || (CODEX_DATA_DIRS.iter().any(|d| name == *d)
                            && !items.iter().any(|i| name == i.name.as_str()))))
            }),
            Err(e) => {
                blockers.push(Error::io(root, &e));
                false
            }
        };
        if foreign {
            blockers.push(Error::invalid("data.path.not_empty"));
        }
        for item in &items {
            if let Some(f) = &item.from
                && overlaps(root, f)
            {
                blockers.push(Error::invalid("data.codex.overlap"));
            }
        }
    }
    let total_bytes = items.iter().map(|i| i.bytes).sum();
    let space_at = root.clone().unwrap_or_else(|| home.to_path_buf());
    let available_bytes = planned_path(&space_at)
        .ok()
        .and_then(|(_, existing)| (scope.fs.available)(&existing).ok());
    if let Some(available) = available_bytes
        && !items.is_empty()
        && let Err(e) = check_space(total_bytes, available)
    {
        blockers.push(e);
    }
    if !items.is_empty() {
        let target_volume = volume(&space_at);
        let same = items.iter().all(|i| {
            i.from
                .as_deref()
                .is_none_or(|f| target_volume.is_some() && volume(f) == target_volume)
        }) && items.iter().any(|i| i.from.is_some());
        if same {
            notes.push(LocalizedText::new(
                "data.codex.note.same_volume",
                "The new folder is on the same disk as the data now, so moving frees no space there.",
            ));
        }
        notes.push(match &root {
            Some(_) => LocalizedText::new(
                "data.codex.note.link",
                "Codex keeps using {home}: its session folders become links to the new folder, so the app, the CLI and editors need no change. Keep the drive connected while Codex runs.",
            )
            .arg("home", display_path(home)),
            None => LocalizedText::new(
                "data.codex.note.back",
                "The session data is copied back into {home} and the links are removed.",
            )
            .arg("home", display_path(home)),
        });
    }
    blockers.dedup();
    let plan = CodexDataPlan {
        location,
        destination: destination.map(|_| root.clone().unwrap_or_default()),
        moves: items
            .iter()
            .map(|i| CodexDataMoveItem {
                name: i.name.clone(),
                from: i.from.clone().unwrap_or_else(|| home.join(&i.name)),
                to: i.to.clone(),
                bytes: i.bytes,
                files: i.files,
            })
            .collect(),
        total_bytes,
        available_bytes,
        blockers: blockers.iter().map(Error::to_text).collect(),
        notes,
    };
    Ok(Planned {
        plan,
        blockers,
        items,
        root,
    })
}

/// Preview a move to `destination` (`None`: back into the Codex home). The
/// only side effect is the destination's write probe.
pub fn plan(scope: Scope<'_>, destination: Option<&Path>) -> Result<CodexDataPlan> {
    Ok(plan_inner(scope, destination)?.plan)
}

// ---------------------------------------------------------------------------
// The move
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    /// Copies are being made; nothing in the Codex home changed.
    Copying,
    /// Links / directories in the Codex home are being swapped.
    Switching,
    /// The record names the new placement; only cleanup is left.
    Committed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalItem {
    name: String,
    from: Option<PathBuf>,
    from_link: bool,
    /// The real directory after the move.
    to: PathBuf,
    /// The verified copy before it is published.
    staging: PathBuf,
    /// Renamed to `<home>/<name>` by the switch: a new link (moving out) or
    /// the staged copy (moving back).
    incoming: PathBuf,
    /// Where `<home>/<name>` is renamed to by the switch.
    aside: PathBuf,
    /// The copy was renamed to `to` (moving out): only then may a rollback
    /// delete `to`. A crash right after the rename leaves an orphan copy,
    /// never a deleted folder this move did not create.
    #[serde(default)]
    published: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    id: String,
    codex_home: PathBuf,
    /// `None`: back into the Codex home.
    destination: Option<PathBuf>,
    /// This move created the destination folder.
    created_root: bool,
    stage_root: PathBuf,
    phase: Phase,
    items: Vec<JournalItem>,
}

impl Journal {
    fn path(data: &DataDir) -> PathBuf {
        data.root().join(JOURNAL_FILE)
    }

    fn write(&self, data: &DataDir) -> Result<()> {
        write_json(&Self::path(data), self)
    }

    fn read(data: &DataDir) -> Result<Option<Self>> {
        let journal: Option<Self> = read_json(&Self::path(data))?;
        if let Some(j) = &journal {
            let home = &j.codex_home;
            let ours = |p: &Path, prefix: &str, parent: &Path| {
                p.parent() == Some(parent)
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(&format!("{prefix}{}", j.id)))
            };
            let stage_parent = j.destination.clone().unwrap_or_else(|| home.clone());
            let valid = j.schema == 1
                && uuid::Uuid::parse_str(&j.id).is_ok()
                && home.is_absolute()
                && ours(&j.stage_root, STAGE_PREFIX, &stage_parent)
                && j.items.iter().all(|i| {
                    CODEX_DATA_DIRS.contains(&i.name.as_str())
                        && i.staging == j.stage_root.join(&i.name)
                        && ours(&i.aside, ASIDE_PREFIX, home)
                        && match &j.destination {
                            Some(d) => {
                                i.to == d.join(&i.name) && ours(&i.incoming, LINK_PREFIX, home)
                            }
                            None => i.to == home.join(&i.name) && i.incoming == i.staging,
                        }
                });
            if !valid {
                return Err(Error::invalid("data.codex.journal"));
            }
        }
        Ok(journal)
    }

    fn clear(data: &DataDir) -> Result<()> {
        let path = Self::path(data);
        match fs::remove_file(&path) {
            Ok(()) => sync_dir(data.root()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&path, &e)),
        }
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// A rename in the Codex home. Windows refuses to rename a directory while a
/// program has a file in it open: that is reported as "in use".
fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).map_err(|e| {
        // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
        if cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33)) {
            Error::Conflict {
                message: "data.codex.in_use".into(),
            }
        } else {
            Error::io(from, &e)
        }
    })
}

fn tree(root: &Path) -> Result<Vec<Entry>> {
    inventory_except(root, &|_| false)
}

/// The data read through `<home>/<name>` (following the top-level link).
fn tree_through(path: &Path) -> Result<Vec<Entry>> {
    let real = canonical(path).map_err(|e| Error::io(path, &e))?;
    tree(&real)
}

fn emit(progress: &DataMoveProgressSink, phase: DataMovePhase, copied: u64, total: u64) -> bool {
    progress(DataMoveProgress {
        phase,
        copied_bytes: copied,
        total_bytes: total,
    })
}

fn copy_tree(
    from: &Path,
    to: &Path,
    copied: &mut u64,
    total: u64,
    progress: &DataMoveProgressSink,
) -> Result<Vec<Entry>> {
    let entries = tree(from)?;
    private_dir(to)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    for entry in &entries {
        let source = from.join(&entry.relative);
        let target = to.join(&entry.relative);
        if entry.directory {
            private_dir(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            private_dir(parent)?;
        }
        copy_file_verified(&source, &target, entry, &mut buffer, &mut |n| {
            *copied += n;
            emit(progress, DataMovePhase::Copying, *copied, total)
        })?;
    }
    for entry in entries.iter().rev().filter(|e| e.directory) {
        sync_dir(&to.join(&entry.relative))?;
    }
    sync_dir(to)?;
    Ok(entries)
}

/// Move the data as [`plan`] describes. The caller has checked that no Codex
/// process uses the home and holds `state.lock`. `progress` returning false
/// cancels while copying; after that the move finishes or rolls back.
pub fn execute(
    scope: Scope<'_>,
    destination: Option<&Path>,
    progress: &DataMoveProgressSink,
) -> Result<CodexDataMoveReport> {
    if !emit(progress, DataMovePhase::Checking, 0, 0) {
        return Err(Error::Cancelled);
    }
    let planned = plan_inner(scope, destination)?;
    if let Some(error) = planned.blockers.into_iter().next() {
        return Err(error);
    }
    let home = scope.home.root().to_path_buf();
    let id = uuid::Uuid::new_v4().to_string();
    let stage_root = planned
        .root
        .clone()
        .unwrap_or_else(|| home.clone())
        .join(format!("{STAGE_PREFIX}{id}"));
    let journal = Journal {
        schema: 1,
        id: id.clone(),
        codex_home: home.clone(),
        destination: planned.root.clone(),
        created_root: planned.root.as_deref().is_some_and(|r| !r.exists()),
        stage_root: stage_root.clone(),
        phase: Phase::Copying,
        items: planned
            .items
            .iter()
            .map(|i| JournalItem {
                name: i.name.clone(),
                from: i.from.clone(),
                from_link: i.from_link,
                to: i.to.clone(),
                staging: stage_root.join(&i.name),
                incoming: if planned.root.is_some() {
                    home.join(format!("{LINK_PREFIX}{id}-{}", i.name))
                } else {
                    stage_root.join(&i.name)
                },
                aside: home.join(format!("{ASIDE_PREFIX}{id}-{}", i.name)),
                published: false,
            })
            .collect(),
    };
    journal.write(scope.data)?;
    let total = planned.plan.total_bytes;
    let mut journal = journal;
    match transfer(scope, &mut journal, total, progress) {
        Ok(()) => {}
        Err(error) => {
            let _ = emit(progress, DataMovePhase::RollingBack, 0, total);
            if let Err(rollback) = roll_back(scope.data, &journal) {
                return Err(Error::other(format!(
                    "{}; Codex data rollback: {}",
                    error.detail(),
                    rollback.detail()
                )));
            }
            return Err(error);
        }
    }
    let _ = emit(progress, DataMovePhase::Cleaning, total, total);
    let leftovers = finish(scope.data, &journal)?;
    Ok(CodexDataMoveReport {
        location: inspect(scope.home, scope.data),
        copied_bytes: total,
        files: planned.items.iter().map(|i| i.files).sum(),
        leftovers,
    })
}

fn transfer(
    scope: Scope<'_>,
    journal: &mut Journal,
    total: u64,
    progress: &DataMoveProgressSink,
) -> Result<()> {
    let home = journal.codex_home.clone();
    if let Some(root) = &journal.destination {
        private_dir(root)?;
        // A folder that already serves this home keeps its marker (and the
        // rollback of this move leaves it alone).
        if !marker_for(root, &home) {
            write_json(
                &root.join(MARKER_FILE),
                &Marker {
                    schema: 1,
                    codex_home: canonical(&home).unwrap_or_else(|_| home.clone()),
                    id: journal.id.clone(),
                },
            )?;
        }
    }
    // Copy and verify everything before touching the Codex home.
    fs::create_dir(&journal.stage_root).map_err(|e| Error::io(&journal.stage_root, &e))?;
    private_dir(&journal.stage_root)?;
    let mut copied = 0u64;
    let mut copies = Vec::new();
    for item in &journal.items {
        let entries = match &item.from {
            Some(from) => copy_tree(from, &item.staging, &mut copied, total, progress)?,
            None => {
                private_dir(&item.staging)?;
                Vec::new()
            }
        };
        copies.push(entries);
    }
    // Nothing may have changed while copying (Codex is not running).
    for (item, entries) in journal.items.iter().zip(&copies) {
        if let Some(from) = &item.from
            && tree(from)? != *entries
        {
            return Err(Error::Conflict {
                message: "data.changed".into(),
            });
        }
    }
    let _ = emit(progress, DataMovePhase::Switching, total, total);
    // Moving out: publish the copies beside the marker and prepare the links.
    if journal.destination.is_some() {
        if journal.items.iter().any(|i| exists(&i.to)) {
            return Err(Error::invalid("data.path.not_empty"));
        }
        for at in 0..journal.items.len() {
            let item = &journal.items[at];
            rename(&item.staging, &item.to)?;
            journal.items[at].published = true;
            journal.write(scope.data)?;
        }
        if let Some(root) = &journal.destination {
            sync_dir(root)?;
        }
        for item in &journal.items {
            (scope.fs.link)(&item.to, &item.incoming).map_err(|e| {
                if matches!(
                    e.kind(),
                    io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
                ) {
                    Error::invalid("data.codex.links")
                } else {
                    Error::io(&item.incoming, &e)
                }
            })?;
        }
    }
    journal.phase = Phase::Switching;
    journal.write(scope.data)?;
    for item in &journal.items {
        let path = home.join(&item.name);
        if exists(&path) {
            rename(&path, &item.aside)?;
        }
        rename(&item.incoming, &path)?;
    }
    sync_dir(&home)?;
    // Codex reads the data through the same path: it must be the copy.
    for (item, entries) in journal.items.iter().zip(&copies) {
        let path = home.join(&item.name);
        let through = canonical(&path).map_err(|e| Error::io(&path, &e))?;
        if !same_path(&through, &item.to) || tree(&through)? != *entries {
            return Err(Error::other("data.verify"));
        }
    }
    update_record(scope.data, &home, |r| {
        r.directory = journal.destination.clone();
        r.id = journal.id.clone();
        r.moved_at_ms = superba_core::Timestamp::now().as_millis();
        r.leftovers.clear();
    })?;
    journal.phase = Phase::Committed;
    journal.write(scope.data)
}

/// Undo an uncommitted move: put the old directories or links back into the
/// Codex home and delete only the copies this move created.
fn roll_back(data: &DataDir, journal: &Journal) -> Result<()> {
    let home = &journal.codex_home;
    for item in journal.items.iter().rev() {
        let path = home.join(&item.name);
        if exists(&item.aside) {
            if exists(&path) {
                if exists(&item.incoming) {
                    // The swap never reached `path`: something else created it
                    // since (Codex started during the move). Keep both.
                    return Err(Error::invalid("data.codex.journal"));
                }
                if journal.destination.is_some() {
                    let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(&path, &e))?;
                    if !is_link(&meta) {
                        return Err(Error::invalid("data.codex.journal"));
                    }
                    remove_dir_link(&path).map_err(|e| Error::io(&path, &e))?;
                } else {
                    rename(&path, &item.incoming)?;
                }
            }
            rename(&item.aside, &path)?;
        }
        if journal.destination.is_some()
            && let Ok(meta) = fs::symlink_metadata(&item.incoming)
            && is_link(&meta)
        {
            remove_dir_link(&item.incoming).map_err(|e| Error::io(&item.incoming, &e))?;
        }
    }
    // Copies this move made: staging, published copies (they did not exist
    // when the journal was written), the marker and a folder it created.
    if exists(&journal.stage_root) {
        fs::remove_dir_all(&journal.stage_root).map_err(|e| Error::io(&journal.stage_root, &e))?;
    }
    if let Some(root) = &journal.destination {
        for item in journal.items.iter().filter(|i| i.published) {
            if let Ok(meta) = fs::symlink_metadata(&item.to)
                && meta.is_dir()
                && !is_link(&meta)
            {
                fs::remove_dir_all(&item.to).map_err(|e| Error::io(&item.to, &e))?;
            }
        }
        if read_marker(root).is_some_and(|m| m.id == journal.id) {
            let marker = root.join(MARKER_FILE);
            fs::remove_file(&marker).map_err(|e| Error::io(&marker, &e))?;
        }
        if journal.created_root {
            let _ = fs::remove_dir(root);
        }
    }
    Journal::clear(data)
}

/// After the commit: delete the old copies that still match the new data,
/// keep (and report) anything else, then clear the journal.
fn finish(data: &DataDir, journal: &Journal) -> Result<Vec<PathBuf>> {
    let home = &journal.codex_home;
    let mut leftovers = Vec::new();
    let mut old_roots = Vec::new();
    for item in &journal.items {
        let path = home.join(&item.name);
        let current = tree_through(&path);
        // The old data: the directory renamed aside, or the old link's target.
        let old = if item.from_link {
            if let Ok(meta) = fs::symlink_metadata(&item.aside)
                && is_link(&meta)
                && remove_dir_link(&item.aside).is_err()
            {
                leftovers.push(item.aside.clone());
            }
            item.from.clone()
        } else if exists(&item.aside) {
            Some(item.aside.clone())
        } else {
            None
        };
        let Some(old) = old else { continue };
        if !exists(&old) {
            continue;
        }
        let unchanged = matches!((&current, tree(&old)), (Ok(a), Ok(b)) if *a == b);
        if !unchanged {
            leftovers.push(old);
            continue;
        }
        match fs::remove_dir_all(&old) {
            Ok(()) => {
                if item.from_link
                    && let Some(parent) = old.parent()
                {
                    old_roots.push(parent.to_path_buf());
                }
            }
            Err(_) => leftovers.push(old),
        }
    }
    // A former destination folder that now holds only Superba's marker.
    old_roots.dedup();
    for root in old_roots {
        if journal
            .destination
            .as_deref()
            .is_some_and(|d| same_path(d, &root))
            || !marker_for(&root, home)
        {
            continue;
        }
        let only_marker = fs::read_dir(&root)
            .map(|entries| entries.flatten().all(|e| e.file_name() == MARKER_FILE))
            .unwrap_or(false);
        if only_marker {
            let _ = fs::remove_file(root.join(MARKER_FILE));
            let _ = fs::remove_dir(&root);
        }
    }
    if exists(&journal.stage_root) && fs::remove_dir_all(&journal.stage_root).is_err() {
        leftovers.push(journal.stage_root.clone());
    }
    update_record(data, home, |r| r.leftovers = leftovers.clone())?;
    Journal::clear(data)?;
    Ok(leftovers)
}

/// What start-up recovery did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovered {
    /// The interrupted move had committed: its cleanup was finished.
    Completed {
        /// Old copies that could not be removed.
        leftovers: Vec<PathBuf>,
    },
    /// The interrupted move had not committed: the old placement is back.
    RolledBack,
}

/// Finish or undo an interrupted move (start-up, under `state.lock`).
pub fn recover(data: &DataDir) -> Result<Option<Recovered>> {
    let Some(journal) = Journal::read(data)? else {
        return Ok(None);
    };
    if journal.phase == Phase::Committed {
        let leftovers = finish(data, &journal)?;
        Ok(Some(Recovered::Completed { leftovers }))
    } else {
        roll_back(data, &journal)?;
        Ok(Some(Recovered::RolledBack))
    }
}
