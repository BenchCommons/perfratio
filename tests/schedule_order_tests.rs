//! `--schedule randomized|balanced` and `perfratio.schedule` (#324).

mod common;
use common::perfratio;

use serde_json::Value;

fn export(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    perfratio()
        .args(["--debug-mode", "--runs=8"])
        .args(args)
        .args(["sleep 0.01", "sleep 0.02", "sleep 0.03"])
        .arg("--export-json")
        .arg(&out)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
}

fn order(doc: &Value) -> Vec<u64> {
    doc["perfratio"]["schedule"]["order"]
        .as_array()
        .expect("order")
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect()
}

#[test]
fn same_seed_same_order_different_seed_differs() {
    let a = export(&["--schedule=randomized", "--seed=1"]);
    let b = export(&["--schedule=randomized", "--seed=1"]);
    let c = export(&["--schedule=randomized", "--seed=2"]);
    assert_eq!(a["perfratio"]["schedule"]["mode"], "randomized");
    assert_eq!(a["perfratio"]["schedule"]["seed"], 1);
    assert_eq!(order(&a), order(&b));
    assert_ne!(order(&a), order(&c));
    // 3 commands x 8 runs, each command equally often
    let o = order(&a);
    assert_eq!(o.len(), 24);
    for command in 0..3 {
        assert_eq!(o.iter().filter(|&&x| x == command).count(), 8);
    }
}

#[test]
fn balanced_rotates_the_start_and_has_no_seed() {
    let doc = export(&["--schedule=balanced"]);
    let execution = &doc["perfratio"]["schedule"];
    assert_eq!(execution["mode"], "balanced");
    assert!(execution.get("seed").is_none());
    assert_eq!(
        order(&doc),
        [0, 1, 2, 1, 2, 0, 2, 0, 1, 0, 1, 2, 1, 2, 0, 2, 0, 1, 0, 1, 2, 1, 2, 0]
    );
}

#[test]
fn round_robin_records_the_fixed_order_and_grouped_only_the_schedule() {
    let rr = export(&["--schedule=round-robin"]);
    assert_eq!(&order(&rr)[..6], [0, 1, 2, 0, 1, 2]);
    let grouped = export(&[]);
    assert_eq!(grouped["perfratio"]["schedule"]["mode"], "grouped");
    assert!(grouped["perfratio"]["schedule"].get("order").is_none());
}
