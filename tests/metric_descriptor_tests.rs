//! Every metric of the JSON export has a descriptor in `perfratio.metrics[]` (#317).

mod common;
use common::perfratio;

use perfratio::benchmark::benchmark_result::BenchmarkResult;
use perfratio::export::metadata::{ExportMetadata, MeasurementStatus, Measurements};
use perfratio::metric_registry::{
    descriptors, Status, DYNAMIC_FIELDS, NON_METRIC_FIELDS, REGISTRY,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn export(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    common::perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// Mock-executor cases covering every measurement group.
const CASES: &[&[&str]] = &[
    &["--runs=3", "sleep 0.1"],
    &["--runs=3", "--energy", "sleep 0.1"],
    &["--runs=3", "--energy", "--energy-domains", "sleep 0.1"],
    &["--runs=3", "--resource-usage", "sleep 0.1"],
    &["--runs=3", "--counters", "sleep 0.5"],
    &["--runs=3", "--counters", "--scope", "split", "sleep 0.5"],
    &["--runs=3", "--counters=cycles", "--topdown", "sleep 0.5"],
    &["--runs=3", "--simd", "sleep 0.5"],
    &["--runs=2", "--concurrency", "3", "--energy", "sleep 0.1"],
    &["--runs=3", "--first-run=separate", "sleep 0.1"],
    &["--runs=4", "--warmup=2", "sleep 0.1", "sleep 0.2"],
    &["--runs=4", "--jobs=2", "sleep 0.1", "sleep 0.2"],
];

#[test]
fn every_key_of_the_result_schema_is_a_metric_or_declared_not_to_be() {
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/docs/schema/perfratio-export.schema.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let covered: BTreeSet<&str> = REGISTRY
        .iter()
        .flat_map(|s| s.fields.iter().copied())
        .chain(NON_METRIC_FIELDS.iter().copied())
        .chain(DYNAMIC_FIELDS.iter().copied())
        .collect();
    let missing: Vec<_> = schema["$defs"]["result"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| !covered.contains(k.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "result keys without a metric descriptor: add them to REGISTRY (src/metric_registry.rs) \
         or to NON_METRIC_FIELDS: {missing:?}"
    );
}

#[test]
fn every_exported_metric_has_a_descriptor() {
    for args in CASES {
        let document = export(args);
        let metrics = document["perfratio"]["metrics"]
            .as_array()
            .unwrap_or_else(|| panic!("no perfratio.metrics for {args:?}"));
        let mut ids = BTreeSet::new();
        let mut fields = BTreeSet::new();
        for m in metrics {
            assert!(ids.insert(m["id"].as_str().unwrap()), "duplicate id in {m}");
            for key in ["unit", "direction", "scope", "source", "kind", "status"] {
                assert!(m[key].is_string(), "{key} of {m} ({args:?})");
            }
            assert_ne!(
                m["status"], "not_requested",
                "{m}: not requested metrics are not listed"
            );
            fields.extend(
                m["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f.as_str().unwrap()),
            );
        }
        for must in ["time.wall", "time.user", "time.system", "memory.peak_rss"] {
            assert!(ids.contains(must), "{must} missing for {args:?}");
        }
        for result in document["results"].as_array().unwrap() {
            for key in result.as_object().unwrap().keys() {
                assert!(
                    NON_METRIC_FIELDS.contains(&key.as_str()) || fields.contains(key.as_str()),
                    "result key `{key}` has no descriptor for {args:?}: ids {ids:?}"
                );
            }
            // names decided at run time
            for (field, prefix) in [
                ("counters", "counter."),
                ("energy_domains_joules", "energy.domain."),
                ("custom_metrics", "custom."),
            ] {
                if let Some(map) = result.get(field).and_then(Value::as_object) {
                    for name in map.keys() {
                        let id = format!("{prefix}{name}");
                        assert!(ids.contains(id.as_str()), "{id} missing for {args:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn the_descriptor_set_follows_the_requested_groups() {
    let ids = |args: &[&str]| -> BTreeSet<String> {
        export(args)["perfratio"]["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };
    let plain = ids(&["--runs=3", "sleep 0.1"]);
    assert!(!plain.iter().any(|i| i.starts_with("energy.")));
    assert!(!plain.iter().any(|i| i.starts_with("batch.")));
    let energy = ids(&["--runs=3", "--energy", "sleep 0.1"]);
    assert!(energy.contains("energy.total"));
    let concurrency = ids(&["--runs=2", "--concurrency", "3", "sleep 0.1"]);
    assert!(concurrency.contains("batch.throughput"));
}

#[test]
fn custom_metrics_are_described() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args([
            "--runs=2",
            "--shell=none",
            "--output-metric",
            "n=n=([0-9]+)",
        ])
        .arg("--export-json")
        .arg(&path)
        .arg(if cfg!(windows) {
            "cmd /c echo n=5"
        } else {
            "echo n=5"
        })
        .assert()
        .success();
    let document: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let custom = document["perfratio"]["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "custom.n")
        .expect("custom.n");
    assert_eq!(custom["unit"], "unitless");
    assert_eq!(custom["direction"], "lower_is_better");
    assert_eq!(custom["status"], "available");
}

fn metadata(measurements: Measurements) -> ExportMetadata {
    ExportMetadata::new(BTreeMap::new(), measurements)
}

#[test]
fn an_unavailable_group_is_described_with_its_reason_and_has_no_values() {
    let mut m = Measurements::new(None, false);
    m.counters = MeasurementStatus::Unavailable;
    m.counters_unavailable_reason = Some("perf_paranoid");
    m.energy = MeasurementStatus::Unavailable;
    m.energy_unavailable_reason = Some("rapl_not_found");
    let result = BenchmarkResult::default();
    let found = descriptors(&[result], &metadata(m));

    let counter = found.iter().find(|d| d.id == "counter").expect("counter");
    assert_eq!(counter.status, Status::PermissionDenied);
    assert_eq!(counter.reason.as_deref(), Some("perf_paranoid"));
    let energy = found.iter().find(|d| d.id == "energy.total").unwrap();
    assert_eq!(energy.status, Status::Unsupported);
    assert_eq!(energy.reason.as_deref(), Some("rapl_not_found"));
    // no value of an unavailable metric may be serialized as 0
    let json = serde_json::to_value(BenchmarkResult::default()).unwrap();
    for field in &energy.fields {
        assert!(json.get(field).is_none(), "{field}");
    }
    assert!(json.get("counters").is_none());
}

#[test]
fn partial_counters_list_the_events_that_could_not_be_opened() {
    let mut m = Measurements::new(None, false);
    m.counters = MeasurementStatus::Partial;
    m.counters_unavailable_reason = Some("no_pmu");
    m.counters_unavailable_events = Some(vec!["cache-misses".into()]);
    let result = BenchmarkResult {
        counters: Some(BTreeMap::from([(
            "instructions".to_string(),
            vec![Some(10u64), None],
        )])),
        ..Default::default()
    };
    let found = descriptors(&[result], &metadata(m));
    let read = found
        .iter()
        .find(|d| d.id == "counter.instructions")
        .unwrap();
    assert_eq!(
        read.status,
        Status::Partial,
        "a run without a reading is partial"
    );
    let missing = found
        .iter()
        .find(|d| d.id == "counter.cache-misses")
        .unwrap();
    assert_eq!(missing.status, Status::Unsupported);
}
