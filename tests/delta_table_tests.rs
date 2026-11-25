//! `--delta-table` (#212)

mod common;
use common::hyperfine;

use predicates::prelude::*;

fn debug() -> assert_cmd::Command {
    let mut cmd = hyperfine();
    cmd.arg("--debug-mode");
    cmd
}

#[test]
fn prints_the_table_after_the_summary() {
    debug()
        .args(["--runs=3", "--delta-table", "sleep 0.1", "sleep 0.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Benchmark comparison (Reference: 'sleep 0.1')",
        ))
        .stdout(predicate::str::contains("Wall time"));
}

#[test]
fn absent_without_the_flag() {
    debug()
        .args(["--runs=3", "sleep 0.1", "sleep 0.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Benchmark comparison").not());
}

#[test]
fn single_command_has_nothing_to_compare() {
    debug()
        .args(["--runs=3", "--delta-table", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Benchmark comparison").not());
}

#[test]
fn markdown_export_gets_the_table_only_with_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let with = dir.path().join("with.md");
    let without = dir.path().join("without.md");
    debug()
        .arg("--runs=3")
        .arg("--delta-table")
        .arg("--export-markdown")
        .arg(&with)
        .args(["sleep 0.1", "sleep 0.2"])
        .assert()
        .success();
    debug()
        .arg("--runs=3")
        .arg("--export-markdown")
        .arg(&without)
        .args(["sleep 0.1", "sleep 0.2"])
        .assert()
        .success();
    let with = std::fs::read_to_string(with).unwrap();
    assert!(with.contains("### Delta: `sleep 0.2` vs reference `sleep 0.1`"));
    assert!(with.contains("| Wall time |"));
    assert!(!std::fs::read_to_string(without).unwrap().contains("Delta"));
}
