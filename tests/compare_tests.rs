//! Tests for --compare / --fail-if-regressed / --export-diff-markdown.

mod common;
use common::perfratio;

use predicates::prelude::*;
use std::path::{Path, PathBuf};

/// Export a baseline where `app` takes 100 ms and `other` 50 ms.
fn baseline(dir: &Path) -> PathBuf {
    let path = dir.join("baseline.json");
    perfratio()
        .args(["--debug-mode", "--runs=5", "--export-json"])
        .arg(&path)
        .args(["-n", "app", "sleep 0.1", "-n", "other", "sleep 0.05"])
        .assert()
        .success();
    path
}

#[test]
fn unchanged_results_pass() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    perfratio()
        .args([
            "--debug-mode",
            "--runs=5",
            "--fail-if-regressed=5%",
            "--compare",
        ])
        .arg(&base)
        .args(["-n", "app", "sleep 0.1", "-n", "other", "sleep 0.05"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Comparison with"))
        .stdout(predicate::str::is_match(r"app .* \+0\.0% +p = 1\.000 +~").unwrap());
}

#[test]
fn a_significant_slowdown_fails_with_exit_code_3() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let diff = dir.path().join("diff.md");
    perfratio()
        .args(["--debug-mode", "--runs=5", "--fail-if-regressed=5%"])
        .arg("--compare")
        .arg(&base)
        .arg("--export-diff-markdown")
        .arg(&diff)
        .args(["-n", "app", "sleep 0.2", "-n", "added", "sleep 0.05"])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("+100.0% ▲"))
        .stdout(predicate::str::contains("REGRESSION"))
        .stderr(predicate::str::contains("1 benchmark(s) regressed"));

    let markdown = std::fs::read_to_string(&diff).unwrap();
    assert!(
        markdown.contains(
            "| `app` | 100.0 ms ± 0.0 | 200.0 ms ± 0.0 | +100.0% ▲ | p < 0.001 | **REGRESSION** |"
        ),
        "{markdown}"
    );
    assert!(
        markdown.contains("| `added` |  |  |  |  | new |"),
        "{markdown}"
    );
    assert!(
        markdown.contains("| `other` |  |  |  |  | removed |"),
        "{markdown}"
    );
}

#[test]
fn a_slowdown_below_the_threshold_passes() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    perfratio()
        .args([
            "--debug-mode",
            "--runs=5",
            "--fail-if-regressed=1000%",
            "--compare",
        ])
        .arg(&base)
        .args(["-n", "app", "sleep 0.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("slower"));
}

#[test]
fn without_a_threshold_it_only_reports() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    perfratio()
        .args(["--debug-mode", "--runs=5", "--compare"])
        .arg(&base)
        .args(["-n", "app", "sleep 0.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("slower"));
}

#[test]
fn invalid_inputs_fail_before_benchmarking() {
    perfratio()
        .args([
            "--debug-mode",
            "--compare",
            "does-not-exist.json",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does-not-exist.json"))
        .stdout(predicate::str::contains("Benchmark 1").not());

    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    perfratio()
        .args(["--debug-mode", "--fail-if-regressed=fast", "--compare"])
        .arg(&base)
        .arg("sleep 0.1")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid threshold 'fast'"))
        .stdout(predicate::str::contains("Benchmark 1").not());

    perfratio()
        .args(["--fail-if-regressed=5%", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--compare"));
}

/// `--export-diff-markdown` uses the atomic export writer: a symlink at the
/// target is replaced, and the file it pointed to is left untouched.
#[cfg(unix)]
#[test]
fn export_diff_markdown_replaces_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, "keep me").unwrap();
    let diff = dir.path().join("diff.md");
    std::os::unix::fs::symlink(&victim, &diff).unwrap();
    perfratio()
        .args(["--debug-mode", "--runs=5", "--compare"])
        .arg(&base)
        .arg("--export-diff-markdown")
        .arg(&diff)
        .args(["-n", "app", "sleep 0.1", "-n", "other", "sleep 0.05"])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
    assert!(!diff.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(std::fs::read_to_string(&diff).unwrap().contains("`app`"));
}
