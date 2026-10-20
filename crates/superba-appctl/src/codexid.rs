//! Which program a Codex path really runs, and whether two paths are the same
//! Codex (user bug 18).
//!
//! The desktop app reaches its bundled Codex under several names:
//!
//! - macOS: `ChatGPT.app/Contents/Resources/codex-cli/bin/codex` (the package
//!   entrypoint) is a ~330-byte sh launcher that `exec`s
//!   `codex-cli/CodexCLI.app/Contents/MacOS/codex`, so the app-server's process
//!   image is the second path while the install names the first;
//! - Windows: `resources/codex.exe`, with the Linux `codex` the app runs inside
//!   WSL in the same directory;
//! - Linux: `resources/codex`.
//!
//! `CODEX_CLI_PATH` may also name a directory (the app looks for `codex` /
//! `codex.exe` in it). [`CodexIdentity`] gathers every file one such path stands
//! for; comparisons use file identity ([`same_file`]: `(device, inode)` on Unix,
//! else the canonical path), never the literal path strings.
//!
//! Launchers are only read, never run: a small file (at most
//! [`LAUNCHER_MAX_BYTES`]) with a POSIX shell `#!` line, whose last `exec` names
//! an absolute path or one relative to the script's own directory
//! (`$(dirname "$0")`, `$(cd "$(dirname "$0")" && pwd)`, `${0%/*}` or a variable
//! holding one of them), and only toward a program named like Codex: a wrapper
//! that execs `node` (or cannot be read this way) is itself the program.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, MAIN_SEPARATOR_STR, Path, PathBuf};

/// Largest file read as a possible launcher script (the macOS one is 328 bytes;
/// native Codex binaries are hundreds of MB).
pub const LAUNCHER_MAX_BYTES: u64 = 16 * 1024;

/// The macOS bundled binary relative to the launcher's directory
/// (`codex-cli/bin/`), used when the launcher cannot be read as expected.
pub const MAC_LAUNCHER_TARGET: &str = "../CodexCLI.app/Contents/MacOS/codex";

/// Launchers followed at most (a launcher of a launcher ...).
const MAX_HOPS: usize = 4;

/// Where a launcher script's `exec` goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecTarget {
    /// An absolute path.
    Absolute(PathBuf),
    /// A path relative to the directory holding the script (empty = that directory).
    ScriptDir(PathBuf),
}

/// Every file one Codex path stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexIdentity {
    /// The path as given (program, launcher or directory).
    pub path: PathBuf,
    /// Launcher scripts on the way to [`Self::binary`], the given path first
    /// when it is one.
    pub launchers: Vec<PathBuf>,
    /// The program that runs: the last launcher's `exec` target, the `codex`
    /// (`codex.exe`) of a directory, else the path itself.
    pub binary: PathBuf,
    /// The same Codex build under another name: next to a desktop app's
    /// `codex.exe` (a directory with `app.asar`), the Linux `codex` the app runs
    /// in WSL, and the reverse.
    pub aliases: Vec<PathBuf>,
}

impl CodexIdentity {
    /// Resolve `path`: a directory to its `codex` program, launchers to their
    /// target. Only reads files; a missing path is its own binary.
    pub fn of(path: &Path) -> Self {
        let mut binary = if path.is_dir() {
            dir_program(path)
        } else {
            path.to_path_buf()
        };
        let mut launchers = Vec::new();
        for _ in 0..MAX_HOPS {
            // Only a Codex program continues the chain: a wrapper that execs
            // `node`, `env` or anything else is itself the program.
            match launcher_target(&binary) {
                Some(next)
                    if codex_named(&next)
                        && !same_file(&next, &binary)
                        && !launchers.contains(&next) =>
                {
                    launchers.push(std::mem::replace(&mut binary, next));
                }
                _ => break,
            }
        }
        let aliases = wsl_twin(&binary).into_iter().collect();
        Self {
            path: path.to_path_buf(),
            launchers,
            binary,
            aliases,
        }
    }

    /// The launchers, the binary and the aliases.
    pub fn files(&self) -> impl Iterator<Item = &Path> {
        self.launchers
            .iter()
            .chain([&self.binary])
            .chain(&self.aliases)
            .map(PathBuf::as_path)
    }

