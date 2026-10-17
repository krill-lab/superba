#![allow(missing_docs)]
//! Which program a Codex path runs (user bug 18): the macOS `codex-cli/bin/codex`
//! launcher and its `CodexCLI.app` binary are one bundled Codex; Windows'
//! `codex.exe` and its WSL twin; directories; links. Layouts are simulated in
//! temporary directories, so every platform runs these.

use std::fs;
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};

use pretty_assertions::assert_eq;
use superba_appctl::codexid::{
    CodexIdentity, ExecTarget, launcher_target, lexical, parse_launcher, resolve_binary,
    same_codex, same_file, short_path,
};
use tempfile::tempdir;

fn write(path: &Path, text: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    path.to_owned()
}

/// The launcher shipped in `ChatGPT.app/Contents/Resources/codex-cli/bin/codex`
/// (about 330 bytes: a comment, `set`, the script directory, `exec`).
const MAC_LAUNCHER: &str = "#!/bin/sh\n\
# Runs the Codex CLI bundled with the ChatGPT app. Generated; do not edit.\n\
set -eu\n\
SCRIPT_DIR=\"$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd -P)\"\n\
exec \"$SCRIPT_DIR/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"\n";

const MAC_TARGET: &str = "../CodexCLI.app/Contents/MacOS/codex";

/// `<root>/ChatGPT.app` with the launcher and the real (fake) binary.
fn mac_app(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let app = root.join("ChatGPT.app");
    let cli = app.join("Contents/Resources/codex-cli");
    let launcher = write(&cli.join("bin/codex"), MAC_LAUNCHER);
    let binary = write(
        &cli.join("CodexCLI.app/Contents/MacOS/codex"),
        "native codex 0.162.0 (fake)",
    );
    (app, launcher, binary)
}

fn script_dir(rel: &str) -> Option<ExecTarget> {
    Some(ExecTarget::ScriptDir(PathBuf::from(rel)))
}

#[test]
fn launchers_resolve_their_exec_target_relative_to_the_script() {
    assert_eq!(parse_launcher(MAC_LAUNCHER), script_dir(MAC_TARGET));
    for exec in [
        "exec \"$(dirname \"$0\")/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"",
        "exec \"${0%/*}/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"",
        "exec `dirname $0`/../CodexCLI.app/Contents/MacOS/codex \"$@\"",
        "DIR=$(dirname \"$0\"); exec \"${DIR}\"/../CodexCLI.app/Contents/MacOS/codex \"$@\"",
        "BIN=\"$(cd \"$(dirname \"$0\")/..\" && pwd)/CodexCLI.app/Contents/MacOS/codex\"\nexec \"$BIN\" \"$@\"",
        "export D=\"$(dirname \"$0\")\"\nexec -a codex \"$D/../CodexCLI.app/Contents/MacOS/codex\" \"$@\" 2>&1",
        "D=\"$(dirname \"$0\")\"\nexec env CODEX_X=1 \"$D/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"",
        "D=\"$(dirname \"$0\")\"\nif [ -x \"$D/x\" ]; then exec \"$D/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"; fi",
    ] {
        assert_eq!(
            parse_launcher(&format!("#!/bin/sh\n{exec}\n")),
            script_dir(MAC_TARGET),
            "{exec}"
        );
    }
    // A bash launcher that follows its own symlinks first.
    let bash = "#!/usr/bin/env bash\n\
SOURCE=\"${BASH_SOURCE[0]}\"\n\
while [ -h \"$SOURCE\" ]; do\n\
  DIR=\"$(cd -P \"$(dirname \"$SOURCE\")\" >/dev/null 2>&1 && pwd)\"\n\
  SOURCE=\"$(readlink \"$SOURCE\")\"\n\
  [[ $SOURCE != /* ]] && SOURCE=\"$DIR/$SOURCE\"\n\
done\n\
DIR=\"$(cd -P \"$(dirname \"$SOURCE\")\" >/dev/null 2>&1 && pwd)\"\n\
exec \"$DIR/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"\n";
    assert_eq!(parse_launcher(bash), script_dir(MAC_TARGET));
    // Absolute targets; the last exec that resolves wins; comments are not code.
    assert_eq!(
        parse_launcher("#!/bin/sh\n# exec /not/this\nexec /opt/codex/bin/codex \"$@\"\n"),
        Some(ExecTarget::Absolute(PathBuf::from("/opt/codex/bin/codex")))
    );
    assert_eq!(
        parse_launcher(
            "#!/bin/sh\n[ -x /a/codex ] && exec /a/codex \"$@\"\nexec \"$(dirname \"$0\")/codex.real\" \"$@\"\n"
        ),
        script_dir("codex.real")
    );
}

