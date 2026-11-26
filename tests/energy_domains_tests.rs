//! `--energy-domains` (#59): debug mode uses a mock sampler, so these run everywhere.
//! The last test needs bare-metal Linux with readable RAPL and is `#[ignore]`d.

mod common;
use common::perfratio;

use predicates::prelude::*;

#[test]
fn energy_domains_requires_energy() {
    perfratio()
        .args(["--debug-mode", "--runs=2", "--energy-domains", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--energy"));
}

#[test]
fn energy_domains_are_exported_per_run_and_not_added_to_the_total() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("r.json");
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--energy",
            "--energy-domains",
            "--export-json",
        ])
        .arg(&out)
        .arg("sleep 0.1")
        .assert()
        .success()
        .stdout(predicate::str::contains("Domains (mean):"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    let r = &json["results"][0];
    let d = &r["energy_domains_joules"];
    assert_eq!(d["package"].as_array().unwrap().len(), 3);
    assert_eq!(d["core"][0], 0.75);
    assert_eq!(d["dram"][2], 0.25);
    // The total is the package energy, not the sum of the overlapping domains
    assert_eq!(r["energy_joules"][0], 1.25);
    assert_eq!(r["mean_energy_joules"], 1.25);
}

#[test]
fn energy_domains_are_absent_without_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("r.json");
    perfratio()
        .args(["--debug-mode", "--runs=2", "--energy", "--export-json"])
        .arg(&out)
        .arg("sleep 0.1")
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    assert!(json["results"][0].get("energy_domains_joules").is_none());
}

/// Bare metal only: `sleep 0.2` must report package energy and a breakdown whose
/// sub-domains do not exceed the package. Run with `cargo test -- --ignored`.
#[test]
#[ignore = "needs bare-metal Linux with readable /sys/class/powercap/intel-rapl*/energy_uj"]
#[cfg(target_os = "linux")]
fn bare_metal_rapl_domains() {
    let readable =
        std::fs::read_to_string("/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj").is_ok();
    assert!(readable, "energy_uj is not readable on this machine");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("r.json");
    perfratio()
        .args([
            "--runs=5",
            "--energy",
            "--energy-domains",
            "--energy-backend=sysfs",
            "--export-json",
        ])
        .arg(&out)
        .arg("sleep 0.2")
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    let r = &json["results"][0];
    assert!(r["mean_energy_joules"].as_f64().unwrap() > 0.0);
    let d = r["energy_domains_joules"].as_object().expect("domains");
    let pkg = d["package"].as_array().unwrap();
    for (i, p) in pkg.iter().enumerate() {
        let p = p.as_f64().unwrap();
        // 5% slack for counters read at slightly different instants
        for sub in ["core", "uncore", "dram"] {
            if let Some(v) = d.get(sub) {
                assert!(
                    v[i].as_f64().unwrap() <= p * 1.05 + 1e-3,
                    "{sub} above package"
                );
            }
        }
    }
}
