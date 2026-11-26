//! `--resource-usage` on Windows: counters without an equivalent are unavailable, not 0.

#![cfg(windows)]

mod common;
use common::perfratio;

#[test]
fn unavailable_counters_are_omitted_and_available_ones_exported() {
    let tempdir = tempfile::tempdir().unwrap();
    let export = tempdir.path().join("out.json");
    perfratio()
        .args(["-N", "--runs=2", "--resource-usage", "--style=basic"])
        .arg("--export-json")
        .arg(&export)
        .arg("cmd /C exit 0")
        .assert()
        .success()
        .stdout(predicates::str::contains("Resources (mean):"))
        .stdout(predicates::str::contains("n/a vol"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&export).unwrap()).unwrap();
    let resources = &json["results"][0]["resources"];
    for key in ["minor_faults", "block_input_ops", "block_output_ops"] {
        assert_eq!(resources[key].as_array().unwrap().len(), 2, "{key}");
    }
    for key in [
        "voluntary_ctx_switches",
        "involuntary_ctx_switches",
        "major_faults",
    ] {
        assert!(resources.get(key).is_none(), "{key} must be omitted");
    }
}