#[test]
fn anything_else_is_not_followed() {
    for text in [
        // Not a shell script.
        "#!/usr/bin/env node\nrequire('child_process').exec('/x/codex')\n",
        "\u{7f}ELF binary",
        // Relative to the working directory or to unknown variables.
        "#!/bin/sh\nexec ./codex \"$@\"\n",
        "#!/bin/sh\nexec codex \"$@\"\n",
        "#!/bin/sh\nexec \"$CODEX_HOME/bin/codex\" \"$@\"\n",
        "#!/bin/sh\nexec \"$(dirname \"$0\")/$NAME\" \"$@\"\n",
        "#!/bin/sh\nexec \"$(basename \"$0\")\" \"$@\"\n",
        // No exec at all, or only a redirection.
        "#!/bin/sh\necho 'codex-cli 0.162.0'\n",
        "#!/bin/sh\nexec 2>/dev/null\n",
    ] {
        assert_eq!(parse_launcher(text), None, "{text}");
    }
}

#[test]
fn the_macos_launcher_and_its_codexcli_binary_are_one_codex() {
    let temp = tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (_, launcher, binary) = mac_app(&root);
    let id = CodexIdentity::of(&launcher);
    assert_eq!(id.launchers, vec![launcher.clone()]);
    assert_eq!(id.binary, binary);
    assert!(id.aliases.is_empty());
    assert!(id.contains(&binary));
    assert!(id.contains(&launcher));
    assert!(same_codex(&launcher, &binary));
    assert!(same_codex(&binary, &launcher));
    assert_eq!(resolve_binary(&launcher), binary);
    assert_eq!(launcher_target(&launcher), Some(binary.clone()));
    // The binary is not a launcher: it stands for itself only.
    assert_eq!(CodexIdentity::of(&binary).binary, binary);
    assert!(CodexIdentity::of(&binary).launchers.is_empty());

    // A genuinely different Codex is never the same.
    let other = write(
        &root.join("store/0.163.0/bin/codex"),
        "native codex 0.163.0",
    );
    assert!(!same_codex(&launcher, &other));
    assert!(!same_codex(&binary, &other));
    // Same content, different file: still another binary.
    let copy = write(&root.join("copy/codex"), "native codex 0.162.0 (fake)");
    assert!(!same_codex(&binary, &copy));
}

#[test]
fn an_unreadable_launcher_falls_back_to_the_known_codexcli_path() {
    let temp = tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (_, launcher, binary) = mac_app(&root);
    write(
        &launcher,
        "#!/bin/sh\neval \"$(/usr/libexec/path_helper -s)\"\nexec \"$CODEX_REAL\" \"$@\"\n",
    );
    assert_eq!(resolve_binary(&launcher), binary);
    assert!(same_codex(&launcher, &binary));
    // Without the CodexCLI.app binary the launcher is its own program.
    fs::remove_file(&binary).unwrap();
    assert_eq!(resolve_binary(&launcher), launcher);
    assert!(!same_codex(&launcher, &binary));
}

