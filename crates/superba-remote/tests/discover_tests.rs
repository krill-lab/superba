use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_core::{
    CodexHome, CodexProcessKind, EnvObservation, Error, HostSource, ProcessInfo, RemoteHost,
    SshEndpoint, TargetId,
};
use superba_remote::discover::{
    AppConnection, DISCOVERED_HOST_ID_PREFIX, GLOBAL_STATE_BAK_SUFFIX, KEY_AUTO_CONNECT,
    KEY_CONNECTIONS, KEY_PROJECTS, SshConfigHost, discover_from_files, discovered_host_id,
    endpoint_from_ssh_argv, host_from_connection, is_suggestion, load_ssh_config, merge_hosts,
    parse_codex_app_config, parse_global_state, parse_ssh_config,
};

fn connections(entries: Value) -> Vec<AppConnection> {
    parse_global_state(&serde_json::to_vec(&json!({KEY_CONNECTIONS: entries})).unwrap()).unwrap()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn endpoint(args: &[&str]) -> SshEndpoint {
    endpoint_from_ssh_argv(&strings(args)).expect("valid SSH argv")
}

fn alias_endpoint(alias: &str) -> SshEndpoint {
    SshEndpoint {
        alias: Some(alias.into()),
        ..SshEndpoint::default()
    }
}

fn host_endpoint(host: &str) -> SshEndpoint {
    SshEndpoint {
        host: Some(host.into()),
        ..SshEndpoint::default()
    }
}

fn renderer(id: Option<&str>, label: &str, ssh: SshEndpoint) -> RemoteHost {
    RemoteHost {
        target_id: TargetId::remote("stale-id"),
        label: label.into(),
        host_id: id.map(str::to_owned),
        kind: superba_core::HostKind::Ssh,
        ssh,
        sources: vec![HostSource::Cdp],
        connected: None,
        wsl: None,
    }
}

fn link(host: &str, argv: &[&str]) -> ProcessInfo {
    ProcessInfo {
        pid: 42,
        ppid: None,
        kind: CodexProcessKind::RemoteLink { host: host.into() },
        exe: None,
        cmdline: strings(argv),
        started_at: None,
        authapi_env: EnvObservation::default(),
        codex_home: None,
        stale: false,
    }
}

fn config_host(alias: &str) -> SshConfigHost {
    SshConfigHost {
        alias: alias.into(),
        ..SshConfigHost::default()
    }
}

fn write_file(root: &Path, relative: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn global_state_current_connection_shape_is_normalised() {
    let actual = connections(json!([{
        "hostId": " id-current ", "connectionAnalyticsId": "ignored",
        "displayName": " Work box ", "source": " codex-managed ",
        "alias": " devbox ", "hostname": " alice@build.example ",
        "sshPort": 2222, "identity": " /keys/work key ", "future": [1, 2]
    }]));
    assert_eq!(
        actual,
        vec![AppConnection {
            host_id: "id-current".into(),
            display_name: Some("Work box".into()),
            source: Some("codex-managed".into()),
            alias: Some("devbox".into()),
            host: Some("build.example".into()),
            user: Some("alice".into()),
            port: Some(2222),
            identity: Some(PathBuf::from("/keys/work key")),
            auto_connect: None,
            project_paths: vec![],
        }]
    );
}

#[test]
fn global_state_legacy_and_alternate_spellings_are_accepted() {
    let actual = connections(json!([
        {"hostId":"legacy", "displayName":"Legacy", "sshAlias":" old ",
         "sshHost":" legacy-user@203.0.113.5 ", "sshPort":22, "identity":null},
        {"hostId":"alternate", "hostname":false, "sshHost":"embedded@host.example",
         "alias":" ", "sshAlias":" fallback ", "user":null, "sshUser":" explicit "},
        {"hostId":"precedence", "hostname":"first@current.example", "sshHost":"old.example",
         "alias":"current", "sshAlias":"old", "user":"new", "sshUser":"old"}
    ]));
    assert_eq!(actual[0].alias.as_deref(), Some("old"));
    assert_eq!(actual[0].host.as_deref(), Some("203.0.113.5"));
    assert_eq!(actual[0].user.as_deref(), Some("legacy-user"));
    assert_eq!(actual[1].alias.as_deref(), Some("fallback"));
    assert_eq!(actual[1].host.as_deref(), Some("host.example"));
    assert_eq!(actual[1].user.as_deref(), Some("explicit"));
    assert_eq!(actual[2].alias.as_deref(), Some("current"));
    assert_eq!(actual[2].host.as_deref(), Some("current.example"));
    assert_eq!(actual[2].user.as_deref(), Some("new"));
}

#[test]
fn global_state_splits_and_trims_users_and_hosts() {
    let actual = connections(json!([
        {"hostId":"one", "hostname":" alice @ host.example "},
        {"hostId":"two", "sshHost":"bob@2001:db8::1"},
        {"hostId":"three", "hostname":"@host", "user":" "},
        {"hostId":"four", "hostname":" user@ "},
        {"hostId":"five", "hostname":" plain.example "}
    ]));
    assert_eq!(actual[0].user.as_deref(), Some("alice"));
    assert_eq!(actual[0].host.as_deref(), Some("host.example"));
    assert_eq!(actual[1].user.as_deref(), Some("bob"));
    assert_eq!(actual[1].host.as_deref(), Some("2001:db8::1"));
    assert_eq!(actual[2].user, None);
    assert_eq!(actual[2].host.as_deref(), Some("host"));
    assert_eq!(actual[3].host, None);
    assert_eq!(actual[4].user, None);
}

#[test]
fn global_state_ports_accept_integers_or_numeric_strings_in_range() {
    for bad in [
        json!(0),
        json!(-1),
        json!(65536),
        json!(1.5),
        json!("0"),
        json!("ssh"),
        json!(true),
        json!(null),
        json!([]),
        json!({}),
        json!(u64::MAX),
    ] {
        let actual = connections(json!([{"hostId":"id", "sshPort":bad}]));
        assert_eq!(actual[0].port, None, "port {bad}");
    }
    for valid in [1, 22, 65535] {
        let actual = connections(json!([{"hostId":"id", "sshPort":valid}]));
        assert_eq!(actual[0].port, Some(valid));
    }
    // Numeric strings are tolerated because the schema is unverified (S1).
    let actual = connections(json!([{"hostId":"id", "sshPort":" 2222 "}]));
    assert_eq!(actual[0].port, Some(2222));
}

#[test]
fn global_state_skips_malformed_entries_and_tolerates_optional_types() {
    let actual = connections(json!([
        null, false, 12, "entry", [], {}, {"hostId":false}, {"hostId":" "},
        {"hostname":"missing-id"},
        {"hostId":"valid", "displayName":false, "source":[], "alias":{},
         "hostname":[], "user":22, "identity":true},
        {"hostId":"empty", "displayName":" ", "source":" ", "alias":" ",
         "hostname":" ", "user":" ", "identity":" "}
    ]));
    assert_eq!(actual.len(), 2);
    for item in actual {
        assert_eq!(item.display_name, None);
        assert_eq!(item.source, None);
        assert_eq!(item.alias, None);
        assert_eq!(item.host, None);
        assert_eq!(item.user, None);
        assert_eq!(item.identity, None);
    }
    for bad_collection in [json!(null), json!(3), json!("array"), json!({})] {
        assert!(connections(bad_collection).is_empty());
    }
}

#[test]
fn global_state_only_invalid_json_or_non_object_roots_fail() {
    for bytes in [
        b"".as_slice(),
        b"{",
        b"null",
        b"[]",
        b"42",
        b"true",
        b"\"text\"",
        b"\xff",
    ] {
        assert!(
            matches!(parse_global_state(bytes), Err(Error::Parse { what, .. })
            if what == "codex global state")
        );
    }
    assert!(parse_global_state(b"{}").unwrap().is_empty());
    assert!(
        parse_global_state(
            br#"{"globalState":{"codex-managed-remote-connections":[{"hostId":"nested"}]}}"#
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn global_state_attaches_projects_in_order_and_boolean_preferences() {
    let value = json!({
        KEY_CONNECTIONS: [{"hostId":"a"}, {"hostId":"b"}, {"hostId":"c"}, {"hostId":"d"}],
        KEY_PROJECTS: [
            {"id":"1", "hostId":"a", "label":"First", "remotePath":" /a/first "},
            {"id":"2", "hostId":"b", "label":"Other", "remotePath":"/b"},
            null, {"hostId":"a", "remotePath":false}, {"remotePath":"/unowned"},
            {"hostId":"a", "remotePath":" "},
            {"id":"3", "hostId":" a ", "label":"Last", "remotePath":"/a/last"},
            {"hostId":"a", "remotePath":"/a/first"}
        ],
        KEY_AUTO_CONNECT: {" a ":true, "b":false, "c":"true", "unknown":true}
    });
    let actual = parse_global_state(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        actual[0].project_paths,
        strings(&["/a/first", "/a/last", "/a/first"])
    );
    assert_eq!(actual[1].project_paths, strings(&["/b"]));
    assert!(actual[2].project_paths.is_empty());
    assert_eq!(
        actual.iter().map(|c| c.auto_connect).collect::<Vec<_>>(),
        vec![Some(true), Some(false), None, None]
    );
}

#[test]
fn global_state_wrong_project_and_preference_types_are_absent() {
    for wrong in [json!(null), json!("bad"), json!(42), json!(true)] {
        let value =
            json!({KEY_CONNECTIONS:[{"hostId":"a"}], KEY_PROJECTS:wrong, KEY_AUTO_CONNECT:wrong});
        let actual = parse_global_state(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(actual[0].auto_connect, None);
        assert!(actual[0].project_paths.is_empty());
    }
}

#[test]
fn discovered_ids_use_javascript_component_encoding() {
    for (alias, encoded) in [
        ("", ""),
        ("a b", "a%20b"),
        ("a/b@c", "a%2Fb%40c"),
        ("开发", "%E5%BC%80%E5%8F%91"),
        ("🦀", "%F0%9F%A6%80"),
        ("AZaz09-_.!~*'()", "AZaz09-_.!~*'()"),
        ("%+?#:\\\n", "%25%2B%3F%23%3A%5C%0A"),
        (" a ", "%20a%20"),
    ] {
        assert_eq!(
            discovered_host_id(alias),
            format!("remote-ssh-discovered:{encoded}")
        );
    }
    assert_eq!(DISCOVERED_HOST_ID_PREFIX, "remote-ssh-discovered:");
    assert_eq!(GLOBAL_STATE_BAK_SUFFIX, ".bak");
    assert_eq!(KEY_CONNECTIONS, "codex-managed-remote-connections");
    assert_eq!(KEY_PROJECTS, "remote-projects");
    assert_eq!(
        KEY_AUTO_CONNECT,
        "remote-connection-auto-connect-by-host-id"
    );
}

#[test]
fn app_config_aliases_are_trimmed_deduplicated_and_ordered() {
    let bytes = serde_json::to_vec(&json!({"version":1, "unknown":true, "remoteConnections":[
        {"sshAlias":" second ", "projects":[{"remotePath":"/work", "label":"Work"}]},
        {"sshAlias":"first"}, {"sshAlias":"second"}, {"sshAlias":"FIRST"},
        {"sshAlias":" "}, {"sshAlias":1}, null, [], {}, {"alias":"wrong-spelling"}
    ]}))
    .unwrap();
    assert_eq!(
        parse_codex_app_config(&bytes),
        strings(&["second", "first", "FIRST"])
    );
    assert_eq!(
        parse_codex_app_config(br#"{"remoteConnections":[{"sshAlias":"no-version"}]}"#),
        strings(&["no-version"])
    );
}

#[test]
fn app_config_invalid_json_and_shapes_are_empty() {
    for bytes in [
        b"".as_slice(),
        b"{",
        b"[]",
        b"null",
        b"42",
        b"{}",
        br#"{"remoteConnections":{}}"#,
        br#"{"remoteConnections":[false]}"#,
    ] {
        assert!(parse_codex_app_config(bytes).is_empty());
    }
}

#[test]
fn ssh_config_supports_multi_alias_blocks_and_first_values() {
    let (hosts, includes) = parse_ssh_config(
        r#"
        # comment
        hOsT = a b *.ignored !excluded skipped?
            HOSTNAME = "build.example" # trailing comment
            HostName ignored.example
            uSeR='build user'
            User ignored
            pOrT=2222
            Port 23
            IdentityFile "~/.ssh/key #1"
            IdentityFile ignored
        Host c
            HostName c.example
    "#,
    );
    assert!(includes.is_empty());
    let expected = SshConfigHost {
        alias: "a".into(),
        host_name: Some("build.example".into()),
        user: Some("build user".into()),
        port: Some(2222),
        identity_file: Some("~/.ssh/key #1".into()),
    };
    assert_eq!(
        hosts,
        vec![
            expected.clone(),
            SshConfigHost {
                alias: "b".into(),
                ..expected
            },
            SshConfigHost {
                alias: "c".into(),
                host_name: Some("c.example".into()),
                ..Default::default()
            }
        ]
    );
}

#[test]
fn ssh_config_patterns_match_blocks_and_unscoped_options_are_ignored() {
    let (hosts, includes) = parse_ssh_config(
        r#"
        User unscoped
        Host * ?.example !excluded
            User wildcard
        Host before
            HostName before.example
        Match host before
            User ignored
            Port 9999
            Include ignored.conf
        Match all
            IdentityFile ignored
        Host after
            User included
    "#,
    );
    assert_eq!(hosts.len(), 2);
    assert_eq!(hosts[0].alias, "before");
    assert_eq!(hosts[0].user, None);
    assert_eq!(hosts[0].port, None);
    assert_eq!(hosts[0].identity_file, None);
    assert_eq!(hosts[1].alias, "after");
    assert_eq!(hosts[1].user.as_deref(), Some("included"));
    assert!(includes.is_empty());
}

#[test]
fn ssh_config_quotes_comments_escapes_and_raw_includes() {
    let (hosts, includes) = parse_ssh_config(
        r#"
        Include first.conf "with space/*.conf" ~/extra/?.cfg # ignored
        include=last.conf
        Host "space alias" literal
            IdentityFile /keys/a\ b\#c
            HostName host.example#comment
            User "user#name"
        Host windows
            IdentityFile C:\keys\id_work
    "#,
    );
    assert_eq!(
        includes,
        strings(&[
            "first.conf",
            "with space/*.conf",
            "~/extra/?.cfg",
            "last.conf"
        ])
    );
    assert_eq!(hosts[0].alias, "space alias");
    assert_eq!(hosts[0].identity_file.as_deref(), Some("/keys/a b#c"));
    assert_eq!(hosts[0].host_name.as_deref(), Some("host.example"));
    assert_eq!(hosts[0].user.as_deref(), Some("user#name"));
    assert_eq!(hosts[2].identity_file.as_deref(), Some(r"C:\keys\id_work"));
}

#[test]
fn ssh_config_bad_values_and_unterminated_quotes_do_not_abort() {
    let (hosts, _) = parse_ssh_config(
        "Host a\nPort 0\nPort -2\nPort 65536\nPort nope\nPort 22\nUser \"unterminated\nUser valid\nIdentityFile \"\"\nIdentityFile /key\nHost b\nPort 1\n",
    );
    assert_eq!(hosts[0].port, Some(22));
    assert_eq!(hosts[0].user.as_deref(), Some("valid"));
    assert_eq!(hosts[0].identity_file.as_deref(), Some("/key"));
    assert_eq!(hosts[1].port, Some(1));
    assert!(parse_ssh_config("\n# only comments\n").0.is_empty());
}

#[test]
fn ssh_includes_resolve_relative_home_and_absolute_paths_with_sorted_globs() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "outside/absolute.conf", "Host absolute\n");
    let absolute = temp.path().join("outside/absolute.conf");
    write_file(
        temp.path(),
        ".ssh/config",
        format!(
            "Host first\nInclude snippets/*.conf\nInclude ~/extras/?.cfg\nInclude \"{}\"\nHost last\n",
            absolute.display()
        ),
    );
    write_file(
        temp.path(),
        ".ssh/snippets/20.conf",
        "Host second\nInclude root-relative.conf\n",
    );
    write_file(temp.path(), ".ssh/snippets/10.conf", "Host first-in-glob\n");
    write_file(
        temp.path(),
        ".ssh/root-relative.conf",
        "Host root-relative\n",
    );
    write_file(
        temp.path(),
        ".ssh/snippets/ignore.txt",
        "Host wrong-extension\n",
    );
    write_file(temp.path(), "extras/开.cfg", "Host unicode\n");
    write_file(temp.path(), "extras/aa.cfg", "Host two-characters\n");
    let actual = load_ssh_config(temp.path());
    assert_eq!(
        actual
            .iter()
            .map(|host| host.alias.as_str())
            .collect::<Vec<_>>(),
        vec![
            "first",
            "first-in-glob",
            "second",
            "root-relative",
            "unicode",
            "absolute",
            "last"
        ]
    );
}

#[test]
fn ssh_includes_expand_in_place_and_first_duplicate_alias_wins() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        ".ssh/config",
        "Host parent\nInclude options.conf\nUser later\nInclude hosts.conf\nHost duplicate\nUser later\n",
    );
    write_file(
        temp.path(),
        ".ssh/options.conf",
        "User from-include\nPort 2222\n",
    );
    write_file(
        temp.path(),
        ".ssh/hosts.conf",
        "Host duplicate\nUser first\n",
    );
    let actual = load_ssh_config(temp.path());
    assert_eq!(actual.len(), 2);
    assert_eq!(actual[0].user.as_deref(), Some("from-include"));
    assert_eq!(actual[0].port, Some(2222));
    assert_eq!(actual[1].user.as_deref(), Some("first"));
}

#[test]
fn ssh_includes_have_a_depth_limit_of_four_and_break_cycles() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        ".ssh/config",
        "Host root\nInclude level1\nInclude ./config\n",
    );
    for level in 1..=5 {
        write_file(
            temp.path(),
            &format!(".ssh/level{level}"),
            format!(
                "Host level{level}\nInclude level{}\nInclude config\n",
                level + 1
            ),
        );
    }
    let actual = load_ssh_config(temp.path());
    assert_eq!(
        actual
            .iter()
            .map(|host| host.alias.as_str())
            .collect::<Vec<_>>(),
        vec!["root", "level1", "level2", "level3", "level4"]
    );
}

#[test]
fn ssh_includes_read_a_file_at_most_once_even_in_different_blocks() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        ".ssh/config",
        "Host first\nInclude common\nHost second\nInclude ./common\n",
    );
    write_file(temp.path(), ".ssh/common", "User only-first\n");
    let actual = load_ssh_config(temp.path());
    assert_eq!(actual[0].user.as_deref(), Some("only-first"));
    assert_eq!(actual[1].user, None);
}

