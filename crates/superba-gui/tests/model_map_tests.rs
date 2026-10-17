#![allow(missing_docs)]

//! Model mapping (Codex model name -> upstream model name) of a custom site: the
//! row parser behind the mapping editor. The editor moves to the Models page
//! (`Site::model_map`, "upstream name" column); these tests move with it.

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use superba_gui::pages::providers::form::{FormError, MapRow, parse_mapping};

fn row(from: &str, to: &str) -> MapRow {
    MapRow {
        from: from.into(),
        to: to.into(),
    }
}

#[test]
fn mapping_parser_ignores_blank_rows_and_trims_both_names() {
    assert_eq!(parse_mapping(&[]), Ok(BTreeMap::new()));
    assert_eq!(
        parse_mapping(&[
            row("", ""),
            row(" \t", "\n"),
            row(" gpt-5.5 ", " provider/gpt-5.5 ")
        ]),
        Ok(BTreeMap::from([(
            "gpt-5.5".into(),
            "provider/gpt-5.5".into()
        )]))
    );
}

#[test]
fn mapping_parser_rejects_either_half_of_an_incomplete_row() {
    for incomplete in [row("gpt-5.5", " \t"), row("\n", "provider/model")] {
        assert_eq!(
            parse_mapping(&[row("valid", "target"), incomplete]),
            Err(FormError::IncompleteMapping)
        );
    }
}

#[test]
fn mapping_parser_rejects_duplicate_trimmed_codex_names() {
    for target in ["first", "second"] {
        assert_eq!(
            parse_mapping(&[row("gpt-5.5", "first"), row(" gpt-5.5 ", target)]),
            Err(FormError::DuplicateMapping("gpt-5.5".into()))
        );
    }
}

#[test]
fn mapping_parser_allows_distinct_codex_names_to_share_a_target() {
    assert_eq!(
        parse_mapping(&[row("a", "shared"), row("b", "shared")]),
        Ok(BTreeMap::from([
            ("a".into(), "shared".into()),
            ("b".into(), "shared".into())
        ]))
    );
}
