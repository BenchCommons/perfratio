//! The man page `doc/perfratio.1` must document every long flag of `--help`.

mod common;
use common::perfratio;

use std::collections::BTreeSet;

/// Long flags declared in the help text: option lines (`  -w, --warmup <NUM>`)
/// and `[alias: --before]` lines. Flags merely mentioned in prose are skipped.
fn declared_flags(help: &str) -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    for line in help.lines() {
        let trimmed = line.trim_start();
        // Option lines are indented by 2 (short flag) or 6 (long only); wrapped prose by 10.
        let indent = line.len() - trimmed.len();
        let is_option_line = (indent == 2 || indent == 6) && trimmed.starts_with('-');
        let alias_list = trimmed
            .strip_prefix("[alias: ")
            .or_else(|| trimmed.strip_prefix("[aliases: "));
        let candidates: Vec<&str> = if is_option_line {
            trimmed.split_whitespace().take(2).collect()
        } else if let Some(rest) = alias_list {
            rest.trim_end_matches(']').split(", ").collect()
        } else {
            continue;
        };
        for word in candidates {
            let name = word.trim_end_matches(',').trim_start_matches("--");
            if word.starts_with("--") && !name.is_empty() {
                let name = name.split(['=', '[', '<']).next().unwrap();
                flags.insert(name.to_string());
            }
        }
    }
    flags
}

#[test]
fn man_page_documents_every_long_flag() {
    let help = perfratio().arg("--help").output().unwrap().stdout;
    let help = String::from_utf8(help).unwrap();
    let flags = declared_flags(&help);
    assert!(flags.len() > 60, "flag parsing broke: {flags:?}");

    let man =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/doc/perfratio.1")).unwrap();
    let missing: Vec<String> = flags
        .iter()
        .filter(|flag| {
            // roff spelling: every hyphen is written `\-`, followed by a non-name character.
            let needle = format!("\\-\\-{}", flag.replace('-', "\\-"));
            !man.match_indices(&needle).any(|(i, _)| {
                let next = man[i + needle.len()..].chars().next();
                !matches!(next, Some(c) if c.is_ascii_alphanumeric() || c == '\\' && man[i + needle.len()..].starts_with("\\-"))
            })
        })
        .map(|flag| format!("--{flag}"))
        .collect();
    assert!(
        missing.is_empty(),
        "doc/perfratio.1 does not document: {missing:?}"
    );
}
