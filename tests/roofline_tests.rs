//! `--roofline` (#379). `--debug-mode` simulates the counters: per second of
//! simulated time 1.2M scalar, 100k 128-bit and 32.4M 256-bit FP counts and
//! 1M cache misses. In double precision that is 131 MFLOP/s over 64 MB/s.

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export(args: &[&str]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&json)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap()
}

fn close(value: &serde_json::Value, expected: f64) {
    let v = value
        .as_f64()
        .unwrap_or_else(|| panic!("not a number: {value}"));
    assert!(
        (v - expected).abs() < 1e-9 * expected.abs().max(1.0),
        "{v} vs {expected}"
    );
}

#[test]
fn configured_peaks_place_the_command() {
    let json = export(&[
        "--runs=3",
        "--roofline",
        "--roofline-peaks=1,0.1",
        "sleep 0.5",
    ]);
    let r = &json["roofline"];
    assert_eq!(r["precision"], "double");
    assert_eq!(r["bytes_source"], "llc_misses");
    assert_eq!(r["cache_line_bytes"], 64.0);
    assert_eq!(r["peaks"]["source"], "configured");
    close(&r["ridge_point"], 10.0);
    let p = &r["results"][0];
    assert_eq!(p["status"], "ok");
    assert_eq!(p["runs_used"], 3);
    close(&p["arithmetic_intensity"], 131.0 / 64.0);
    close(&p["gflops"], 0.131);
    close(&p["gbytes_per_s"], 0.064);
    assert_eq!(p["bound"], "memory");
    close(&p["attainable_gflops"], 0.1 * 131.0 / 64.0);
    close(&p["efficiency"], 0.131 / (0.1 * 131.0 / 64.0));
    // the counters it needs are enabled
    assert_eq!(json["perfratio"]["measurements"]["simd"], "measured");
    assert!(json["results"][0]["counters"]["cache-misses"].is_array());
}

#[test]
fn single_precision_doubles_the_packed_flops() {
    let json = export(&[
        "--runs=2",
        "--roofline",
        "--roofline-precision=single",
        "--roofline-peaks=1,0.1",
        "sleep 0.5",
    ]);
    assert_eq!(json["roofline"]["precision"], "single");
    // 1.2M + 100k*4 + 32.4M*8 = 260.8M per second
    close(&json["roofline"]["results"][0]["gflops"], 0.2608);
}

#[test]
fn observed_peaks_need_two_commands() {
    let one = export(&["--runs=2", "--roofline", "sleep 0.5"]);
    assert!(one["roofline"].get("peaks").is_none());
    assert!(one["roofline"]["peaks_note"]
        .as_str()
        .unwrap()
        .contains("--roofline-peaks"));
    assert!(one["roofline"]["results"][0].get("bound").is_none());

    let two = export(&["--runs=2", "--roofline", "sleep 0.5", "sleep 0.25"]);
    assert_eq!(two["roofline"]["peaks"]["source"], "observed");
    assert_eq!(two["roofline"]["results"].as_array().unwrap().len(), 2);
}

#[test]
fn terminal_block() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--roofline",
            "--roofline-peaks=1,0.1",
            "sleep 0.5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Roofline (double precision"))
        .stdout(predicate::str::contains("AI 2.05 FLOP/B"))
        .stdout(predicate::str::contains(
            "memory-bound, 64% of the attainable 0.20 GFLOP/s",
        ))
        .stdout(predicate::str::contains(
            "Peaks (configured): 1.00 GFLOP/s, 0.10 GB/s; ridge point 10.0 FLOP/B",
        ));
}

#[test]
fn flags_are_validated() {
    for bad in ["48", "0,1", "x,y", "1,2,3"] {
        perfratio()
            .args([
                "--debug-mode",
                "--roofline",
                &format!("--roofline-peaks={bad}"),
                "true",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("--roofline-peaks"));
    }
    perfratio()
        .args(["--debug-mode", "--roofline-peaks=1,2", "true"])
        .assert()
        .failure();
    perfratio()
        .args([
            "--debug-mode",
            "--roofline-precision=half",
            "--roofline",
            "true",
        ])
        .assert()
        .failure();
}

#[test]
fn without_the_flag_there_is_no_roofline() {
    let json = export(&["--runs=2", "--simd", "sleep 0.1"]);
    assert!(json.get("roofline").is_none());
}
