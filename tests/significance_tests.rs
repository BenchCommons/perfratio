//! Significance markers in the summary and the JSON export (#215).

mod common;
use common::perfratio;

use predicates::prelude::*;

fn perfratio_debug() -> assert_cmd::Command {
    let mut cmd = perfratio();
    cmd.arg("--debug-mode");
    cmd
}

#[test]
fn plain_markers_for_non_tty_output() {
    // stdout is a pipe here: no symbols, plain-text markers.
    perfratio_debug()
        .arg("sleep 1")
        .arg("sleep 2")
        .arg("--sort=mean-time")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("[-] significant regression (p < 0.001)")
                .and(predicate::str::contains("⚡").not())
                .and(predicate::str::contains("🔻").not()),
        );
}

#[test]
fn no_color_env_gives_plain_markers() {
    perfratio_debug()
        .env("NO_COLOR", "1")
        .arg("sleep 1")
        .arg("sleep 2")
        .assert()
        .success()
        .stdout(predicate::str::contains("[-] significant regression"));
}

#[test]
fn reference_faster_than_item_is_a_speedup() {
    perfratio_debug()
        .arg("--reference=sleep 2")
        .arg("sleep 1")
        .arg("--sort=command")
        .assert()
        .success()
        .stdout(predicate::str::contains("[+] significant speedup"));
}

#[test]
fn json_export_has_significance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio_debug()
        .arg("sleep 1")
        .arg("sleep 2")
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let results = json["results"].as_array().unwrap();
    // the reference (fastest) has no significance of its own
    assert!(results[0].get("significance").is_none());
    assert_eq!(results[1]["significance"], "significant_regression");
    assert!(results[1]["p_value"].as_f64().unwrap() < 0.001);
}