#[cfg(unix)]
#[test]
fn ssh_includes_canonicalize_symlink_cycles() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        ".ssh/config",
        "Host root\nInclude alias.conf\nHost after\n",
    );
    std::os::unix::fs::symlink("config", temp.path().join(".ssh/alias.conf")).unwrap();
    let actual = load_ssh_config(temp.path());
    assert_eq!(actual.len(), 2);
    assert_eq!(actual[1].alias, "after");
}

#[test]
fn ssh_loading_ignores_io_errors_non_utf8_and_directory_globs() {
    let temp = tempfile::tempdir().unwrap();
    assert!(load_ssh_config(temp.path()).is_empty());
    write_file(
        temp.path(),
        ".ssh/config",
        "Include missing invalid.conf snippets/*/*.conf a**z?.cfg\nHost valid\n",
    );
    write_file(temp.path(), ".ssh/invalid.conf", [0xff]);
    write_file(
        temp.path(),
        ".ssh/snippets/one/hidden.conf",
        "Host unsupported\n",
    );
    write_file(temp.path(), ".ssh/abz1.cfg", "Host matched\n");
    write_file(temp.path(), ".ssh/az.cfg", "Host too-short\n");
    let actual = load_ssh_config(temp.path());
    assert_eq!(
        actual
            .iter()
            .map(|host| host.alias.as_str())
            .collect::<Vec<_>>(),
        vec!["matched", "valid"]
    );
}

