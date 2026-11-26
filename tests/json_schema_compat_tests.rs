//! Schema versioning and compatibility checks for the JSON export (#319).
//!
//! `docs/schema/<MAJOR.MINOR>/perfratio-export.schema.json` is a frozen copy of the schema of
//! each published `schemaVersion`; `docs/schema/perfratio-export.schema.json` is the current one.
//! These tests check that
//! - the current schema is archived under its own version (so a MINOR bump must add a directory),
//! - a fresh export still validates against every older schema of the same MAJOR (consumers
//!   pinned to an older MINOR keep working), and
//! - the schema diff used for that contract flags renamed, removed and retyped fields.

mod common;
use common::perfratio;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const SCHEMA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/schema");
const FILE: &str = "perfratio-export.schema.json";

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn current() -> Value {
    read(&Path::new(SCHEMA_DIR).join(FILE))
}

fn version_of(schema: &Value) -> String {
    schema["properties"]["schemaVersion"]["const"]
        .as_str()
        .expect("schemaVersion const")
        .to_string()
}

fn parse(version: &str) -> (u32, u32) {
    let (major, minor) = version.split_once('.').expect("MAJOR.MINOR");
    (major.parse().unwrap(), minor.parse().unwrap())
}

/// All archived `(version, path)` pairs, oldest first.
fn archived() -> Vec<((u32, u32), PathBuf)> {
    let mut found: Vec<_> = std::fs::read_dir(SCHEMA_DIR)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (parse(&name), e.path().join(FILE))
        })
        .collect();
    found.sort();
    found
}

/// Differences between two schemas that break a consumer written for `old`:
/// a property that disappeared or changed type, an enum value that was removed, or a newly
/// required key. New optional properties are fine (MINOR).
fn breaking_changes(old: &Value, new: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(old, new, "#", &mut out);
    if let Some(defs) = old.get("$defs").and_then(Value::as_object) {
        for (name, def) in defs {
            match new.get("$defs").and_then(|d| d.get(name)) {
                Some(new_def) => walk(def, new_def, &format!("#/$defs/{name}"), &mut out),
                None => out.push(format!("#/$defs/{name}: definition removed")),
            }
        }
    }
    out
}