#[test]
fn only_codex_programs_continue_the_chain() {
    let temp = tempdir().unwrap();
    // Windows canonicalises to a verbatim `\\?\C:\..` path. Inside a shell script
    // that is mangled by backslash escapes and, being verbatim, cannot take `/`
    // either; so the whole layout uses the plain `C:\..` form, and the script
    // names its target with forward slashes, as launchers do.
    let plain = |p: PathBuf| match p.to_str().and_then(|t| t.strip_prefix(r"\\?\")) {
        Some(t) => PathBuf::from(t),
        None => p,
    };
    let root = plain(fs::canonicalize(temp.path()).unwrap());
    let script_path = |p: &Path| p.display().to_string().replace('\\', "/");
    // An npm-style wrapper execs node: the wrapper is the program.
    let node = write(&root.join("bin/node"), "native node (fake)");
    let wrapper = write(
        &root.join("bin/codex"),
        "#!/bin/sh\nexec \"$(dirname \"$0\")/node\" \"$(dirname \"$0\")/codex.js\" \"$@\"\n",
    );
    assert_eq!(launcher_target(&wrapper), Some(node));
    assert_eq!(resolve_binary(&wrapper), wrapper);
    // A launcher of a launcher is followed to the end.
    let (_, launcher, binary) = mac_app(&root);
    let outer = write(
        &root.join("tools/codex"),
        &format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", script_path(&launcher)),
    );
    let id = CodexIdentity::of(&outer);
    assert_eq!(id.launchers, vec![outer.clone(), launcher]);
    assert_eq!(id.binary, binary);
    // A launcher that execs itself stops.
    let looping = write(
        &root.join("loop/codex"),
        "#!/bin/sh\nexec \"$(dirname \"$0\")/codex\" \"$@\"\n",
    );
    assert_eq!(resolve_binary(&looping), looping);
}

#[test]
fn windows_codex_exe_and_its_wsl_twin_are_one_codex() {
    let temp = tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let resources = root.join("app/resources");
    write(&resources.join("app.asar"), "archive");
    let exe = write(&resources.join("codex.exe"), "MZ windows codex");
    let linux = write(&resources.join("codex"), "ELF linux codex for WSL");
    let id = CodexIdentity::of(&exe);
    assert_eq!(id.binary, exe);
    assert_eq!(id.aliases, vec![linux.clone()]);
    assert!(same_codex(&exe, &linux));
    assert_eq!(CodexIdentity::of(&linux).aliases, vec![exe.clone()]);
    // CODEX_CLI_PATH may name the directory: the app picks its codex there.
    assert!(same_codex(&resources, &exe));
    assert!(same_codex(&resources, &linux));
    // Outside an app's resources two such files are unrelated.
    let loose = root.join("loose");
    let a = write(&loose.join("codex.exe"), "MZ");
    let b = write(&loose.join("codex"), "ELF");
    assert!(!same_codex(&a, &b));
}

#[test]
fn linux_resources_codex_and_a_pinned_codex() {
    let temp = tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    write(&root.join("chatgpt/resources/app.asar"), "archive");
    let bundled = write(&root.join("chatgpt/resources/codex"), "ELF bundled");
    let pinned = write(
        &root.join("superba/codex/0.162.0-x86_64-unknown-linux-musl/bin/codex"),
        "ELF pinned",
    );
    let id = CodexIdentity::of(&bundled);
    assert_eq!(
        (id.binary.clone(), id.aliases.clone()),
        (bundled.clone(), Vec::new())
    );
    assert!(same_codex(&pinned, &pinned));
    assert!(same_codex(
        &pinned.parent().unwrap().join("../bin/codex"),
        &pinned
    ));
    assert!(!same_codex(&pinned, &bundled));
    // A directory pin names its codex.
    assert!(same_codex(pinned.parent().unwrap(), &pinned));
    assert!(!same_codex(pinned.parent().unwrap(), &bundled));
}

#[cfg(unix)]
#[test]
fn symlinked_applications_and_linked_launchers_are_the_same_codex() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (_, launcher, binary) = mac_app(&root.join("Volumes/Apps"));
    // `/Applications` is a link to another volume.
    symlink(root.join("Volumes/Apps"), root.join("Applications")).unwrap();
    let linked_app = root.join("Applications/ChatGPT.app");
    let linked_launcher = linked_app.join("Contents/Resources/codex-cli/bin/codex");
    let linked_binary =
        linked_app.join("Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex");
    assert!(same_codex(&linked_launcher, &binary));
    assert!(same_codex(&launcher, &linked_binary));
    assert!(same_codex(&linked_launcher, &linked_binary));
    // A link to the launcher elsewhere (`~/bin/codex`): the script's real
    // directory decides where its target is.
    symlink(&launcher, root.join("codex-link")).unwrap();
    assert_eq!(resolve_binary(&root.join("codex-link")), binary);
    assert!(same_codex(&root.join("codex-link"), &linked_binary));
    // Hard links are the same file.
    let hard = root.join("hard-codex");
    fs::hard_link(&binary, &hard).unwrap();
    assert!(same_file(&hard, &linked_binary));
}