#[test]
fn argv_checks_executable_and_recognises_aliases_hosts_and_addresses() {
    for executable in [
        "ssh",
        "/usr/bin/ssh",
        "ssh.exe",
        r"C:\Windows\System32\OpenSSH\ssh.exe",
    ] {
        assert_eq!(endpoint(&[executable, "devbox"]), alias_endpoint("devbox"));
    }
    for executable in ["scp", "notssh", "ssh-wrapper", "/ssh/not-ssh", "SSH"] {
        assert_eq!(
            endpoint_from_ssh_argv(&strings(&[executable, "devbox"])),
            None
        );
    }
    for host in ["host.example", "127.0.0.1", "2001:db8::1", "[::1]"] {
        assert_eq!(endpoint(&["ssh", host]), host_endpoint(host));
    }
    assert_eq!(
        endpoint(&["ssh", "alice@host"]).user.as_deref(),
        Some("alice")
    );
    assert_eq!(
        endpoint(&["ssh", "alice@host"]).host.as_deref(),
        Some("host")
    );
}

#[test]
fn argv_skips_every_option_with_an_argument_in_separate_and_attached_forms() {
    for option in [
        "-B", "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-J", "-L", "-m", "-O", "-P", "-Q", "-R",
        "-S", "-W", "-w",
    ] {
        assert_eq!(
            endpoint(&["ssh", option, "argument", "devbox"]),
            alias_endpoint("devbox"),
            "{option}"
        );
        assert_eq!(
            endpoint(&["ssh", &format!("{option}argument"), "devbox"]),
            alias_endpoint("devbox"),
            "{option}"
        );
    }
    assert_eq!(
        endpoint(&["ssh", "-o", "BatchMode=yes", "devbox"]),
        alias_endpoint("devbox")
    );
    assert_eq!(
        endpoint(&["ssh", "-oBatchMode=yes", "devbox"]),
        alias_endpoint("devbox")
    );
}

