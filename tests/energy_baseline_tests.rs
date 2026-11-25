//! CLI-level checks for `--energy-baseline` (debug mode, no RAPL needed).

mod common;
use common::hyperfine;

use predicates::prelude::*;

#[test]
fn energy_baseline_requires_energy() {
    hyperfine()
        .args([
            "--debug-mode",
            "--runs=2",
            "--energy-baseline=1s",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--energy"));
}

#[test]
fn energy_baseline_rejects_an_invalid_duration() {
    hyperfine()
        .args([
            "--debug-mode",
            "--runs=2",
            "--energy",
            "--energy-baseline=-1s",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Invalid duration '-1s' for '--energy-baseline'",
        ));
}

#[test]
fn energy_baseline_zero_disables_the_baseline() {
    hyperfine()
        .args([
            "--debug-mode",
            "--runs=2",
            "--energy",
            "--energy-baseline=0",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("'--energy-baseline' was requested").not());
}

#[test]
fn energy_baseline_warns_when_idle_power_is_unavailable() {
    // On hosts with readable RAPL the idle measurement succeeds and no warning is printed.
    if std::path::Path::new("/sys/class/powercap/intel-rapl:0/energy_uj").exists()
        && std::fs::read_to_string("/sys/class/powercap/intel-rapl:0/energy_uj").is_ok()
    {
        return;
    }
    hyperfine()
        .args([
            "--debug-mode",
            "--runs=2",
            "--energy",
            "--energy-baseline=10ms",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "'--energy-baseline' was requested but the idle power could not be measured",
        ));
}
