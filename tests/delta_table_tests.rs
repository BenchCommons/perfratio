//! `--delta-table` (#212)

mod common;
use common::perfratio;

use predicates::prelude::*;

fn debug() -> assert_cmd::Command {
    let mut cmd = perfratio();
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

/// A failing command. Run with `-N` (no shell) so that no shell spawning time,
/// which is noisy under load, is subtracted from the short sleeps.
#[cfg(unix)]
const FAILING: &str = "sh -c 'sleep 0.1; exit 1'";

/// `--filter-failed` applies to the terminal table and the Markdown export alike
#[cfg(unix)]
#[test]
fn filter_failed_applies_to_terminal_and_markdown() {
    let dir = tempfile::tempdir().unwrap();
    let md = dir.path().join("out.md");
    perfratio()
        .arg("--runs=3")
        .arg("-N")
        .arg("--ignore-failure")
        .arg("--filter-failed")
        .arg("--delta-table")
        .arg("--export-markdown")
        .arg(&md)
        .args(["sleep 0.05", FAILING, "sleep 0.15"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Benchmark comparison (Reference: 'sleep 0.05')",
        ))
        .stdout(predicate::str::contains("'sh -c 'sleep 0.1; exit 1'' (mean").not());
    let content = std::fs::read_to_string(&md).unwrap();
    assert!(content.contains("### Delta: `sleep 0.15` vs reference `sleep 0.05`"));
    assert!(!content.contains("exit 1"), "{content}");
}