#[test]
fn argv_parses_connection_options_in_separate_and_attached_forms() {
    let expected = SshEndpoint {
        host: Some("devbox".into()),
        port: Some(2222),
        user: Some("root".into()),
        identity: Some(PathBuf::from("/key")),
        ..Default::default()
    };
    assert_eq!(
        endpoint(&["ssh", "-i", "/key", "-p", "2222", "-l", "root", "devbox"]),
        expected
    );
    assert_eq!(
        endpoint(&["ssh", "-i/key", "-p2222", "-lroot", "devbox"]),
        expected
    );
    for args in [
        ["ssh", "-p22", "devbox"],
        ["ssh", "-i/key", "devbox"],
        ["ssh", "-lroot", "devbox"],
    ] {
        let actual = endpoint(&args);
        assert_eq!(actual.alias, None);
        assert_eq!(actual.host.as_deref(), Some("devbox"));
    }
}

#[test]
fn argv_parses_o_connection_options_case_insensitively_with_equals_or_spaces() {
    let expected = SshEndpoint {
        host: Some("host".into()),
        port: Some(2222),
        user: Some("alice".into()),
        identity: Some(PathBuf::from("/keys/work key")),
        ..Default::default()
    };
    assert_eq!(
        endpoint(&[
            "ssh",
            "-oPoRt=2222",
            "-o",
            "uSeR alice",
            "-oIdentityFile=\"/keys/work key\"",
            "host"
        ]),
        expected
    );
    assert_eq!(
        endpoint(&[
            "ssh",
            "-o",
            "Port 2222",
            "-oUser=alice",
            "-o",
            "IdentityFile '/keys/work key'",
            "host"
        ]),
        expected
    );
    assert_eq!(endpoint(&["ssh", "-oPort=22", "host"]).port, Some(22));
}

