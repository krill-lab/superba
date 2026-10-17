use pretty_assertions::{assert_eq, assert_ne};
use superba_config::toml_keys::{ConfigDoc, literal_repr, parse_repr, reprs_equal, string_repr};
use superba_core::{Error, codex::TomlLiteral};

const REALISTIC: &str = r###"# Codex configuration / 用户配置
model_provider = 'krill' # keep provider notes
model = "gpt-6"
openai_base_url = 'https://example.test/codex/v1/'
description = """
多行内容
second line with \"quotes\"
"""
literal = '''
C:\Users\名字
literal # text
'''

[mcp_servers.x] # 工具
command = 'node'
args = ["server.js", '--verbose']
env = { TOKEN = "dummy-token", LABEL = '中文' }

[model_providers.krill]
name = 'Krill'
base_url = "https://example.test/v1/"
env_key = "KRILL_API_KEY"
wire_api = 'responses'

[profiles.p]
model = 'gpt-6'
model_reasoning_effort = "high"

[[some.array]]
name = 'one'
numbers = [
    1, # a comment in an array
    2,
]

[[some.array]]
name = "第二个"

# End of the user's configuration.
"###;

fn doc(source: &str) -> ConfigDoc {
    ConfigDoc::parse(source.as_bytes()).unwrap()
}

fn assert_bytes(document: &ConfigDoc, expected: &str) {
    assert_eq!(document.to_bytes(), expected.as_bytes());
    // Each expected result must remain valid TOML after an edit.
    ConfigDoc::parse(&document.to_bytes()).unwrap();
}

#[test]
fn realistic_config_round_trips() {
    assert_bytes(&doc(REALISTIC), REALISTIC);
}

#[test]
fn round_trip_preserves_crlf_and_bom() {
    for newline in ["\n", "\r\n"] {
        for bom in ["", "\u{feff}"] {
            let source = format!("{bom}{}", REALISTIC.replace('\n', newline));
            assert_bytes(&doc(&source), &source);
        }
    }
}

#[test]
fn new_root_key_preserves_crlf_and_bom_everywhere() {
    let source = format!("\u{feff}{}", REALISTIC.replace('\n', "\r\n"));
    let mut document = doc(&source);
    document.set_repr("web_search", "\"live\"").unwrap();
    let expected = source.replacen(
        "'''\r\n\r\n[mcp_servers.x]",
        "'''\r\nweb_search = \"live\"\r\n\r\n[mcp_servers.x]",
        1,
    );
    assert_bytes(&document, &expected);
    let bytes = document.to_bytes();
    assert!(bytes.starts_with(b"\xef\xbb\xbf"));
    assert!(
        bytes
            .iter()
            .enumerate()
            .all(|(i, &byte)| byte != b'\n' || i > 0 && bytes[i - 1] == b'\r')
    );
}

#[test]
fn empty_and_whitespace_documents_round_trip() {
    assert_bytes(&ConfigDoc::empty(), "");
    for source in [
        "",
        "\u{feff}",
        " \t\n\n",
        "\u{feff}\r\n \t\r\n",
        "# only comments\n",
    ] {
        let document = doc(source);
        assert_bytes(&document, source);
        assert!(!document.has_table("features"));
        assert_eq!(document.get_repr("model"), None);
    }
}

#[test]
fn final_newline_and_interleaved_dotted_keys_round_trip() {
    for source in [
        "model = '中文'",
        "[empty]",
        "a.x=1\nb=2\na.y=3\n",
        "a.x=1\r\nb=2\na.y=3",
        "# comment without newline",
    ] {
        assert_bytes(&doc(source), source);
    }
}

#[test]
fn existing_value_keeps_key_and_value_decor() {
    let source =
        "# provider notes\n  'model_provider'\t=  'krill'  # trailing\nmodel = 'unchanged'\n";
    let mut document = doc(source);
    document.set_repr("model_provider", "\"openai\"").unwrap();
    assert_bytes(
        &document,
        "# provider notes\n  'model_provider'\t=  \"openai\"  # trailing\nmodel = 'unchanged'\n",
    );
    let once = document.to_bytes();
    document.set_repr("model_provider", "\"openai\"").unwrap();
    assert_eq!(document.to_bytes(), once);
}