fn types(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn walk(old: &Value, new: &Value, at: &str, out: &mut Vec<String>) {
    // `schemaVersion` is expected to change on every MINOR bump
    let old_types = types(old);
    let new_types = types(new);
    if !old_types.is_empty() {
        for t in &old_types {
            let widened = t == "integer" && new_types.iter().any(|n| n == "number");
            if !new_types.contains(t) && !widened {
                out.push(format!("{at}: type `{t}` is no longer accepted"));
            }
        }
    }
    if let (Some(old_enum), Some(new_enum)) = (
        old.get("enum").and_then(Value::as_array),
        new.get("enum").and_then(Value::as_array),
    ) {
        for value in old_enum {
            if !new_enum.contains(value) {
                out.push(format!("{at}: enum value {value} removed"));
            }
        }
    }
    let old_required: Vec<&str> = old
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(new_required) = new.get("required").and_then(Value::as_array) {
        for key in new_required.iter().filter_map(Value::as_str) {
            if !old_required.contains(&key) {
                out.push(format!("{at}: `{key}` became required"));
            }
        }
    }
    if let Some(old_props) = old.get("properties").and_then(Value::as_object) {
        for (key, old_prop) in old_props {
            let child = format!("{at}/properties/{key}");
            match new.get("properties").and_then(|p| p.get(key)) {
                Some(new_prop) => {
                    if key != "schemaVersion" {
                        walk(old_prop, new_prop, &child, out);
                    }
                }
                None => out.push(format!("{child}: property removed or renamed")),
            }
        }
    }
    if let (Some(old_items), Some(new_items)) = (old.get("items"), new.get("items")) {
        walk(old_items, new_items, &format!("{at}/items"), out);
    }
}

fn export(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    read(&path)
}

#[test]
fn the_current_schema_is_archived_under_its_version() {
    let current = current();
    let version = version_of(&current);
    let archived = Path::new(SCHEMA_DIR).join(&version).join(FILE);
    assert!(
        archived.exists(),
        "schemaVersion is {version}: copy docs/schema/{FILE} to docs/schema/{version}/{FILE}"
    );
    assert_eq!(
        read(&archived),
        current,
        "docs/schema/{version}/{FILE} differs from docs/schema/{FILE}. An archived schema is \
         frozen: bump schemaVersion and add a new directory instead of editing it"
    );
}

#[test]
fn every_archived_schema_is_filed_under_its_own_version() {
    for ((major, minor), path) in archived() {
        assert_eq!(
            version_of(&read(&path)),
            format!("{major}.{minor}"),
            "{}",
            path.display()
        );
    }
}

#[test]
fn exports_validate_against_every_older_schema_of_the_same_major() {
    let current_version = parse(&version_of(&current()));
    let older: Vec<_> = archived()
        .into_iter()
        .filter(|((major, minor), _)| *major == current_version.0 && *minor < current_version.1)
        .collect();
    assert!(
        !older.is_empty(),
        "no archived schema older than {current_version:?}"
    );

    let documents = [
        export(&["--runs=3", "sleep 0.1"]),
        export(&["--runs=3", "--warmup=2", "sleep 0.1", "sleep 0.2"]),
        export(&["--runs=2", "--parameter-scan", "n", "1", "3", "sleep 0.{n}"]),
        export(&["--runs=3", "--concurrency=2", "sleep 0.1"]),
    ];
    for ((major, minor), path) in older {
        let mut schema = read(&path);
        // the version const is the one thing that must differ
        schema["properties"]["schemaVersion"] = json!({ "type": "string" });
        let validator = jsonschema::validator_for(&schema).unwrap();
        for document in &documents {
            let errors: Vec<String> = validator
                .iter_errors(document)
                .map(|e| format!("{} (at {})", e, e.instance_path()))
                .collect();
            assert!(
                errors.is_empty(),
                "a new export breaks consumers of schema {major}.{minor}: a MINOR bump may only \
                 add optional fields.\n{}",
                errors.join("\n")
            );
        }
    }
}

#[test]
fn the_current_schema_is_not_a_breaking_change_over_the_previous_minor() {
    let current = current();
    let (major, minor) = parse(&version_of(&current));
    let previous = archived()
        .into_iter()
        .rev()
        .find(|((m, n), _)| *m == major && *n < minor);
    let Some((_, path)) = previous else {
        panic!("no archived schema older than {major}.{minor}");
    };
    let problems = breaking_changes(&read(&path), &current);
    assert!(
        problems.is_empty(),
        "changes that need a MAJOR bump of schemaVersion:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_diff_flags_a_renamed_field() {
    let old = current();
    let mut renamed = old.clone();
    let props = renamed["$defs"]["summary"]["properties"]
        .as_object_mut()
        .unwrap();
    let (key, value) = {
        let key = props.keys().find(|k| *k == "mean").expect("mean").clone();
        (key.clone(), props.remove(&key).unwrap())
    };
    props.insert(format!("{key}_renamed"), value);
    let problems = breaking_changes(&old, &renamed);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("mean") && p.contains("renamed")),
        "{problems:?}"
    );
    assert!(breaking_changes(&old, &old).is_empty());
}

#[test]
fn the_diff_flags_retyped_and_newly_required_fields_but_allows_additions() {
    let old = json!({
        "type": "object",
        "required": ["a"],
        "properties": { "a": { "type": "number" }, "b": { "type": "string", "enum": ["x", "y"] } }
    });
    let mut added = old.clone();
    added["properties"]["c"] = json!({ "type": "array" });
    assert!(breaking_changes(&old, &added).is_empty());

    let mut retyped = old.clone();
    retyped["properties"]["a"]["type"] = json!("string");
    assert_eq!(breaking_changes(&old, &retyped).len(), 1);

    let mut required = old.clone();
    required["required"] = json!(["a", "c"]);
    assert_eq!(breaking_changes(&old, &required).len(), 1);

    let mut narrowed = old.clone();
    narrowed["properties"]["b"]["enum"] = json!(["x"]);
    assert_eq!(breaking_changes(&old, &narrowed).len(), 1);
}