    /// `file` (for example a process image) is one of [`Self::files`].
    pub fn contains(&self, file: &Path) -> bool {
        self.files().any(|f| same_file(f, file))
    }

    /// Both identities share a file (the same launcher, binary or alias).
    pub fn same_as(&self, other: &Self) -> bool {
        self.files().any(|f| other.contains(f))
    }
}

/// `a` and `b` are the same Codex: a launcher and the program it execs, a
/// directory and its `codex`, a desktop app's `codex.exe` and its WSL twin, or
/// the same file under two names (symlinks, hard links).
pub fn same_codex(a: &Path, b: &Path) -> bool {
    a == b || CodexIdentity::of(a).same_as(&CodexIdentity::of(b))
}

/// The program `path` runs ([`CodexIdentity::binary`]).
pub fn resolve_binary(path: &Path) -> PathBuf {
    CodexIdentity::of(path).binary
}

/// Two paths name the same file: the same `(device, inode)` on Unix when both
/// exist, else the same canonical path (a missing file: its lexically
/// normalized path; case-insensitive on Windows).
pub fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    if let (Ok(x), Ok(y)) = (fs::metadata(a), fs::metadata(b)) {
        use std::os::unix::fs::MetadataExt;
        return (x.dev(), x.ino()) == (y.dev(), y.ino());
    }
    path_key(a) == path_key(b)
}