#[test]
fn argv_handles_clustered_flags_double_dash_and_stops_at_destination() {
    assert_eq!(
        endpoint(&["ssh", "-vT", "--", "devbox", "-p", "9999"]),
        alias_endpoint("devbox")
    );
    let actual = endpoint(&["ssh", "-vvTp2222", "host", "-lroot"]);
    assert_eq!(actual.port, Some(2222));
    assert_eq!(actual.host.as_deref(), Some("host"));
    assert_eq!(actual.user, None);
    assert_eq!(
        endpoint(&["ssh", "--", "-destination"]),
        alias_endpoint("-destination")
    );
}

#[test]
fn argv_parses_ssh_uris_including_ipv6_and_explicit_option_precedence() {
    assert_eq!(
        endpoint(&["ssh", "ssh://alice@host.example:2222"]),
        SshEndpoint {
            host: Some("host.example".into()),
            user: Some("alice".into()),
            port: Some(2222),
            ..Default::default()
        }
    );
    assert_eq!(
        endpoint(&["ssh", "ssh://bob@[2001:db8::1]:2200"]),
        SshEndpoint {
            host: Some("2001:db8::1".into()),
            user: Some("bob".into()),
            port: Some(2200),
            ..Default::default()
        }
    );
    assert_eq!(endpoint(&["ssh", "ssh://short"]), host_endpoint("short"));
    assert_eq!(
        endpoint(&["ssh", "ssh://host.example/"]),
        host_endpoint("host.example")
    );
    let actual = endpoint(&["ssh", "-lroot", "-p23", "ssh://alice@host:24"]);
    assert_eq!(actual.user.as_deref(), Some("root"));
    assert_eq!(actual.port, Some(23));
}

#[test]
fn argv_ignores_bad_ports_and_keeps_first_valid_connection_values() {
    for port in ["0", "-1", "65536", "22.5", "bad"] {
        let actual = endpoint(&["ssh", "-p", port, "devbox"]);
        assert_eq!(actual.port, None);
        assert_eq!(actual.host.as_deref(), Some("devbox"));
        assert_eq!(endpoint(&["ssh", &format!("ssh://host:{port}")]).port, None);
    }
    let actual = endpoint(&[
        "ssh",
        "-p22",
        "-p23",
        "-lfirst",
        "-lsecond",
        "-i/first",
        "-i/second",
        "user@host",
    ]);
    assert_eq!(actual.port, Some(22));
    assert_eq!(actual.user.as_deref(), Some("first"));
    assert_eq!(actual.identity, Some(PathBuf::from("/first")));
}

#[test]
fn argv_rejects_incomplete_or_malformed_input_without_panicking() {
    for args in [
        vec![],
        vec!["ssh"],
        vec!["ssh", "-vT"],
        vec!["ssh", "--"],
        vec!["ssh", "-p"],
        vec!["ssh", "-o"],
        vec!["ssh", "-i", "/key"],
        vec!["ssh", ""],
        vec!["ssh", " "],
        vec!["ssh", "user@"],
        vec!["ssh", "ssh://"],
        vec!["ssh", "ssh://[broken"],
        vec!["ssh", "ssh://host/path"],
        vec!["ssh", "-é", "host"],
        vec!["ssh", "--unknown", "host"],
        vec!["ssh", "-", "host"],
    ] {
        assert_eq!(endpoint_from_ssh_argv(&strings(&args)), None, "{args:?}");
    }
}

#[test]
fn argv_handles_the_apps_real_proxy_command_as_an_opaque_argument() {
    let command =
        "sh -c 'set -e; printf ready' sh 'cd /work && exec codex app-server proxy --listen stdio'";
    let common = [
        "ssh",
        "-v",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=12",
    ];
    let mut args = common.to_vec();
    args.extend(["devbox", command]);
    assert_eq!(endpoint(&args), alias_endpoint("devbox"));
    let mut args = common.to_vec();
    args.extend([
        "-i",
        "/path/key",
        "-p",
        "2222",
        "--",
        "user@203.0.113.5",
        command,
    ]);
    assert_eq!(
        endpoint(&args),
        SshEndpoint {
            host: Some("203.0.113.5".into()),
            user: Some("user".into()),
            port: Some(2222),
            identity: Some(PathBuf::from("/path/key")),
            ..Default::default()
        }
    );
}

#[test]
fn connection_hosts_prefer_alias_and_choose_the_expected_labels() {
    let input = connections(json!([
        {"hostId":"a", "displayName":"Friendly", "alias":"devbox", "hostname":"u@other", "sshPort":23, "identity":"/key"},
        {"hostId":"b", "alias":"other"},
        {"hostId":"c", "hostname":"user@host.example", "sshPort":2222, "identity":"/key"},
        {"hostId":"d"}
    ]));
    let actual: Vec<_> = input.iter().map(host_from_connection).collect();
    assert_eq!(actual[0].ssh, alias_endpoint("devbox"));
    assert_eq!(actual[0].label, "Friendly");
    assert_eq!(actual[1].label, "other");
    assert_eq!(actual[2].label, "user@host.example:2222");
    assert_eq!(actual[2].ssh.identity, Some(PathBuf::from("/key")));
    assert_eq!(actual[3].label, "?");
    for (host, connection) in actual.iter().zip(&input) {
        assert_eq!(host.target_id, TargetId::remote(&connection.host_id));
        assert_eq!(host.host_id.as_deref(), Some(connection.host_id.as_str()));
        assert_eq!(host.sources, vec![HostSource::GlobalState]);
        assert_eq!(host.connected, None);
    }
}

