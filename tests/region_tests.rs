//! `--region` and `--metric region:<name>` (#310, `docs/specs/region-protocol.md`).
//!
//! The `--debug-mode` tests run everywhere; the others use a shell script as a
//! fake SDK that writes the wire format to `$PERFRATIO_REGION_FILE`, so they
//! are Unix only (the real SDKs are tested in `sdk_tests.rs`).

mod common;
use common::perfratio;

use serde_json::Value;

fn json_of(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(args)
        .args(["--export-json", path.to_str().unwrap()])
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn debug_mode_reports_region_next_to_whole_process_time() {
    let json = json_of(&["--debug-mode", "--runs=3", "--region=work", "sleep 0.2"]);
    assert_eq!(json["schemaVersion"], "1.42");
    assert_eq!(json["perfratio"]["region_protocol"], 1);
    assert_eq!(json["perfratio"]["measurements"]["region"], "measured");
    let result = &json["results"][0];
    // the whole process keeps its own time
    assert_eq!(result["times"], serde_json::json!([0.2, 0.2, 0.2]));
    let region = &result["regions"][0];
    assert_eq!(region["name"], "work");
    assert_eq!(region["status"], "measured");
    // half of the simulated 0.2 s run
    for time in region["times"].as_array().unwrap() {
        assert!((time.as_f64().unwrap() - 0.1).abs() < 1e-9, "{time}");
    }
    assert_eq!(region["count_per_run"], serde_json::json!([1, 1, 1]));
    assert!((region["mean"].as_f64().unwrap() - 0.1).abs() < 1e-9);
    assert!(result.get("custom_metrics").is_none());
}

#[test]
fn region_has_a_metric_descriptor_with_region_scope() {
    let json = json_of(&["--debug-mode", "--runs=2", "--region=work", "sleep 0.2"]);
    let metrics = json["perfratio"]["metrics"].as_array().unwrap();
    let descriptor = metrics.iter().find(|m| m["id"] == "region.work").unwrap();
    assert_eq!(descriptor["unit"], "seconds");
    assert_eq!(descriptor["scope"], "region");
    assert_eq!(descriptor["direction"], "lower_is_better");
    assert_eq!(descriptor["status"], "available");
    assert_eq!(descriptor["fields"], serde_json::json!(["regions"]));
    // whole-process time keeps its own process scope
    let wall = metrics.iter().find(|m| m["id"] == "time.wall").unwrap();
    assert_eq!(wall["scope"], "process_tree");
}

#[test]
fn bare_region_is_the_default_region() {
    let json = json_of(&["--debug-mode", "--runs=2", "--region", "sleep 0.2"]);
    assert_eq!(json["results"][0]["regions"][0]["name"], "default");
}

#[test]
fn without_region_nothing_is_exported() {
    let json = json_of(&["--debug-mode", "--runs=2", "sleep 0.2"]);
    assert!(json["results"][0].get("regions").is_none());
    assert!(json["perfratio"].get("region_protocol").is_none());
    assert_eq!(json["perfratio"]["measurements"]["region"], "not_requested");
}

#[test]
fn metric_region_ranks_on_the_region_and_implies_it() {
    let json = json_of(&[
        "--debug-mode",
        "--runs=2",
        "--metric=region:work",
        "sleep 0.4",
        "sleep 0.2",
    ]);
    assert_eq!(json["metric"], "region:work");
    let slow = &json["results"][0];
    let fast = &json["results"][1];
    assert_eq!(fast["relative_speed"], 1.0);
    assert_eq!(slow["relative_speed"], 2.0);
    assert_eq!(slow["primary_metric"]["metric"], "region:work");
    assert!((slow["primary_metric"]["mean"].as_f64().unwrap() - 0.2).abs() < 1e-9);
}

#[test]
fn invalid_region_names_are_rejected() {
    perfratio()
        .args(["--debug-mode", "--region=a-b", "sleep 0.1"])
        .assert()
        .failure();
    perfratio()
        .args(["--debug-mode", "--metric=region:", "sleep 0.1"])
        .assert()
        .failure();
}

#[test]
fn region_conflicts_with_concurrency() {
    perfratio()
        .args(["--debug-mode", "--region", "--concurrency=2", "sleep 0.1"])
        .assert()
        .failure();
}

#[cfg(unix)]
mod fake_sdk {
    use super::*;
    use predicates::prelude::*;

    /// A program that reports `work` twice (1 ms + 2 ms) and `outer` once.
    const SCRIPT: &str = "[ -z \"$PERFRATIO_REGION_FILE\" ] || printf 'hello 1 fake/1\\nstart outer 100\\nstart work 1000\\n\
         stop work 1001000\\nstart work 5000000\\nstop work 7000000\\nstop outer 9000000\\n' \
         >> \"$PERFRATIO_REGION_FILE\"";

    #[test]
    fn sums_pairs_and_reports_the_requested_regions() {
        let json = json_of(&[
            "--runs=3",
            "--warmup=1",
            "--region=work",
            "--region=outer",
            SCRIPT,
        ]);
        let regions = json["results"][0]["regions"].as_array().unwrap();
        let work = regions.iter().find(|r| r["name"] == "work").unwrap();
        let outer = regions.iter().find(|r| r["name"] == "outer").unwrap();
        // 1 ms + 2 ms, three runs, no warmup run leaks in
        assert_eq!(work["times"], serde_json::json!([0.003, 0.003, 0.003]));
        assert_eq!(work["count_per_run"], serde_json::json!([2, 2, 2]));
        assert_eq!(work["sdk"], "fake/1");
        assert_eq!(work["min"], 0.003);
        assert_eq!(outer["count_per_run"], serde_json::json!([1, 1, 1]));
        assert!((outer["mean"].as_f64().unwrap() - 0.0089999).abs() < 1e-6);
        // the whole-process series stays next to the region series
        assert_eq!(json["results"][0]["times"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn region_metric_uses_the_region_time() {
        let json = json_of(&["--runs=2", "--metric=region:work", SCRIPT]);
        assert_eq!(
            json["results"][0]["primary_metric"]["samples"],
            serde_json::json!([0.003, 0.003])
        );
    }

    #[test]
    fn uninstrumented_command_fails_with_an_actionable_error() {
        perfratio()
            .args(["--runs=2", "--region", "true"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("reported no region"))
            .stderr(predicate::str::contains("PERFRATIO_REGION_FILE"));
    }

    #[test]
    fn a_region_the_program_never_reports_is_an_error() {
        perfratio()
            .args(["--runs=2", "--region=typo", SCRIPT])
            .assert()
            .failure()
            .stderr(predicate::str::contains("never reported the region 'typo'"));
    }

    #[test]
    fn protocol_version_mismatch_is_detected() {
        perfratio()
            .args([
                "--runs=1",
                "--region",
                "[ -z \"$PERFRATIO_REGION_FILE\" ] || printf 'hello 2 future\\n' >> \"$PERFRATIO_REGION_FILE\"",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("protocol mismatch"))
            .stderr(predicate::str::contains("protocol 2"));
    }

    #[test]
    fn same_name_overlap_is_a_protocol_error() {
        perfratio()
            .args([
                "--runs=1",
                "--region",
                "[ -z \"$PERFRATIO_REGION_FILE\" ] || printf 'hello 1\\nstart default 1\\nstart default 2\\n' >> \"$PERFRATIO_REGION_FILE\"",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("started twice"));
    }

    #[test]
    fn a_run_missing_the_region_is_null_not_zero() {
        // the second run (the marker file exists then) reports nothing for `work`
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("seen");
        let script = format!(
            "[ -z \"$PERFRATIO_REGION_FILE\" ] || if [ -e '{m}' ]; then printf 'hello 1\\n' >> \"$PERFRATIO_REGION_FILE\"; \
             else : > '{m}'; printf 'hello 1\\nstart work 0\\nstop work 4000000\\n' \
             >> \"$PERFRATIO_REGION_FILE\"; fi",
            m = marker.display()
        );
        let json = json_of(&["--runs=3", "--region=work", &script]);
        let region = &json["results"][0]["regions"][0];
        assert_eq!(region["status"], "region_missing");
        assert_eq!(region["times"], serde_json::json!([0.004, null, null]));
        assert_eq!(region["count_per_run"], serde_json::json!([1, null, null]));
        assert_eq!(region["mean"], 0.004);
        // ... and the primary metric cannot rank on a region that runs lack
        perfratio()
            .args([
                "--runs=3",
                "--metric=region:work",
                &script.replace(
                    &marker.display().to_string(),
                    &format!("{}2", marker.display()),
                ),
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("every run"));
    }
}

#[test]
fn debug_mode_region_scope_reports_scoped_metrics_with_scope_region() {
    let json = json_of(&[
        "--debug-mode",
        "--runs=3",
        "--region=work",
        "--region-scope",
        "--energy",
        "sleep 0.2",
    ]);
    let result = &json["results"][0];
    let metrics = result["regions"][0]["metrics"].as_array().unwrap();
    let names: Vec<&str> = metrics
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["cpu_user", "cpu_system", "energy"]);
    for metric in metrics {
        assert_eq!(metric["scope"], "region");
        assert_eq!(metric["values"].as_array().unwrap().len(), 3);
    }
    // region time 0.1 s: 60% user, 20% system, 10 J/s
    let mean = |i: usize| metrics[i]["mean"].as_f64().unwrap();
    assert!((mean(0) - 0.06).abs() < 1e-9);
    assert!((mean(1) - 0.02).abs() < 1e-9);
    assert!((mean(2) - 1.0).abs() < 1e-9);
    assert_eq!(metrics[2]["unit"], "J");
    // process-wide values are unchanged and carry no scoped series
    // (with energy, only the derived energy-delay products are series of their own)
    let series: Vec<&String> = result
        .get("custom_metrics")
        .and_then(|m| m.as_object())
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    assert!(
        series.iter().all(|k| *k == "edp" || *k == "ed2p"),
        "{series:?}"
    );
    assert!(result["user"].is_number());
}

#[test]
fn region_scope_is_absent_without_the_flag() {
    let json = json_of(&["--debug-mode", "--runs=2", "--region=work", "sleep 0.2"]);
    assert!(json["results"][0]["regions"][0].get("metrics").is_none());
}

#[test]
fn region_scope_needs_a_region() {
    perfratio()
        .args(["--debug-mode", "--region-scope", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--region-scope"));
}