#[test]
fn equal_semantics_can_change_repr_but_exact_repr_is_a_noop() {
    let mut document = doc("model='openai' # note");
    document.set_repr("model", "'openai'").unwrap();
    assert_bytes(&document, "model='openai' # note");
    document.set_repr("model", "\"openai\"").unwrap();
    assert_bytes(&document, "model=\"openai\" # note");
}

#[test]
fn new_root_value_precedes_tables_and_keeps_initial_comment() {
    let mut document =
        doc("# configuration\nmodel = 'original'\n\n[mcp_servers.x]\ncommand='node'\n");
    document.set_repr("web_search", "'live'").unwrap();
    assert_bytes(
        &document,
        "# configuration\nmodel = 'original'\nweb_search = 'live'\n\n[mcp_servers.x]\ncommand='node'\n",
    );
}

#[test]
fn new_root_value_in_a_document_containing_only_tables() {
    for source in [
        "[mcp_servers.x]\ncommand='node'\n",
        "# table comment\n[mcp_servers.x]\ncommand='node'\n",
    ] {
        let mut document = doc(source);
        document.set_repr("model", "'new'").unwrap();
        assert_bytes(&document, &format!("model = 'new'\n{source}"));
        assert!(document.remove("model"));
        assert_bytes(&document, source);
    }
}

#[test]
fn insert_into_standard_features_table() {
    let mut document = doc("[features]\nother = 1\n\n[profiles.p]\nmodel='stay'\n");
    document.set_repr("features.fast_mode", "true").unwrap();
    assert_bytes(
        &document,
        "[features]\nother = 1\nfast_mode = true\n\n[profiles.p]\nmodel='stay'\n",
    );
}

#[test]
fn insert_into_an_empty_existing_table() {
    let mut document = doc("[features]\n\n[profiles.p]\nmodel='stay'\n");
    document.set_repr("features.fast_mode", "true").unwrap();
    assert_bytes(
        &document,
        "[features]\nfast_mode = true\n\n[profiles.p]\nmodel='stay'\n",
    );
}

#[test]
fn dotted_table_keeps_its_style_and_surrounding_order() {
    let mut document = doc("features.other = 1\nname='stay'\nfeatures.last = 2\n");
    document.set_repr("features.fast_mode", "true").unwrap();
    assert_bytes(
        &document,
        "features.other = 1\nname='stay'\nfeatures.last = 2\nfeatures.fast_mode = true\n",
    );
    document.set_repr("features.other", "3").unwrap();
    assert_bytes(
        &document,
        "features.other = 3\nname='stay'\nfeatures.last = 2\nfeatures.fast_mode = true\n",
    );
}

#[test]
fn inline_table_keeps_its_style_and_decor() {
    let mut document = doc("features = { other = 1 } # features\nname='stay'\n");
    document.set_repr("features.fast_mode", "true").unwrap();
    assert_bytes(
        &document,
        "features = { other = 1, fast_mode = true } # features\nname='stay'\n",
    );
    document.set_repr("features.other", "42").unwrap();
    assert_bytes(
        &document,
        "features = { other = 42, fast_mode = true } # features\nname='stay'\n",
    );
}

#[test]
fn inline_dotted_paths_and_missing_nested_inline_parents() {
    let mut document = doc("features = { token_budget.other=1, last=2 }\n");
    document
        .set_repr("features.token_budget.enabled", "true")
        .unwrap();
    assert_bytes(
        &document,
        "features = { token_budget.other=1, last=2, token_budget.enabled = true }\n",
    );
    document
        .set_repr("features.new_parent.enabled", "false")
        .unwrap();
    assert_bytes(
        &document,
        "features = { token_budget.other=1, last=2, token_budget.enabled = true, new_parent = { enabled = false } }\n",
    );
}

#[test]
fn inline_dotted_parents_read_as_tables_and_update_in_place() {
    let source = "features = { token_budget.enabled = true, other = 1 }\n";
    let mut document = doc(source);
    assert!(document.has_table("features.token_budget"));
    assert_eq!(
        document.get_repr("features.token_budget").as_deref(),
        Some("{ enabled = true }")
    );
    assert_eq!(
        document.get_value("features.token_budget"),
        parse_repr("{ enabled = true }")
    );
    // A value that is not a table cannot replace the table.
    assert!(matches!(
        document.set_repr("features.token_budget", "false"),
        Err(Error::Conflict { .. })
    ));
    assert_bytes(&document, source);
    // A table keeps the dotted style.
    let mut updated = document.clone();
    updated
        .set_repr("features.token_budget", "{ enabled = false, extra = 2 }")
        .unwrap();
    assert_bytes(
        &updated,
        "features = { token_budget.enabled = false, other = 1, token_budget.extra = 2 }\n",
    );
    assert!(document.remove("features.token_budget.enabled"));
    assert_bytes(&document, "features = { other = 1 }\n");
}