#[test]
fn merge_preserves_source_order_and_adds_only_new_hosts() {
    let global = connections(json!([
        {"hostId":"g1", "alias":"one"}, {"hostId":"g2", "alias":"two"}
    ]));
    let from_app = [
        renderer(Some("g1"), "duplicate", alias_endpoint("one")),
        renderer(Some("a1"), "app", alias_endpoint("three")),
    ];
    let actual = merge_hosts(
        &global,
        &from_app,
        &strings(&["two", "four", "four"]),
        &[
            link("four", &["ssh", "four"]),
            link("five", &["ssh", "five"]),
        ],
        &[config_host("one"), config_host("six"), config_host("six")],
    );
    assert_eq!(
        actual
            .iter()
            .map(|host| host.ssh.alias.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("one"),
            Some("two"),
            Some("three"),
            Some("four"),
            Some("five"),
            Some("six")
        ]
    );
    assert_eq!(
        actual[0].sources,
        vec![
            HostSource::GlobalState,
            HostSource::Cdp,
            HostSource::SshConfig
        ]
    );
    assert_eq!(
        actual[3].sources,
        vec![HostSource::GlobalState, HostSource::ProcessScan]
    );
    assert_eq!(actual[4].sources, vec![HostSource::ProcessScan]);
    assert_eq!(actual[5].sources, vec![HostSource::SshConfig]);
    assert_eq!(actual[3].host_id, None);
    assert_eq!(actual[4].host_id, None);
    assert_eq!(actual[5].host_id, None);
    assert_eq!(actual[3].label, "four");
    assert_eq!(actual[4].connected, Some(true));
}

#[test]
fn merge_matches_equal_ids_even_with_different_endpoints() {
    let global = connections(json!([{"hostId":"shared", "hostname":"first.example"}]));
    let incoming = renderer(
        Some("shared"),
        "later",
        SshEndpoint {
            alias: Some("later-alias".into()),
            host: Some("second.example".into()),
            user: Some("alice".into()),
            port: Some(2222),
            identity: Some(PathBuf::from("/key")),
        },
    );
    let actual = merge_hosts(&global, &[incoming], &[], &[], &[]);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].ssh.host.as_deref(), Some("first.example"));
    assert_eq!(actual[0].ssh.alias.as_deref(), Some("later-alias"));
    assert_eq!(actual[0].ssh.user.as_deref(), Some("alice"));
    assert_eq!(actual[0].ssh.port, Some(2222));
    assert_eq!(actual[0].ssh.identity, Some(PathBuf::from("/key")));
    assert_eq!(actual[0].label, "first.example");
}

#[test]
fn merge_matches_aliases_and_retains_the_first_nonempty_fields() {
    let global = connections(json!([{"hostId":"first", "alias":"shared", "displayName":"First"}]));
    let incoming = renderer(Some("second"), "Second", alias_endpoint("shared"));
    let actual = merge_hosts(&global, &[incoming], &[], &[], &[]);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].host_id.as_deref(), Some("first"));
    assert_eq!(actual[0].label, "First");
    assert_eq!(actual[0].target_id, TargetId::remote("first"));
}

#[test]
fn merge_endpoint_matching_uses_default_port_and_optional_user() {
    for (left_port, right_port, left_user, right_user, matches) in [
        (None, Some(22), None, Some("alice"), true),
        (Some(22), None, Some("alice"), None, true),
        (Some(2222), Some(2222), Some("alice"), Some("alice"), true),
        (None, None, None, None, true),
        (None, Some(2222), None, None, false),
        (Some(2222), Some(23), None, None, false),
        (None, None, Some("alice"), Some("bob"), false),
    ] {
        let left = SshEndpoint {
            port: left_port,
            user: left_user.map(str::to_owned),
            ..host_endpoint("same.example")
        };
        let right = SshEndpoint {
            port: right_port,
            user: right_user.map(str::to_owned),
            ..host_endpoint("same.example")
        };
        let actual = merge_hosts(
            &[],
            &[renderer(None, "left", left), renderer(None, "right", right)],
            &[],
            &[],
            &[],
        );
        assert_eq!(actual.len(), if matches { 1 } else { 2 });
        if matches {
            assert_eq!(actual[0].ssh.user.as_deref(), left_user.or(right_user));
            assert_eq!(actual[0].ssh.port, left_port.or(right_port));
        }
    }
}

#[test]
fn merge_absent_identity_fields_do_not_match_each_other() {
    let actual = merge_hosts(
        &[],
        &[
            renderer(None, "one", SshEndpoint::default()),
            renderer(None, "two", SshEndpoint::default()),
            renderer(None, "three", host_endpoint("other")),
        ],
        &[],
        &[],
        &[],
    );
    assert_eq!(actual.len(), 3);
    assert_eq!(actual[0].target_id, TargetId::remote("one"));
    assert_eq!(actual[1].target_id, TargetId::remote("two"));
    assert_eq!(actual[2].target_id, TargetId::remote("other"));
}

#[test]
fn merge_fills_missing_identity_and_label_and_deduplicates_sources_in_order() {
    let mut first = renderer(None, "", host_endpoint("same.example"));
    first.sources = vec![HostSource::Manual, HostSource::Cdp, HostSource::Manual];
    let mut second = renderer(
        Some("learned"),
        "Filled",
        SshEndpoint {
            alias: Some("filled".into()),
            port: Some(22),
            user: Some("alice".into()),
            identity: Some(PathBuf::from("/filled")),
            ..host_endpoint("same.example")
        },
    );
    second.sources = vec![
        HostSource::Cdp,
        HostSource::GlobalState,
        HostSource::GlobalState,
    ];
    second.connected = Some(false);
    let third = renderer(
        Some("learned"),
        "Ignored",
        SshEndpoint {
            port: Some(23),
            user: Some("bob".into()),
            identity: Some(PathBuf::from("/ignored")),
            ..alias_endpoint("ignored")
        },
    );
    let actual = merge_hosts(&[], &[first, second, third], &[], &[], &[]);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].host_id.as_deref(), Some("learned"));
    assert_eq!(actual[0].target_id, TargetId::remote("learned"));
    assert_eq!(actual[0].label, "Filled");
    assert_eq!(actual[0].ssh.alias.as_deref(), Some("filled"));
    assert_eq!(actual[0].ssh.host.as_deref(), Some("same.example"));
    assert_eq!(actual[0].ssh.port, Some(22));
    assert_eq!(actual[0].ssh.user.as_deref(), Some("alice"));
    assert_eq!(actual[0].ssh.identity, Some(PathBuf::from("/filled")));
    assert_eq!(
        actual[0].sources,
        vec![HostSource::Manual, HostSource::Cdp, HostSource::GlobalState]
    );
    assert_eq!(actual[0].connected, Some(false));
}

