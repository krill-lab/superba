# Data location and disk use

Preferences → Data shows Superba's storage by category, Codex's storage separately,
request retention, and **Clean now**. Choose an empty local directory and **Move all
data** to relocate Superba, including its backups, settings and file-backed keys.
Finish active requests first: the router is stopped while the data is copied and
restarted afterwards with its existing token, port and configuration.

The same operation is available without the GUI:

```console
spb data where
spb data usage
spb data move "D:\Superba 数据"
spb --json data usage
```

On macOS or Linux, pass an absolute local path, for example
`spb data move "/Volumes/Work/Superba data"` or
`spb data move "/mnt/work/Superba data"`.

## Defaults and the fixed pointer

| Platform | Default Superba directory |
| --- | --- |
| Windows | `%LOCALAPPDATA%\krill-code\Superba\data` |
| macOS | `~/Library/Application Support/com.krill-code.Superba` |
| Linux | `$XDG_DATA_HOME/superba`, otherwise `~/.local/share/superba` |

After a move, `data-location.json` stays in this default directory. It contains
only a schema number and the absolute new directory. The app and `spb` use this
same pointer on every start. The coordination lock and GUI handover endpoint also
stay there. Installers and upgrades do not change it. Do not delete the default
directory just because most of its contents have moved.

`SUPERBA_DATA_DIR` chooses an alternative **fixed pointer directory** for isolated
installs or tests; a pointer inside it is followed in the same way. Changing or
removing that variable selects a different independent location. An invalid
pointer or an unavailable drive reports an error; Superba does not silently
create an empty replacement profile.

## What grows

All paths below are relative to Superba's active directory. `spb data usage`
reports logical file bytes, including SQLite WAL files, without opening message
contents. Symlinks and unreadable files are skipped and the count is marked
partial. Compressed/sparse files and filesystem allocation units mean these
numbers can differ from the operating system's “size on disk”.

| Category | Paths | Growth and existing limits |
| --- | --- | --- |
| Router requests | `router/usage.sqlite3`, `-wal`, `-shm` | One row per recorded request. Defaults: 90 days, at most 200,000 rows. Retention runs at router start and hourly; SQLite incrementally reclaims unused pages. The database is not a copy of every conversation. |
| Imported usage | `imported-usage.sqlite3` and companions | Imported cc-switch / AI Toolbox history, deduplicated by source identity. Grows with new imports; request retention does not remove it. |
| Session migration backups | `backups/migrate-*/` | Rollout pre-images and SQLite online backups before a provider migration. Keeps the full pre-images of the latest five completed migrations per Codex home; older ledgers remain. A large session collection or database can therefore produce large backups. |
| Configuration backups | `backups/first-write`, `switch`, `editor`, `login-stash`, `rc`; `journal/` | First-write originals are retained. Switch snapshots keep 20 generations per home; the editor keeps 50 encrypted backups per file/home. Finished switch journals keep 32 entries. Ledgers and other metadata can still accumulate. |
| Cache | `cache/`, `feeds/` | Refetchable model/version feeds, account/probe results and other caches. Clean now removes these. |
| Logs | `logs/`, `router/*.log`, notification logs | GUI startup log is replaced each run; crash log resets above 1 MiB; notification delivery log resets above 512 KiB; self-update keeps four previous logs. Router/service logs can grow with runtime and errors (systemd also uses the OS journal outside this directory). |
| Installers | `downloads/` | Verified ChatGPT/Superba packages and staging; size depends on selected releases. Existing updater pruning runs on successful updates/downloads. Interrupted work can leave files. |
| Programs | `codex/`, `tools/` when present | Downloaded programs, distinct from sessions. Codex version retention keeps three newest versions plus pinned/in-use versions. |
| Settings and other state | `profiles.json`, `state.json`, `secrets/`, `remote/`, remaining router/notification state | Mostly proportional to profiles, hosts and operations. OS-keyring entries are outside the directory; their identifiers do not change during a move. |

**Clean now** removes expired request rows and refetchable caches. It keeps
session/configuration backups, imported history, installers and programs; these
have separate purposes and can be needed for recovery or offline use. The data
block also controls request retention days and the row limit. Size totals are
refreshed after cleanup, with the amount reclaimed.

## Migration guarantees and limits

The app excludes its other operations, takes an interprocess lock, drains its
background probes, and stops the router. It copies into a private sibling staging
directory on the destination volume, flushes and verifies every copied file, then
rebases Superba-owned absolute paths in metadata. Session/configuration backup
contents and credentials remain unchanged. It publishes the directory and
atomically replaces the small pointer. Source removal only begins after the
router has restarted successfully.

An ordinary failure restores the pointer and source router. A durable journal
allows the next app/CLI operation to finish or roll back after process death.
If the OS prevents source cleanup, the result names the old directory so its
remaining copies can be reviewed. A failure of rollback itself leaves both copies
and the journal for recovery, and is reported rather than hidden.

The destination must be absolute, writable, empty/new, local, outside system
directories, and disjoint from the source, fixed pointer directory and Codex home.
The MSI folder check uses the same local/writable/system-directory policy. UNC,
mapped network disks, junction/symlink destinations, special files, and known
network/FUSE filesystems are rejected. Free space must cover all files plus the
larger of 16 MiB or 1% headroom. A file/drive changing during copying can still
cause failure, in which case the old data remains available.

Before moving, set an app Codex version pin to **Bundled**. Such pins can leave
external app/launcher settings pointing to a program inside the old directory.
Choose the desired version again after moving. Complete recovery of any
interrupted switch first. Other running copies of Superba must use a version
that understands the fixed pointer; close older copies before moving.

## Codex sessions are separate

Codex's default home is `~/.codex` (Windows: `%USERPROFILE%\.codex`), overridden by
`CODEX_HOME`. Its live/archived rollouts are under `sessions/` and
`archived_sessions/`; they grow with conversations, tool output and attachments.
Codex's SQLite state, history, logs, caches and plugins contribute additional
space. `CODEX_SQLITE_HOME` can put the database elsewhere. The data block reports
the entire Codex home, its session subset, and a separate state database when
configured. These numbers are **not** added to Superba's total.

An issue saying “AppData” alone does not establish which application's files are
large. The default Superba location is in AppData on Windows; default Codex
rollouts are elsewhere. Superba's migration backups can themselves contain old
copies of Codex rollouts. Inspect the displayed category sizes before choosing
what to move. There is no universal size estimate: two users' conversations,
request volume and downloaded releases can differ by orders of magnitude.

### Design for a future Codex-home move

This feature relocates Superba only. Automatically changing `CODEX_HOME` is not
safe without coordinating all Codex CLI, desktop/app-server and IDE processes,
their launch environments, SQLite WAL writers, and database rollout paths.
A GUI-launched app and a terminal can inherit different environments; setting a
variable in one process does not configure the other. Remote hosts and WSL have
their own homes.

A future opt-in flow would require all local Codex processes to stop, inventory
the actual homes used by the app and CLI, back up and verify the complete home
and any external SQLite location, update their supported launch settings, then
verify that both resolve the same new home and can reopen old conversations.
It would retain the old data until these checks pass. Moving only `sessions/`
would also need to preserve database rollout paths, usually through a tested
local directory link or a supported Codex setting; it must not merely rename
the folder. Until that full workflow is verified, Codex relocation remains a
design rather than an automatic button.