#[test]
fn missing_files_compare_by_normalized_path() {
    let temp = tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let a = root.join("gone/bin/../bin/./codex");
    let b = root.join("gone/bin/codex");
    assert!(same_file(&a, &b));
    assert!(same_codex(&a, &b));
    assert!(!same_file(&a, &root.join("gone/other/codex")));
    assert_eq!(lexical(&a), b);
    assert_eq!(lexical(Path::new("../x/./y/../z")), PathBuf::from("../x/z"));
}

#[test]
fn short_paths_name_the_app_or_home_and_the_last_parts() {
    let temp = tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (app, launcher, binary) = mac_app(&root);
    let join = |parts: &[&str]| parts.join(MAIN_SEPARATOR_STR);
    assert_eq!(
        short_path(&launcher, Some(&app), None),
        join(&["ChatGPT.app", "\u{2026}", "codex-cli", "bin", "codex"])
    );
    assert_eq!(
        short_path(&binary, Some(&app), Some(&root)),
        join(&["ChatGPT.app", "\u{2026}", "Contents", "MacOS", "codex"])
    );
    let home = root.join("home/u");
    assert_eq!(
        short_path(&home.join(".local/bin/codex"), Some(&app), Some(&home)),
        join(&["~", ".local", "bin", "codex"])
    );
    assert_eq!(
        short_path(
            &home.join(".local/share/superba/codex/0.162.0-x86_64/bin/codex"),
            None,
            Some(&home)
        ),
        join(&["~", "\u{2026}", "0.162.0-x86_64", "bin", "codex"])
    );
    assert_eq!(short_path(Path::new("codex"), None, None), "codex");
    #[cfg(unix)]
    {
        assert_eq!(
            short_path(Path::new("/usr/local/bin/codex"), None, None),
            "/usr/local/bin/codex"
        );
        assert_eq!(
            short_path(Path::new("/opt/tools/codex/v1/bin/codex"), None, None),
            "/\u{2026}/v1/bin/codex"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn versions_are_read_from_the_launcher_target_without_running_the_launcher() {
    use std::os::unix::fs::PermissionsExt;
    use superba_appctl::install::codex_version;
    use superba_core::CodexVersion;

    let temp = tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (_, launcher, binary) = mac_app(&root);
    let log = root.join("launcher-ran");
    write(
        &launcher,
        &format!(
            "#!/bin/sh\necho ran > '{}'\nexec \"$(dirname \"$0\")/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"\n",
            log.display()
        ),
    );
    write(&binary, "#!/bin/sh\necho 'codex-cli 0.162.0-alpha.17.2'\n");
    for p in [&launcher, &binary] {
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(
        codex_version(&launcher).await,
        Some(CodexVersion::parse("0.162.0-alpha.17.2").unwrap())
    );
    assert!(!log.exists(), "the launcher itself was run");
    // A target that does not answer: the launcher is asked instead.
    write(&binary, "#!/bin/sh\nexit 3\n");
    write(
        &launcher,
        &format!(
            "#!/bin/sh\necho ran > '{}'\n[ -n \"$NEVER\" ] && exec \"$(dirname \"$0\")/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"\necho 'codex-cli 0.150.1'\n",
            log.display()
        ),
    );
    assert_eq!(
        codex_version(&launcher).await,
        Some(CodexVersion::parse("0.150.1").unwrap())
    );
    assert!(log.exists());
}
