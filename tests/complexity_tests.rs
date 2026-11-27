mod common;
use common::perfratio;
use serde_json::Value;

fn run(args: &[&str]) -> (Value, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    let out = perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    (
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap(),
        String::from_utf8(out).unwrap(),
    )
}

#[test]
fn linear_mock_times_fit_o_n() {
    let (json, stdout) = run(&[
        "--runs=2",
        "--complexity=n",
        "-L",
        "n",
        "1,2,4,8,16",
        "sleep {n}",
    ]);
    let c = &json["complexity"][0];
    assert_eq!(c["parameter"], "n");
    assert_eq!(c["command"], "sleep {n}");
    assert_eq!(c["values"].as_array().unwrap().len(), 5);
    let wall = &c["metrics"]["wall"];
    assert_eq!(wall["best"], "O(n)");
    assert!(wall["r2"].as_f64().unwrap() > 0.999);
    assert_eq!(wall["models"].as_array().unwrap().len(), 6);
    assert!(stdout.contains("Complexity of 'sleep {n}'"), "{stdout}");
    assert!(stdout.contains("not a proof"), "{stdout}");
}

#[test]
fn constant_mock_times_fit_o_1() {
    let (json, _) = run(&[
        "--runs=2",
        "--complexity=n",
        "-L",
        "n",
        "1,2,4,8",
        "sleep 0.5 0 0 {n}",
    ]);
    assert_eq!(json["complexity"][0]["metrics"]["wall"]["best"], "O(1)");
}

#[test]
fn too_few_values_warn_without_a_fit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--complexity=n",
            "-L",
            "n",
            "1,2",
            "sleep {n}",
        ])
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success()
        .stderr(predicates::str::contains("need at least 3"));
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(json.get("complexity").is_none());
}

#[test]
fn command_without_the_parameter_is_rejected() {
    perfratio()
        .args([
            "--debug-mode",
            "--complexity=n",
            "-L",
            "m",
            "1,2,3",
            "sleep {m}",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("does not contain '{n}'"));
}
