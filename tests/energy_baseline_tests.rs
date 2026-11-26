//! CLI-level checks for `--energy-baseline` (debug mode, no RAPL needed).

mod common;
use common::perfratio;

use predicates::prelude::*;

#[test]
fn energy_baseline_requires_energy() {
    perfratio()
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
    perfratio()
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
    perfratio()
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
    perfratio()
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

/// Bare metal only: the net energy of `sleep 0.2` is about zero up to the noise of the
/// idle estimate (#59). A sleeping process does not use more power than the idle machine,
/// so the gross energy is almost all idle power. Run with `cargo test -- --ignored`.
#[test]
#[ignore = "needs bare-metal Linux with readable /sys/class/powercap/intel-rapl*/energy_uj"]
#[cfg(target_os = "linux")]
fn bare_metal_net_energy_of_sleep_is_about_zero() {
    let readable =
        std::fs::read_to_string("/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj").is_ok();
    assert!(readable, "energy_uj is not readable on this machine");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("r.json");
    perfratio()
        .args([
            "--runs=10",
            "--energy",
            "--energy-backend=sysfs",
            "--energy-baseline=2s",
            "--export-json",
        ])
        .arg(&out)
        .arg("sleep 0.2")
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    let r = &json["results"][0];
    let gross = r["mean_energy_joules"].as_f64().unwrap();
    let net = r["mean_net_energy_joules"].as_f64().unwrap();
    let idle = r["idle_watts"].as_f64().unwrap();
    assert!(gross > 0.0 && idle > 0.0, "gross {gross} J, idle {idle} W");
    assert!(net >= 0.0);
    // The idle estimate has noise (other processes, frequency changes): allow 35% of the
    // gross energy plus 0.1 J. A net of the size of the gross energy means the baseline
    // was not subtracted.
    assert!(
        net <= 0.35 * gross + 0.1,
        "net {net} J is not about zero (gross {gross} J, idle {idle} W)"
    );
}
