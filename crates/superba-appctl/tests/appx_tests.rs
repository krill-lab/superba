//! MSIX helpers that run on every platform: AUMIDs, manifest applications,
//! package paths, the activation command line and plain (non-verbatim) paths.

use std::path::{Path, PathBuf};

use superba_appctl::appx::{
    aumid, family_of_full_name, in_windows_apps, is_family_name, main_application, package_file,
    pick_aumid, plain_path, quote_windows_arg, relocated_codex_candidates, version_of_full_name,
    windows_command_line,
};

const FAMILY: &str = "OpenAI.Codex_2p2nqsd0c76g0";

#[test]
fn family_names_and_full_names() {
    assert!(is_family_name(FAMILY));
    assert!(is_family_name("OpenAI.ChatGPT-Desktop_2p2nqsd0c76g0"));
    for id in [
        "path:C:\\x",
        "com.openai.codex",
        "OpenAI.Codex_2P2NQSD0C76G0",
        "OpenAI.Codex_short",
        "_2p2nqsd0c76g0",
        "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0",
    ] {
        assert!(!is_family_name(id), "{id}");
    }
    let full = "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0";
    assert_eq!(family_of_full_name(full).as_deref(), Some(FAMILY));
    assert_eq!(version_of_full_name(full), Some("26.1013.4894.0"));
    assert!(family_of_full_name("garbage").is_none());
}

#[test]
fn the_main_application_is_activated() {
    assert_eq!(aumid(FAMILY, "App"), "OpenAI.Codex_2p2nqsd0c76g0!App");
    // GetPackageApplicationIds reports full AUMIDs in manifest order; the
    // command runner helper must never be picked over the desktop app.
    let ids = [
        format!("{FAMILY}!CodexCoreCommandRunner"),
        format!("{FAMILY}!App"),
    ];
    assert_eq!(
        pick_aumid(FAMILY, &ids).as_deref(),
        Some("OpenAI.Codex_2p2nqsd0c76g0!App")
    );
    // Bare ids (from a manifest) get the family; without `App` the first wins.
    assert_eq!(
        pick_aumid(FAMILY, &["Desktop".into(), " ".into()]).as_deref(),
        Some("OpenAI.Codex_2p2nqsd0c76g0!Desktop")
    );
    assert!(pick_aumid(FAMILY, &[]).is_none());

    let apps = vec![
        (
            "CodexCoreCommandRunner".to_owned(),
            Some("app/resources/codex-command-runner.exe".to_owned()),
        ),
        ("Main".to_owned(), Some("app\\ChatGPT.exe".to_owned())),
    ];
    assert_eq!(main_application(&apps).unwrap().0, "Main");
    let apps = vec![("Only".to_owned(), None)];
    assert_eq!(main_application(&apps).unwrap().0, "Only");
    assert!(main_application(&[]).is_none());
}

#[test]
fn manifest_paths_stay_inside_the_package() {
    let root = Path::new("pkg");
    assert_eq!(
        package_file(root, "app/ChatGPT.exe"),
        Some(root.join("app").join("ChatGPT.exe"))
    );
    assert_eq!(
        package_file(root, "app\\resources\\codex.exe"),
        Some(root.join("app").join("resources").join("codex.exe"))
    );
    for bad in ["../x.exe", "C:\\x.exe", "", "./"] {
        assert!(package_file(root, bad).is_none(), "{bad}");
    }
}

/// `CommandLineToArgvW` / MSVC argument splitting, to check the quoting.
fn split_windows(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut started = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            let mut n = 0;
            while i < chars.len() && chars[i] == '\\' {
                n += 1;
                i += 1;
            }
            if i < chars.len() && chars[i] == '"' {
                cur.extend(std::iter::repeat_n('\\', n / 2));
                if n % 2 == 1 {
                    cur.push('"');
                    i += 1;
                }
            } else {
                cur.extend(std::iter::repeat_n('\\', n));
            }
            started = true;
            continue;
        }
        match c {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            ' ' | '\t' if !in_quotes => {
                if started {
                    args.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
        i += 1;
    }
    if started {
        args.push(cur);
    }
    args
}

#[test]
fn the_activation_command_line_round_trips() {
    assert_eq!(
        quote_windows_arg("--remote-debugging-port=9229"),
        "--remote-debugging-port=9229"
    );
    assert_eq!(quote_windows_arg(""), "\"\"");
    let args: Vec<String> = [
        "--remote-debugging-port=50123",
        "--remote-allow-origins=http://127.0.0.1:50123",
        "C:\\Program Files\\x\\",
        "say \"hi\"",
        "a\\\\\"b",
        "",
        "tab\there",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    assert_eq!(split_windows(&windows_command_line(&args)), args);
}

#[test]
fn verbatim_paths_become_plain() {
    assert_eq!(
        plain_path(Path::new(
            r"\\?\C:\Program Files\WindowsApps\OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0"
        )),
        PathBuf::from(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0"
        )
    );
    assert_eq!(
        plain_path(Path::new(r"\\?\UNC\Mac\Home\superba")),
        PathBuf::from(r"\\Mac\Home\superba")
    );
    // Not representable as a plain path: kept.
    for keep in [
        r"\\?\C:\dir.\x",
        r"\\?\Volume{0b1c}\x",
        r"C:\already\plain",
        "/usr/lib/chatgpt",
    ] {
        assert_eq!(plain_path(Path::new(keep)), PathBuf::from(keep), "{keep}");
    }
    let long = format!(r"\\?\C:\{}", "a".repeat(300));
    assert_eq!(plain_path(Path::new(&long)), PathBuf::from(&long));
}

#[test]
fn windows_apps_paths_and_the_relocated_codex() {
    assert!(in_windows_apps(Path::new(
        r"C:\Program Files\WindowsApps\OpenAI.Codex_1_x64__p\app\resources\codex.exe"
    )));
    assert!(in_windows_apps(Path::new("D:/windowsapps/x/codex.exe")));
    assert!(!in_windows_apps(Path::new(
        r"C:\Users\me\AppData\Local\OpenAI\Codex\bin\codex.exe"
    )));
    // The app's copies: `bin\<hash>\codex.exe` (and a plain `bin\codex.exe`),
    // newest first; other files and empty folders are not copies.
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("OpenAI").join("Codex").join("bin");
    std::fs::create_dir_all(bin.join("896a0166b774037a")).unwrap();
    std::fs::create_dir_all(bin.join("empty")).unwrap();
    std::fs::create_dir_all(bin.join("0123456789abcdef")).unwrap();
    let written = |dir: &str, secs: u64| {
        let path = bin.join(dir).join("codex.exe");
        std::fs::write(&path, dir).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    };
    written("896a0166b774037a", 1_700_000_000);
    written("0123456789abcdef", 1_800_000_000);
    std::fs::write(bin.join("notes.txt"), "x").unwrap();
    assert_eq!(
        relocated_codex_candidates(temp.path()),
        vec![
            bin.join("0123456789abcdef").join("codex.exe"),
            bin.join("896a0166b774037a").join("codex.exe"),
        ]
    );
    assert!(relocated_codex_candidates(&temp.path().join("none")).is_empty());
}