const SECTIONS: &str = "# 用户配置\n\
[shell_environment_policy]\n\
inherit = \"core\"\n\
\n\
[shell_environment_policy.set] # 环境变量\n\
PATH = \"/usr/bin\" # keep me\n\
OLD = \"x\"\n\
\n\
[mcp_servers.docs]\n\
command = \"docs-mcp\"\n\
\n\
[mcp_servers.docs.env]\n\
TOKEN = \"a\" # rotate monthly\n\
\n\
[tui]\n\
animations = true\n";

#[test]
fn a_section_reads_as_an_inline_table_and_is_updated_in_place() {
    let mut document = doc(SECTIONS);
    let path = "shell_environment_policy.set";
    let current = document.get_repr(path).unwrap();
    assert_eq!(current, r#"{ PATH = "/usr/bin", OLD = "x" }"#);
    assert!(reprs_equal(&current, r#"{OLD = "x", PATH = '/usr/bin'}"#));
    assert_eq!(
        document.get_repr("mcp_servers.docs").as_deref(),
        Some(r#"{ command = "docs-mcp", env = { TOKEN = "a" } }"#)
    );

    // Changed, added and removed inside the section; comments and every other
    // byte stay.
    let wanted = r#"{ PATH = "/opt/bin:/usr/bin", NEW = "1" }"#;
    document.set_repr(path, wanted).unwrap();
    let expected = SECTIONS.replace(
        "PATH = \"/usr/bin\" # keep me\nOLD = \"x\"\n",
        "PATH = \"/opt/bin:/usr/bin\" # keep me\nNEW = \"1\"\n",
    );
    assert_bytes(&document, &expected);
    assert!(reprs_equal(&document.get_repr(path).unwrap(), wanted));
    // The same value again changes nothing.
    document.set_repr(path, wanted).unwrap();
    assert_bytes(&document, &expected);
}

#[test]
fn nested_tables_under_a_dotted_section_are_updated_in_place() {
    let mut document = doc(SECTIONS);
    let wanted =
        r#"{ command = "docs-mcp", args = ["--stdio"], env = { TOKEN = "b", REGION = "eu" } }"#;
    document.set_repr("mcp_servers.docs", wanted).unwrap();
    let expected = SECTIONS
        .replace(
            "command = \"docs-mcp\"\n",
            "command = \"docs-mcp\"\nargs = [\"--stdio\"]\n",
        )
        .replace(
            "TOKEN = \"a\" # rotate monthly\n",
            "TOKEN = \"b\" # rotate monthly\nREGION = \"eu\"\n",
        );
    assert_bytes(&document, &expected);
    assert!(reprs_equal(
        &document.get_repr("mcp_servers.docs").unwrap(),
        wanted
    ));
    // A nested table set on its own path is a section update too.
    document
        .set_repr("mcp_servers.docs.env", r#"{ REGION = "us" }"#)
        .unwrap();
    assert_bytes(
        &document,
        &expected.replace(
            "TOKEN = \"b\" # rotate monthly\nREGION = \"eu\"\n",
            "REGION = \"us\"\n",
        ),
    );
}

#[test]
fn an_emptied_section_reads_back_as_an_empty_table() {
    let mut document = doc("[shell_environment_policy.set]\nA = \"1\"\n");
    document
        .set_repr("shell_environment_policy.set", "{}")
        .unwrap();
    assert_eq!(
        document.get_value("shell_environment_policy.set"),
        parse_repr("{}")
    );
}

#[test]
fn section_updates_that_cannot_be_expressed_are_conflicts_and_change_nothing() {
    let source = format!(
        "{SECTIONS}\n[odd]\n\"a b\" = 1\n\n[arr]\nx = 1\n\n[[arr.items]]\nname = \"one\"\n"
    );
    let mut document = doc(&source);
    for (path, repr) in [
        // A value that is not a table where a section is.
        ("shell_environment_policy.set", "\"PATH=/usr/bin\""),
        // A key that a dotted path cannot address (new or present).
        ("shell_environment_policy.set", r#"{ "a b" = "1" }"#),
        ("odd", "{ c = 2 }"),
        // An array of tables cannot become an inline value; the change to `x`
        // before it is undone.
        ("arr", r#"{ x = 2, items = [{ name = "two" }] }"#),
    ] {
        assert!(
            matches!(document.set_repr(path, repr), Err(Error::Conflict { .. })),
            "{path} = {repr}"
        );
        assert_bytes(&document, &source);
    }
    assert_eq!(document.get_repr("arr"), None);
}

#[test]
fn edits_preserve_mixed_existing_endings_and_use_crlf_for_new_lines() {
    let source = "model='stay'\nother=1\r\n[mcp_servers.x]\ncommand='node'\r\n";
    let mut document = doc(source);
    document.set_repr("web_search", "'live'").unwrap();
    assert_bytes(
        &document,
        "model='stay'\nother=1\r\nweb_search = 'live'\r\n[mcp_servers.x]\ncommand='node'\r\n",
    );
    assert!(document.remove("web_search"));
    assert_bytes(&document, source);
}

#[test]
fn absent_features_get_a_new_header_at_the_end() {
    let source = "model='stay'\n\n[profiles.p]\nmodel='profile'\n";
    let mut document = doc(source);
    document.set_repr("features.fast_mode", "true").unwrap();
    assert_bytes(
        &document,
        &format!("{source}\n[features]\nfast_mode = true\n"),
    );
}

#[test]
fn empty_document_gets_a_header_without_a_leading_blank() {
    let mut document = ConfigDoc::empty();
    document.set_repr("features.fast_mode", "true").unwrap();
    assert_bytes(&document, "[features]\nfast_mode = true\n");
}

#[test]
fn nested_standard_tables_are_appended_after_existing_content() {
    for source in [
        "model='stay'\n",
        "[features]\nother=1\n\n[profiles.p]\nmodel='stay'\n",
        "features.other = 1\n",
        "[features.other]\nx=1\n\n[profiles.p]\nmodel='stay'\n",
    ] {
        let mut document = doc(source);
        document
            .set_repr("features.token_budget.enabled", "true")
            .unwrap();
        assert_bytes(
            &document,
            &format!("{source}\n[features.token_budget]\nenabled = true\n"),
        );
        assert!(document.has_table("features"));
        assert!(document.has_table("features.token_budget"));
    }
}

#[test]
fn implicit_parent_can_receive_its_own_value() {
    let mut document = doc("[features.token_budget]\nenabled=true\n\n[profiles.p]\nmodel='stay'\n");
    document.set_repr("features.fast_mode", "false").unwrap();
    assert_bytes(
        &document,
        "[features.token_budget]\nenabled=true\n\n[profiles.p]\nmodel='stay'\n\n[features]\nfast_mode = false\n",
    );
}

#[test]
fn conflict_is_atomic_for_scalars_arrays_and_tables() {
    for (source, path) in [
        ("features = 1\n", "features.fast_mode"),
        ("features = [1,2]\n", "features.fast_mode"),
        (
            "features = {token_budget=1}\n",
            "features.token_budget.enabled",
        ),
        ("[[features]]\nother=1\n", "features.fast_mode"),
        ("[features]\nother=1\n", "features"),
        ("[[features]]\nother=1\n", "features"),
    ] {
        let mut document = doc(source);
        assert!(matches!(
            document.set_repr(path, "true"),
            Err(Error::Conflict { .. })
        ));
        assert_bytes(&document, source);
    }
}

#[test]
fn invalid_reprs_and_paths_are_atomic() {
    let mut document = doc("model='stay'\n");
    for repr in [
        "",
        "not a value",
        "[1,",
        "true\nother=1",
        "true # comment",
        "1\n[table]",
    ] {
        assert!(
            matches!(
                document.set_repr("features.new.enabled", repr),
                Err(Error::Invalid { .. })
            ),
            "{repr:?}"
        );
        assert_bytes(&document, "model='stay'\n");
    }
    for path in [
        "",
        ".model",
        "model.",
        "features..fast_mode",
        "'model'",
        "a b",
        "a\nb",
    ] {
        assert!(matches!(
            document.set_repr(path, "true"),
            Err(Error::Invalid { .. })
        ));
        assert_eq!(document.get_repr(path), None);
        assert!(!document.has_table(path));
        assert!(!document.remove(path));
        assert_bytes(&document, "model='stay'\n");
    }
}

#[test]
fn removing_a_leaf_keeps_neighbours_exactly() {
    let mut document = doc("model='stay'\nweb_search = 'live' # removed\nother  =  'keep'\n");
    assert!(document.remove("web_search"));
    assert_bytes(&document, "model='stay'\nother  =  'keep'\n");
}

#[test]
fn removing_a_leaf_moves_leading_comments_to_the_next_value() {
    let mut document = doc(
        "[features]\nbefore=1\n\n# user comment\n  fast_mode = true\n# neighbour comment\nother=2\n",
    );
    assert!(document.remove("features.fast_mode"));
    assert_bytes(
        &document,
        "[features]\nbefore=1\n\n# user comment\n  # neighbour comment\nother=2\n",
    );
}

#[test]
fn last_leaf_comments_are_dropped_without_crossing_table_boundaries() {
    let mut document =
        doc("[features]\nother=1\n# key comment\nfast_mode=true\n\n[profiles.p]\nmodel='stay'\n");
    assert!(document.remove("features.fast_mode"));
    assert_bytes(
        &document,
        "[features]\nother=1\n\n[profiles.p]\nmodel='stay'\n",
    );
}

#[test]
fn dotted_key_comments_move_to_the_next_actual_occurrence() {
    let mut document = doc(
        "features.first=1\n# user comment\nfeatures.fast_mode=true\nroot='stay'\nfeatures.last=2\n",
    );
    assert!(document.remove("features.fast_mode"));
    assert_bytes(
        &document,
        "features.first=1\nroot='stay'\n# user comment\nfeatures.last=2\n",
    );
}

#[test]
fn newly_empty_tables_are_pruned_up_to_the_root() {
    for (source, path, expected) in [
        ("[analytics]\nenabled = false\n", "analytics.enabled", ""),
        (
            "[features.token_budget]\nenabled = true\n",
            "features.token_budget.enabled",
            "",
        ),
        (
            "[features]\n\n[features.token_budget]\nenabled = true\n",
            "features.token_budget.enabled",
            "",
        ),
        (
            "[empty]\n\n[analytics]\nenabled = false\n",
            "analytics.enabled",
            "[empty]\n",
        ),
        (
            "features.fast_mode = true\nname='stay'\n",
            "features.fast_mode",
            "name='stay'\n",
        ),
    ] {
        let mut document = doc(source);
        assert!(document.remove(path));
        assert_bytes(&document, expected);
    }
}

#[test]
fn tables_with_comments_and_preexisting_empty_children_survive() {
    for (source, expected) in [
        (
            "# user section\n[analytics]\nenabled=false\n",
            "# user section\n[analytics]\n",
        ),
        (
            "[analytics] # user section\nenabled=false\n",
            "[analytics] # user section\n",
        ),
        (
            "[analytics]\nenabled=false\n[analytics.empty]\n",
            "[analytics]\n[analytics.empty]\n",
        ),
        (
            "[already_empty]\n[analytics]\nenabled=false\n",
            "[already_empty]\n",
        ),
    ] {
        let mut document = doc(source);
        assert!(document.remove("analytics.enabled"));
        assert_bytes(&document, expected);
    }
}

#[test]
fn missing_or_untraversable_removal_is_a_noop() {
    let source = "features=1\n[empty]\n[[servers]]\nname='stay'\n";
    let mut document = doc(source);
    for path in [
        "missing",
        "features.fast_mode",
        "empty.missing",
        "servers.name",
    ] {
        assert!(!document.remove(path));
        assert_bytes(&document, source);
    }
}

#[test]
fn removing_inline_leaves_preserves_neighbours_and_valid_commas() {
    for (source, path, expected) in [
        (
            "features={ fast_mode=true, other=1 }\n",
            "features.fast_mode",
            "features={ other=1 }\n",
        ),
        (
            "features={ other=1, fast_mode=true }\n",
            "features.fast_mode",
            "features={ other=1 }\n",
        ),
        (
            "features={ a=1, fast_mode=true, b=2 }\n",
            "features.fast_mode",
            "features={ a=1, b=2 }\n",
        ),
        (
            "features={ other=1, fast_mode=true, }\n",
            "features.fast_mode",
            "features={ other=1, }\n",
        ),
        (
            "features={ fast_mode=true }\nother=1\n",
            "features.fast_mode",
            "other=1\n",
        ),
        (
            "features={ fast_mode=true } # keep\n",
            "features.fast_mode",
            "features={ } # keep\n",
        ),
        (
            "features={ budget.enabled=true, budget.other=1 }\n",
            "features.budget.enabled",
            "features={ budget.other=1 }\n",
        ),
    ] {
        let mut document = doc(source);
        assert!(document.remove(path), "{source:?}");
        assert_bytes(&document, expected);
    }
}

#[test]
fn setting_and_removing_a_new_key_restores_all_original_bytes() {
    for source in [
        "",
        "# only a comment",
        "\u{feff}",
        "\u{feff}model='stay'\r\n",
        "model='stay'",
        "model='stay'\n",
        "model='stay'\n\n",
        "model='stay'\n\n# trailing comment\n",
        "[profiles.p]\nmodel='stay'\n",
        "# table comment\n[profiles.p]\nmodel='stay'\n",
        REALISTIC,
    ] {
        for path in ["web_search", "features.x", "features.token_budget.enabled"] {
            let mut document = doc(source);
            document.set_repr(path, "true").unwrap();
            assert!(document.remove(path), "{source:?}, {path}");
            assert_bytes(&document, source);
        }
    }
}

#[test]
fn generated_separators_survive_other_edits_until_they_are_unneeded() {
    let mut document = doc("model='stay'");
    document.set_repr("web_search", "'live'").unwrap();
    document.set_repr("features.x", "true").unwrap();
    document
        .set_repr("chatgpt_base_url", "'https://example.test/'")
        .unwrap();
    document.set_repr("features.x", "false").unwrap();
    assert!(document.remove("web_search"));
    assert!(document.remove("features.x"));
    assert!(document.remove("chatgpt_base_url"));
    assert_bytes(&document, "model='stay'");
}

#[test]
fn stripped_compares_only_unowned_content_and_leaves_originals_unchanged() {
    let a =
        "model='a'\nuser_key='same'\n\n[features]\nfast_mode=true\n\n[analytics]\nenabled=false\n";
    let b = "model=\"b\"\nuser_key='same'\n\n[features]\nfast_mode=false\n\n[analytics]\nenabled=true\n";
    let paths = ["model", "features.fast_mode", "analytics.enabled"];
    let first = doc(a);
    let second = doc(b);
    assert_eq!(first.stripped(&paths), "user_key='same'\n");
    assert_eq!(first.stripped(&paths), second.stripped(&paths));
    assert_ne!(
        first.stripped(&paths),
        doc(&b.replace("'same'", "'different'")).stripped(&paths)
    );
    assert_bytes(&first, a);
    assert_bytes(&second, b);
}

#[test]
fn stripped_preserves_bom_and_crlf() {
    let document = doc("\u{feff}model='a'\r\nuser_key='same'\r\n");
    assert_eq!(document.stripped(&["model"]), "\u{feff}user_key='same'\r\n");
}

#[test]
fn invalid_toml_reports_the_problem_line() {
    for newline in ["\n", "\r\n"] {
        let source = ["# 配置", "model='valid'", "bad = ?", ""].join(newline);
        match ConfigDoc::parse(source.as_bytes()) {
            Err(Error::Parse { what, message }) => {
                assert_eq!(what, "config.toml");
                assert!(message.contains("line 3"), "{message}");
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
    }
}

#[test]
fn invalid_utf8_is_a_config_parse_error() {
    match ConfigDoc::parse(b"model='ok'\n\xff") {
        Err(Error::Parse { what, message }) => {
            assert_eq!(what, "config.toml");
            assert!(message.contains("line 2"));
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn getters_traverse_every_table_style_and_strip_only_outer_decor() {
    let source = "# comment\nmodel = '中文' # trailing\nfeatures.fast_mode = true\ninline = { nested = { enabled = false }, array = [1, 2] }\n[implicit.child]\nx=42\n[[servers]]\nname='stay'\n";
    let document = doc(source);
    assert_eq!(document.get_repr("model"), Some("'中文'".to_owned()));
    assert_eq!(document.get_str("model"), Some("中文".to_owned()));
    assert_eq!(
        document.get_value("features.fast_mode"),
        Some(toml::Value::Boolean(true))
    );
    assert_eq!(
        document.get_value("inline.nested.enabled"),
        Some(toml::Value::Boolean(false))
    );
    assert_eq!(
        document.get_value("implicit.child.x"),
        Some(toml::Value::Integer(42))
    );
    assert_eq!(document.get_repr("inline.array"), Some("[1, 2]".to_owned()));
    for path in [
        "features",
        "implicit",
        "implicit.child",
        "inline",
        "inline.nested",
    ] {
        assert!(document.has_table(path), "{path}");
    }
    // Tables that are not inline values read as inline tables.
    for (path, repr) in [
        ("features", "{ fast_mode = true }"),
        ("implicit", "{ child = { x = 42 } }"),
        ("implicit.child", "{ x = 42 }"),
        ("inline.nested", "{ enabled = false }"),
    ] {
        assert_eq!(document.get_repr(path).as_deref(), Some(repr), "{path}");
    }
    for path in ["servers", "servers.name", "model.x", "missing"] {
        assert_eq!(document.get_repr(path), None, "{path}");
    }
    for path in ["model", "inline.array", "servers", "missing"] {
        assert!(!document.has_table(path), "{path}");
    }
    assert_eq!(document.get_str("features.fast_mode"), None);
    assert_eq!(document.get_str("missing"), None);
}

#[test]
fn get_repr_preserves_internal_multiline_comments_and_crlf() {
    let document =
        doc("array = [\r\n  1, # 中文\r\n  2,\r\n] # outer\r\ntext = '''\r\n多行\r\n'''\r\n");
    assert_eq!(
        document.get_repr("array"),
        Some("[\r\n  1, # 中文\r\n  2,\r\n]".to_owned())
    );
    assert_eq!(
        document.get_repr("text"),
        Some("'''\r\n多行\r\n'''".to_owned())
    );
}

#[test]
fn repr_helpers_compare_semantics_without_accepting_documents() {
    for (a, b) in [
        ("\"a\"", "'a'"),
        ("1", "1"),
        ("0x2a", "42"),
        ("{a=1}", "{ a = 1 }"),
        ("[1,2]", "[ 1, 2 ]"),
    ] {
        assert!(reprs_equal(a, b), "{a}, {b}");
    }
    assert!(!reprs_equal("1", "1.0"));
    assert!(!reprs_equal("'a'", "'b'"));
    assert!(reprs_equal("invalid", "invalid"));
    assert!(!reprs_equal("invalid", " invalid"));
    for invalid in ["", "invalid", "a=1", "true\na=1", "[section]\na=1"] {
        assert_eq!(parse_repr(invalid), None, "{invalid}");
    }
    assert_eq!(parse_repr("42"), Some(toml::Value::Integer(42)));
}

#[test]
fn basic_string_repr_escapes_controls_and_preserves_urls() {
    let url = "https://example.test/backend-api/";
    assert_eq!(string_repr(url), "\"https://example.test/backend-api/\"");
    let mut document = ConfigDoc::empty();
    document
        .set_repr("chatgpt_base_url", &string_repr(url))
        .unwrap();
    assert_eq!(document.get_str("chatgpt_base_url"), Some(url.to_owned()));
    let value = "中文 \"quote\" \\ \u{8}\t\n\u{c}\r\0\u{1b}\u{7f}";
    assert_eq!(
        string_repr(value),
        "\"中文 \\\"quote\\\" \\\\ \\b\\t\\n\\f\\r\\u0000\\u001B\\u007F\""
    );
    assert_eq!(
        parse_repr(&string_repr(value)),
        Some(toml::Value::String(value.to_owned()))
    );
    for ch in (0..=0x7f).filter_map(char::from_u32) {
        let value = ch.to_string();
        assert_eq!(
            parse_repr(&string_repr(&value)),
            Some(toml::Value::String(value))
        );
    }
}

#[test]
fn static_literals_use_basic_strings_and_booleans() {
    assert_eq!(
        literal_repr(TomlLiteral::Str("https://example.test/")),
        "\"https://example.test/\""
    );
    assert_eq!(literal_repr(TomlLiteral::Bool(true)), "true");
    assert_eq!(literal_repr(TomlLiteral::Bool(false)), "false");
}

#[test]
fn clone_edits_do_not_change_the_original() {
    let original = doc(REALISTIC);
    let mut copy = original.clone();
    copy.set_repr("model", "'new'").unwrap();
    assert!(copy.remove("model_provider"));
    assert_bytes(&original, REALISTIC);
    assert_ne!(copy.to_bytes(), original.to_bytes());
}
