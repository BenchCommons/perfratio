//! Software perf events (#220) and the effective frequency (#222) on a real
//! kernel. `PERF_TYPE_SOFTWARE` events work without a PMU, but opening them in
//! kernel scope needs `perf_event_paranoid <= 1` or `CAP_PERFMON`; where that
//! is denied the tests skip. The frequency is only checked when the hardware
//! `cycles` event could be opened too.
#![cfg(target_os = "linux")]

mod common;
use common::perfratio;

const SPIN: &str = "sh -c 'i=0; while [ $i -lt 30000 ]; do i=$((i+1)); done'";

fn run_with(list: &str) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=3", "--warmup=0"])
        .arg(format!("--counters={list}"))
        .arg("--export-json")
        .arg(&json)
        .arg(SPIN)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap()
}

#[test]
fn software_events_count_the_child() {
    let json = run_with("task-clock,page-faults,context-switches,cpu-migrations");
    let status = json["perfratio"]["measurements"]["counters"]
        .as_str()
        .unwrap();
    if status != "measured" {
        eprintln!(
            "skipped: software events not opened ({status}, {:?})",
            json["perfratio"]["measurements"]["counters_unavailable_reason"]
        );
        return;
    }
    let r = &json["results"][0];
    for run in r["counters"]["task-clock"].as_array().unwrap() {
        // a shell loop of 30000 iterations takes well over a millisecond
        assert!(run.as_u64().unwrap() > 1_000_000, "{run}");
    }
    for run in r["counters"]["page-faults"].as_array().unwrap() {
        assert!(run.as_u64().unwrap() > 0, "{run}");
    }
    // present and aligned with the runs; the value depends on the scheduler
    for name in ["context-switches", "cpu-migrations"] {
        assert_eq!(r["counters"][name].as_array().unwrap().len(), 3, "{name}");
    }
}

#[test]
fn denied_software_events_are_unavailable_not_zero() {
    let json = run_with("cpu-migrations");
    let status = json["perfratio"]["measurements"]["counters"]
        .as_str()
        .unwrap();
    if status == "measured" {
        return;
    }
    assert_eq!(status, "unavailable");
    assert!(json["results"][0].get("counters").is_none());
    assert!(json["perfratio"]["measurements"]["counters_unavailable_reason"].is_string());
}

#[test]
fn frequency_is_plausible_when_cycles_can_be_counted() {
    let json = run_with("frequency");
    let r = &json["results"][0];
    let Some(hz) = r["mean_effective_frequency_hz"].as_f64() else {
        eprintln!("skipped: no cycles counter or the runs are too short");
        return;
    };
    assert!((1.0e8..1.0e10).contains(&hz), "{hz}");
    assert!(["task_clock", "user_time"].contains(&r["effective_frequency_basis"].as_str().unwrap()));
}
