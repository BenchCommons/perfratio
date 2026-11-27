//! Keeps the docs in step with the JSON export `SCHEMA_VERSION` (#366).
//!
//! Docs state the current version with an explicit phrase: "currently `"X.Y"`", "current version
//! is `"X.Y"`" or "current `schemaVersion` is `"X.Y"`". Only those phrases are checked, so
//! historical mentions such as "added in 1.12" are free text.

use regex::Regex;
use std::path::Path;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// Docs that must state the current version at least once.
const REQUIRED: &[&str] = &[
    "README.md",
    "docs/CROSS_LANGUAGE.md",
    "docs/MIGRATION.md",
    "doc/understanding-output.md",
];

fn read(rel: &str) -> String {
    std::fs::read_to_string(Path::new(ROOT).join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// `SCHEMA_VERSION` is private to the crate, so read it from its source line.
fn schema_version() -> String {
    let src = read("src/export/json.rs");
    Regex::new(r#"pub const SCHEMA_VERSION: &str = "(\d+\.\d+)";"#)
        .unwrap()
        .captures(&src)
        .expect("SCHEMA_VERSION constant")[1]
        .to_string()
}

fn current_mentions(text: &str) -> Vec<String> {
    Regex::new(r#"(?i)\bcurrent(?:ly)?(?:\s+(?:schemaVersion|`schemaVersion`|version))?(?:\s+is)?\s+`"?(\d+\.\d+)"?`"#)
        .unwrap()
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .collect()
}

fn markdown_files() -> Vec<String> {
    let mut out = vec!["README.md".to_string()];
    for dir in ["doc", "docs"] {
        for entry in std::fs::read_dir(Path::new(ROOT).join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "md") {
                out.push(format!(
                    "{dir}/{}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    out
}

#[test]
fn docs_state_the_current_schema_version() {
    let version = schema_version();
    for rel in markdown_files() {
        for found in current_mentions(&read(&rel)) {
            assert_eq!(
                found, version,
                "{rel} says the current schemaVersion is {found}, but SCHEMA_VERSION is {version}"
            );
        }
    }
    for rel in REQUIRED {
        assert!(
            !current_mentions(&read(rel)).is_empty(),
            "{rel} must state the current schemaVersion, e.g. currently `\"{version}\"`"
        );
    }
}

#[test]
fn historical_mentions_are_ignored() {
    assert!(current_mentions("added in 1.12, later `\"1.13\"`").is_empty());
    assert_eq!(
        current_mentions("The current version is `\"1.32\"`."),
        ["1.32"]
    );
    assert_eq!(current_mentions("(currently `\"1.32\"`)"), ["1.32"]);
}

#[test]
fn schema_is_archived_under_the_current_version() {
    let version = schema_version();
    let dir = Path::new(ROOT).join("docs/schema");
    let archived = dir.join(&version).join("perfratio-export.schema.json");
    assert!(archived.is_file(), "missing {}", archived.display());
    assert_eq!(
        std::fs::read(&archived).unwrap(),
        std::fs::read(dir.join("perfratio-export.schema.json")).unwrap(),
        "docs/schema/{version}/ differs from docs/schema/perfratio-export.schema.json"
    );
}