fn path_key(path: &Path) -> String {
    let path = superba_core::paths::canonical(path).unwrap_or_else(|_| lexical(path));
    let text = path.to_string_lossy();
    if cfg!(windows) {
        // canonicalize returns verbatim `\\?\C:\...` paths; a missing file has none.
        text.strip_prefix(r"\\?\")
            .unwrap_or(&text)
            .replace('/', "\\")
            .to_lowercase()
    } else {
        text.into_owned()
    }
}

/// `path` with `.` dropped and `..` applied to the preceding name.
pub fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// A file named like a Codex program (`codex`, `codex.exe`,
/// `codex-x86_64-unknown-linux-musl`), not a JavaScript launcher.
fn codex_named(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    name.starts_with("codex") && ![".js", ".cjs", ".mjs"].iter().any(|x| name.ends_with(x))
}

/// The program the app runs for a `CODEX_CLI_PATH` directory: `codex.exe` before
/// `codex` on Windows, the reverse elsewhere; the first name when neither exists.
fn dir_program(dir: &Path) -> PathBuf {
    let names = if cfg!(windows) {
        ["codex.exe", "codex"]
    } else {
        ["codex", "codex.exe"]
    };
    names
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
        .unwrap_or_else(|| dir.join(names[0]))
}

/// The other half of a desktop app's `codex.exe` / `codex` pair (Windows ships
/// the Linux build next to the Windows one for WSL).
fn wsl_twin(binary: &Path) -> Option<PathBuf> {
    let dir = binary.parent()?;
    if !dir.join("app.asar").is_file() {
        return None;
    }
    let name = binary.file_name()?.to_str()?.to_ascii_lowercase();
    let twin = match name.as_str() {
        "codex.exe" => dir.join("codex"),
        "codex" => dir.join("codex.exe"),
        _ => return None,
    };
    twin.is_file().then_some(twin)
}

/// The existing program a small POSIX shell launcher at `path` execs ([module
/// docs](self)); for an unreadable launcher next to a macOS `CodexCLI.app`, that
/// app's binary ([`MAC_LAUNCHER_TARGET`]). `None` for anything else.
pub fn launcher_target(path: &Path) -> Option<PathBuf> {
    let text = read_launcher(path)?;
    // The script's own directory, symlinks resolved first (what launchers that
    // follow `$0` links compute), then as named.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(canonical) = superba_core::paths::canonical(path)
        && let Some(dir) = canonical.parent()
    {
        dirs.push(dir.to_owned());
    }
    if let Some(dir) = path.parent().filter(|d| !dirs.iter().any(|x| x == d)) {
        dirs.push(dir.to_owned());
    }
    let in_dirs = |rel: &Path| {
        dirs.iter()
            .map(|d| lexical(&d.join(rel)))
            .find(|p| p.is_file())
    };
    let parsed = match parse_launcher(&text) {
        Some(ExecTarget::Absolute(p)) => p.is_file().then_some(p),
        Some(ExecTarget::ScriptDir(rel)) => in_dirs(&rel),
        None => None,
    };
    parsed.or_else(|| in_dirs(Path::new(MAC_LAUNCHER_TARGET)))
}

fn read_launcher(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > LAUNCHER_MAX_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if !bytes.starts_with(b"#!") || bytes.contains(&0) {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    posix_shell(text.lines().next()?).then_some(text)
}

/// The `#!` line names a POSIX-style shell (directly or through `env`).
fn posix_shell(shebang: &str) -> bool {
    let mut words = shebang.trim_start_matches("#!").split_whitespace();
    let mut program = words.next().unwrap_or_default();
    if program.rsplit('/').next() == Some("env") {
        program = words.find(|w| !w.starts_with('-')).unwrap_or_default();
    }
    matches!(
        program.rsplit('/').next(),
        Some("sh" | "bash" | "dash" | "zsh" | "ksh" | "mksh" | "ash")
    )
}

/// What a shell word stands for while reading a launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    /// The script's own path (`$0`, `$(readlink -f "$0")`).
    ScriptFile,
    /// A path.
    Path(ExecTarget),
}

/// One shell word: its text with quotes removed (command substitutions and
/// `${...}` kept verbatim), whether it is an operator (`;`, `&&`, `|` ...), and
/// where an unquoted assignment `=` sits.
#[derive(Clone, Debug, Default)]
struct Word {
    text: String,
    operator: bool,
    assign: Option<usize>,
}

impl Word {
    fn is(&self, keyword: &str) -> bool {
        !self.operator && self.assign.is_none() && self.text == keyword
    }
}

fn is_name(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Split one line into shell words (comments dropped). Enough for launchers:
/// quotes, backslashes, `$(...)` / `` `...` `` / `${...}` kept whole, operators.
fn shell_words(line: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut word = Word::default();
    let mut started = false;
    let mut quoted = false;
    let mut in_double = false;
    let mut chars = line.chars().peekable();
    let finish = |words: &mut Vec<Word>, word: &mut Word, started: &mut bool, quoted: &mut bool| {
        if *started {
            words.push(std::mem::take(word));
        }
        *started = false;
        *quoted = false;
    };
    while let Some(c) = chars.next() {
        match c {
            '$' if matches!(chars.peek(), Some('(' | '{')) => {
                let (open, close) = if chars.peek() == Some(&'(') {
                    ('(', ')')
                } else {
                    ('{', '}')
                };
                word.text.push('$');
                let mut depth = 0usize;
                for c in chars.by_ref() {
                    word.text.push(c);
                    if c == open {
                        depth += 1;
                    } else if c == close {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                started = true;
            }
            '`' => {
                word.text.push('`');
                for c in chars.by_ref() {
                    word.text.push(c);
                    if c == '`' {
                        break;
                    }
                }
                started = true;
            }
            '\'' if !in_double => {
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    word.text.push(c);
                }
                started = true;
                quoted = true;
            }
            '"' => {
                in_double = !in_double;
                started = true;
                quoted = true;
            }
            '\\' => {
                if let Some(n) = chars.next() {
                    word.text.push(n);
                }
                started = true;
            }
            '#' if !in_double && !started => break,
            ';' | '&' | '|' if !in_double => {
                finish(&mut words, &mut word, &mut started, &mut quoted);
                let mut op = c.to_string();
                while let Some(&n) = chars.peek().filter(|n| matches!(n, '&' | '|' | ';')) {
                    op.push(n);
                    chars.next();
                }
                words.push(Word {
                    text: op,
                    operator: true,
                    assign: None,
                });
            }
            c if c.is_whitespace() && !in_double => {
                finish(&mut words, &mut word, &mut started, &mut quoted);
            }
            c => {
                if c == '=' && !quoted && word.assign.is_none() && is_name(&word.text) {
                    word.assign = Some(word.text.len());
                }
                word.text.push(c);
                started = true;
            }
        }
    }
    finish(&mut words, &mut word, &mut started, &mut quoted);
    words
}

/// The target of the last `exec <program>` of a POSIX shell launcher that
/// resolves to an absolute path or one relative to the script's directory.
pub fn parse_launcher(text: &str) -> Option<ExecTarget> {
    let mut lines = text.lines();
    if !posix_shell(lines.next()?) {
        return None;
    }
    let joined = lines.collect::<Vec<_>>().join("\n").replace("\\\n", "");
    let mut vars: HashMap<String, Value> = HashMap::new();
    let mut target = None;
    for line in joined.lines() {
        let words = shell_words(line);
        for command in words.split(|w| w.operator) {
            let mut rest = command;
            // Compound-command keywords before a simple command.
            while let Some((first, tail)) = rest.split_first()
                && ["then", "do", "else", "{", "!", "("]
                    .iter()
                    .any(|k| first.is(k))
            {
                rest = tail;
            }
            if let Some((first, tail)) = rest.split_first()
                && ["export", "readonly", "local", "declare", "typeset"]
                    .iter()
                    .any(|k| first.is(k))
            {
                rest = tail;
            }
            while let Some((first, tail)) = rest.split_first()
                && let Some(eq) = first.assign
            {
                let (name, value) = (&first.text[..eq], &first.text[eq + 1..]);
                // A value that cannot be followed keeps the last one that could:
                // launchers reassign only to resolve links of the same file.
                if let Some(v) = resolve(value, &vars) {
                    vars.insert(name.to_owned(), v);
                }
                rest = tail;
            }
            if let Some((first, args)) = rest.split_first()
                && first.is("exec")
                && let Some(Value::Path(t)) = exec_program(args).and_then(|p| resolve(p, &vars))
            {
                target = Some(t);
            }
        }
    }
    target
}

/// The program word of `exec`'s arguments (options and `env NAME=value` skipped).
fn exec_program(args: &[Word]) -> Option<&str> {
    let mut i = 0;
    while let Some(w) = args.get(i) {
        match w.text.as_str() {
            "-a" => i += 2,
            "env" | "--" => i += 1,
            t if t.starts_with('-') || w.assign.is_some() => i += 1,
            t => return Some(t),
        }
    }
    None
}

/// The value of a word: a path (absolute or under the script's directory) or the
/// script itself; `None` when it depends on anything else.
fn resolve(text: &str, vars: &HashMap<String, Value>) -> Option<Value> {
    if text.starts_with('/') && !text.contains(['$', '`']) {
        return Some(Value::Path(ExecTarget::Absolute(lexical(Path::new(text)))));
    }
    let (base, rest) = head(text, vars)?;
    if rest.contains(['$', '`']) {
        return None;
    }
    let rest = match rest {
        "" => "",
        r => r.strip_prefix('/')?,
    };
    match base {
        Value::ScriptFile => rest.is_empty().then_some(Value::ScriptFile),
        Value::Path(ExecTarget::Absolute(p)) => {
            Some(Value::Path(ExecTarget::Absolute(lexical(&p.join(rest)))))
        }
        Value::Path(ExecTarget::ScriptDir(p)) => {
            Some(Value::Path(ExecTarget::ScriptDir(lexical(&p.join(rest)))))
        }
    }
}

/// The leading expansion of a word and the text after it.
fn head<'a>(text: &'a str, vars: &HashMap<String, Value>) -> Option<(Value, &'a str)> {
    let bytes = text.as_bytes();
    if let Some(inner) = text.strip_prefix("$(") {
        let end = matching_paren(inner)?;
        return Some((substitution(&inner[..end], vars)?, &inner[end + 1..]));
    }
    if let Some(inner) = text.strip_prefix('`') {
        let end = inner.find('`')?;
        return Some((substitution(&inner[..end], vars)?, &inner[end + 1..]));
    }
    if let Some(inner) = text.strip_prefix("${") {
        let end = inner.find('}')?;
        let expr = &inner[..end];
        let value = match expr {
            "0%/*" => Value::Path(ExecTarget::ScriptDir(PathBuf::new())),
            "0" => Value::ScriptFile,
            e if e.starts_with("BASH_SOURCE") => Value::ScriptFile,
            e => vars.get(e)?.clone(),
        };
        return Some((value, &inner[end + 1..]));
    }
    if bytes.first() == Some(&b'$') {
        let name_len = text[1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(text.len() - 1);
        let name = &text[1..1 + name_len];
        let value = match name {
            "0" => Value::ScriptFile,
            "BASH_SOURCE" => Value::ScriptFile,
            n if is_name(n) => vars.get(n)?.clone(),
            _ => return None,
        };
        return Some((value, &text[1 + name_len..]));
    }
    None
}

/// Index of the `)` closing a `$(` whose content starts `inner`.
fn matching_paren(inner: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The value of a command substitution launchers use to find themselves:
/// `dirname X`, `cd X && pwd`, `readlink -f X` / `realpath X`.
fn substitution(content: &str, vars: &HashMap<String, Value>) -> Option<Value> {
    let words = shell_words(content);
    let mut command = words.split(|w| w.operator).next()?;
    while let Some((first, tail)) = command.split_first()
        && first.assign.is_some()
    {
        command = tail;
    }
    let (program, args) = command.split_first()?;
    let arg = || {
        args.iter()
            .map(|w| w.text.as_str())
            .find(|t| !t.starts_with('-') && !t.contains(['>', '<']))
    };
    match program.text.as_str() {
        "dirname" => match resolve(arg()?, vars)? {
            Value::ScriptFile => Some(Value::Path(ExecTarget::ScriptDir(PathBuf::new()))),
            Value::Path(ExecTarget::Absolute(p)) => {
                Some(Value::Path(ExecTarget::Absolute(p.parent()?.to_owned())))
            }
            Value::Path(ExecTarget::ScriptDir(p)) => {
                Some(Value::Path(ExecTarget::ScriptDir(lexical(&p.join("..")))))
            }
        },
        // `cd X && pwd` prints X: only when `pwd` follows.
        "cd" if words.iter().any(|w| w.is("pwd")) => resolve(arg()?, vars),
        "readlink" | "greadlink" | "realpath" => resolve(arg()?, vars),
        _ => None,
    }
}

/// A path short enough for a sentence: inside `root` (an app install) as
/// `<root name>/<relative>`, else under `home` as `~/<relative>`, keeping only
/// the last three names of a longer path (`ChatGPT.app/…/codex-cli/bin/codex`).
/// Show the full path next to it (hover, copy).
pub fn short_path(path: &Path, root: Option<&Path>, home: Option<&Path>) -> String {
    let canonical = superba_core::paths::canonical(path).unwrap_or_else(|_| path.to_path_buf());
    let under = |base: &Path| -> Option<Vec<String>> {
        let base_canonical =
            superba_core::paths::canonical(base).unwrap_or_else(|_| base.to_path_buf());
        [path, canonical.as_path()]
            .iter()
            .find_map(|p| {
                p.strip_prefix(base)
                    .or_else(|_| p.strip_prefix(&base_canonical))
                    .ok()
            })
            .map(|rel| {
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
    };
    let (prefix, names) = if let Some(root) = root
        && let Some(name) = root.file_name()
        && let Some(names) = under(root)
    {
        (Some(name.to_string_lossy().into_owned()), names)
    } else if let Some(home) = home
        && let Some(names) = under(home)
    {
        (Some("~".to_owned()), names)
    } else {
        let mut prefix = String::new();
        let mut names = Vec::new();
        for c in path.components() {
            match c {
                Component::Normal(n) => names.push(n.to_string_lossy().into_owned()),
                c if names.is_empty() => prefix.push_str(&c.as_os_str().to_string_lossy()),
                c => names.push(c.as_os_str().to_string_lossy().into_owned()),
            }
        }
        // `/` and `C:\` end in the separator the join adds again; a relative
        // path has no prefix.
        let prefix = (!prefix.is_empty()).then(|| prefix.trim_end_matches(['/', '\\']).to_owned());
        (prefix, names)
    };
    let mut parts: Vec<String> = prefix.into_iter().collect();
    if names.len() > 4 {
        parts.push("\u{2026}".to_owned());
        parts.extend(names[names.len() - 3..].iter().cloned());
    } else {
        parts.extend(names);
    }
    parts.join(MAIN_SEPARATOR_STR)
}