#[test]
fn merge_renderer_connection_observation_is_preserved_until_a_live_link_matches() {
    for connected in [Some(false), Some(true), None] {
        let global = connections(json!([{"hostId":"id", "alias":"devbox"}]));
        let mut from_app = renderer(Some("id"), "app", alias_endpoint("devbox"));
        from_app.connected = connected;
        let actual = merge_hosts(&global, &[from_app.clone()], &[], &[], &[]);
        assert_eq!(actual[0].connected, connected);
        let actual = merge_hosts(
            &global,
            &[from_app],
            &[],
            &[link("devbox", &["ssh", "devbox"])],
            &[],
        );
        assert_eq!(actual[0].connected, Some(true));
        assert_eq!(
            actual[0].sources,
            vec![
                HostSource::GlobalState,
                HostSource::Cdp,
                HostSource::ProcessScan
            ]
        );
    }
}

#[test]
fn merge_live_links_match_alias_or_hostname_even_when_argv_classifies_it_differently() {
    let known = [
        renderer(Some("alias"), "known alias", alias_endpoint("devbox")),
        renderer(Some("host"), "known host", host_endpoint("short")),
        renderer(
            Some("fqdn"),
            "known fqdn alias",
            alias_endpoint("box.example"),
        ),
    ];
    let actual = merge_hosts(
        &[],
        &known,
        &[],
        &[
            link("stale", &["ssh", "-i/key", "-p2222", "user@devbox"]),
            link("stale", &["ssh", "short"]),
            link("stale", &["ssh", "box.example"]),
        ],
        &[],
    );
    assert_eq!(actual.len(), 3);
    assert!(actual.iter().all(|host| host.connected == Some(true)));
    assert_eq!(actual[0].ssh.identity, Some(PathBuf::from("/key")));
    assert_eq!(actual[0].ssh.port, Some(2222));
}

#[test]
fn merge_live_links_prefer_argv_and_fall_back_to_split_remote_link_host() {
    let mut unrelated = link("unrelated", &["ssh", "unrelated"]);
    unrelated.kind = CodexProcessKind::CliTui;
    let actual = merge_hosts(
        &[],
        &[],
        &[],
        &[
            unrelated,
            link("ignored", &["ssh", "actual"]),
            link("alice@fallback.example", &["not-ssh", "wrong"]),
            link("plain", &[]),
            link(" ", &[]),
            link("user@", &[]),
        ],
        &[],
    );
    assert_eq!(actual.len(), 3);
    assert_eq!(actual[0].ssh.alias.as_deref(), Some("actual"));
    assert_eq!(actual[1].ssh.host.as_deref(), Some("fallback.example"));
    assert_eq!(actual[1].ssh.user.as_deref(), Some("alice"));
    assert_eq!(actual[2].ssh.alias.as_deref(), Some("plain"));
    assert!(
        actual
            .iter()
            .all(|host| host.host_id.is_none() && host.connected == Some(true))
    );
}

#[test]
fn merge_config_adds_provenance_without_enriching_known_aliases() {
    let known = renderer(Some("id"), "Existing", alias_endpoint("known"));
    let config = SshConfigHost {
        alias: "known".into(),
        host_name: Some("real.example".into()),
        port: Some(2222),
        user: Some("alice".into()),
        identity_file: Some("/key".into()),
    };
    let suggestion = SshConfigHost {
        alias: "suggestion".into(),
        ..config.clone()
    };
    let actual = merge_hosts(&[], &[known], &[], &[], &[config, suggestion]);
    assert_eq!(actual[0].ssh, alias_endpoint("known"));
    assert_eq!(actual[0].label, "Existing");
    assert_eq!(actual[0].connected, None);
    assert_eq!(
        actual[0].sources,
        vec![HostSource::Cdp, HostSource::SshConfig]
    );
    assert_eq!(
        actual[1].ssh,
        SshEndpoint {
            alias: Some("suggestion".into()),
            host: Some("real.example".into()),
            port: Some(2222),
            user: Some("alice".into()),
            identity: Some(PathBuf::from("/key"))
        }
    );
    assert_eq!(actual[1].target_id, TargetId::remote("suggestion"));
    assert!(is_suggestion(&actual[1]));
    assert!(!is_suggestion(&actual[0]));
}

#[test]
fn merge_config_matches_only_alias_and_declared_aliases_never_synthesize_ids() {
    let known = renderer(None, "host", host_endpoint("host.example"));
    let config = SshConfigHost {
        host_name: Some("host.example".into()),
        ..config_host("new")
    };
    let actual = merge_hosts(
        &[],
        &[known],
        &strings(&[" declared ", "declared", " "]),
        &[],
        &[config],
    );
    assert_eq!(actual.len(), 3);
    assert_eq!(actual[1].host_id, None);
    assert_eq!(actual[1].sources, vec![HostSource::GlobalState]);
    assert_eq!(actual[1].target_id, TargetId::remote("declared"));
    assert_eq!(actual[1].label, "declared");
    assert!(!is_suggestion(&actual[1]));
    assert!(is_suggestion(&actual[2]));
}

#[test]
fn merge_collapses_duplicate_global_entries_and_recomputes_all_target_ids() {
    let global = connections(json!([
        {"hostId":"same", "hostname":"one"}, {"hostId":"same", "alias":"learned"},
        {"hostId":"other", "hostname":"other"}
    ]));
    let actual = merge_hosts(
        &global,
        &[
            renderer(None, "label", alias_endpoint("alias")),
            renderer(None, "label", host_endpoint("host.example")),
            renderer(None, "label", SshEndpoint::default()),
        ],
        &[],
        &[],
        &[],
    );
    assert_eq!(
        actual
            .iter()
            .map(|host| host.target_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "ssh:same",
            "ssh:other",
            "ssh:alias",
            "ssh:host.example",
            "ssh:label"
        ]
    );
    assert_eq!(actual[0].ssh.alias.as_deref(), Some("learned"));
    assert_eq!(actual[0].sources, vec![HostSource::GlobalState]);
}

#[test]
fn suggestion_requires_exactly_one_ssh_config_source() {
    let mut host = renderer(None, "alias", alias_endpoint("alias"));
    for (sources, expected) in [
        (vec![], false),
        (vec![HostSource::SshConfig], true),
        (vec![HostSource::GlobalState], false),
        (vec![HostSource::SshConfig, HostSource::Cdp], false),
        (vec![HostSource::SshConfig, HostSource::SshConfig], false),
    ] {
        host.sources = sources;
        assert_eq!(is_suggestion(&host), expected);
    }
}

#[test]
fn discovery_missing_files_still_merges_supplied_observations() {
    let temp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(temp.path().join("missing-codex"));
    assert!(discover_from_files(&home, Some(temp.path()), &[], &[]).is_empty());
    let from_app = [renderer(Some("id"), "app", alias_endpoint("app"))];
    let live = [link("live", &["ssh", "live"])];
    let actual = discover_from_files(&home, None, &from_app, &live);
    assert_eq!(actual.len(), 2);
    assert_eq!(actual[0].target_id, TargetId::remote("id"));
    assert_eq!(actual[1].target_id, TargetId::remote("live"));
}

#[test]
fn discovery_falls_back_to_backup_for_missing_invalid_or_nonobject_main() {
    for main in [None, Some("{"), Some("[]"), Some("null"), Some("42")] {
        let temp = tempfile::tempdir().unwrap();
        let home = CodexHome::new(temp.path());
        if let Some(main) = main {
            write_file(temp.path(), ".codex-global-state.json", main);
        }
        write_file(
            temp.path(),
            ".codex-global-state.json.bak",
            serde_json::to_vec(&json!({KEY_CONNECTIONS:[{"hostId":"backup", "alias":"saved"}]}))
                .unwrap(),
        );
        let actual = discover_from_files(&home, None, &[], &[]);
        assert_eq!(actual.len(), 1, "main {main:?}");
        assert_eq!(actual[0].host_id.as_deref(), Some("backup"));
    }
}

#[test]
fn discovery_valid_main_including_empty_objects_takes_precedence_over_backup() {
    for main in [
        json!({}),
        json!({KEY_CONNECTIONS:"wrong type"}),
        json!({KEY_CONNECTIONS:[{"hostId":"main", "alias":"current"}]}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let home = CodexHome::new(temp.path());
        write_file(
            temp.path(),
            ".codex-global-state.json",
            serde_json::to_vec(&main).unwrap(),
        );
        write_file(
            temp.path(),
            ".codex-global-state.json.bak",
            serde_json::to_vec(&json!({KEY_CONNECTIONS:[{"hostId":"backup"}]})).unwrap(),
        );
        let actual = discover_from_files(&home, None, &[], &[]);
        assert!(
            actual
                .iter()
                .all(|host| host.host_id.as_deref() == Some("main"))
        );
        assert_eq!(actual.len(), usize::from(main[KEY_CONNECTIONS].is_array()));
    }
}

#[test]
fn discovery_invalid_main_and_unusable_backup_leave_other_sources_available() {
    for backup in [None, Some("bad json"), Some("[]")] {
        let temp = tempfile::tempdir().unwrap();
        let home = CodexHome::new(temp.path());
        write_file(temp.path(), ".codex-global-state.json", "bad json");
        if let Some(backup) = backup {
            write_file(temp.path(), ".codex-global-state.json.bak", backup);
        }
        write_file(
            temp.path(),
            "codex-app/config.json",
            br#"{"remoteConnections":[{"sshAlias":"declared"}]}"#,
        );
        let actual = discover_from_files(&home, None, &[], &[]);
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].ssh.alias.as_deref(), Some("declared"));
        assert_eq!(actual[0].host_id, None);
    }
}

#[test]
fn discovery_integrates_all_files_and_only_loads_ssh_for_an_explicit_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(temp.path().join("codex"));
    write_file(
        home.root(),
        ".codex-global-state.json",
        serde_json::to_vec(&json!({KEY_CONNECTIONS:[{"hostId":"global-id", "alias":"global"}]}))
            .unwrap(),
    );
    write_file(
        home.root(),
        "codex-app/config.json",
        br#"{"remoteConnections":[{"sshAlias":"declared"}]}"#,
    );
    write_file(
        temp.path(),
        ".ssh/config",
        "Host suggestion\nIdentityFile /nonexistent/private-key\nHost global\nHost declared\n",
    );
    let from_app = [renderer(Some("app-id"), "app", alias_endpoint("app"))];
    let live = [link("live", &["ssh", "live"])];
    let actual = discover_from_files(&home, Some(temp.path()), &from_app, &live);
    assert_eq!(
        actual
            .iter()
            .map(|host| host.label.as_str())
            .collect::<Vec<_>>(),
        vec!["global", "app", "declared", "live", "suggestion"]
    );
    assert_eq!(
        actual[0].sources,
        vec![HostSource::GlobalState, HostSource::SshConfig]
    );
    assert_eq!(
        actual[2].sources,
        vec![HostSource::GlobalState, HostSource::SshConfig]
    );
    assert_eq!(
        actual[4].ssh.identity,
        Some(PathBuf::from("/nonexistent/private-key"))
    );
    assert_eq!(discover_from_files(&home, None, &from_app, &live).len(), 4);
    write_file(home.root(), "codex-app/config.json", "invalid");
    assert_eq!(discover_from_files(&home, None, &[], &[]).len(), 1);
}
