//! `perfratio-mcp` over stdio (#386). The comparisons use fixed exports, so no verdict depends
//! on a clock; the commands that are run only check the shape of the answer.

use serde_json::{json, Value};

const META: &str = "io.modelcontextprotocol/protocolVersion";

fn server(root: &std::path::Path, args: &[&str], messages: &[Value]) -> Vec<Value> {
    let input: String = messages.iter().map(|m| format!("{m}\n")).collect();
    let output = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("perfratio-mcp"))
        .args(["--root", root.to_str().unwrap()])
        .args(args)
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("stdout holds only JSON-RPC messages"))
        .collect()
}

fn modern(id: u64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({META: "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}});
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    modern(
        id,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}

fn export(mean: f64, memory: f64) -> String {
    let times = [mean, mean * 1.001, mean * 0.999, mean, mean * 1.0005];
    json!({"results": [{
        "command": "app", "mean": mean, "stddev": mean * 0.001, "median": mean,
        "user": 0.0, "system": 0.0, "min": mean * 0.999, "max": mean * 1.001,
        "times": times, "memory_usage_byte": vec![memory as u64; 5],
    }]})
    .to_string()
}

#[test]
fn discover_and_tools_list() {
    let dir = tempfile::tempdir().unwrap();
    let out = server(
        dir.path(),
        &[],
        &[
            modern(1, "server/discover", json!({})),
            modern(2, "tools/list", json!({})),
        ],
    );
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[0]["result"]["resultType"], "complete");
    assert_eq!(out[0]["result"]["supportedVersions"][0], "2026-07-28");
    assert_eq!(
        out[0]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "perfratio-mcp"
    );
    let names: Vec<&str> = out[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["capabilities", "run_benchmark", "compare"]);
    for response in &out {
        assert_eq!(response["result"]["ttlMs"], 0);
        assert_eq!(response["result"]["cacheScope"], "public");
    }
}

#[test]
fn oversized_line_is_refused_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let ping = modern(7, "ping", json!({}));
    let input = format!("{}\n{ping}\n", "x".repeat(3 * 1024 * 1024));
    let output = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("perfratio-mcp"))
        .args(["--root", dir.path().to_str().unwrap()])
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["error"]["code"], -32600);
    assert_eq!(lines[1]["id"], 7);
    assert!(lines[1]["result"].is_object());
}

#[test]
fn root_without_value_is_a_usage_error() {
    assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("perfratio-mcp"))
        .arg("--root")
        .assert()
        .code(2);
}

#[cfg(unix)]
#[test]
fn export_to_a_symlink_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("target.json"), "keep").unwrap();
    std::os::unix::fs::symlink(dir.path().join("target.json"), dir.path().join("out.json"))
        .unwrap();
    let out = server(
        dir.path(),
        &[],
        &[call(
            1,
            "run_benchmark",
            json!({"argv": ["true"], "runs": 1, "export_file": "out.json"}),
        )],
    );
    assert_eq!(out[0]["result"]["isError"], true);
    assert!(out[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("symbolic link"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("target.json")).unwrap(),
        "keep"
    );
}

#[test]
fn legacy_initialize_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let out = server(
        dir.path(),
        &[],
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "initialize",
                   "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                              "clientInfo": {"name": "t", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
        ],
    );
    assert_eq!(out.len(), 3, "a notification gets no answer");
    assert_eq!(out[0]["error"]["code"], -32602);
    assert_eq!(out[1]["result"]["protocolVersion"], "2025-11-25");
    assert!(out[1]["result"].get("resultType").is_none());
    assert_eq!(out[2]["result"]["tools"].as_array().unwrap().len(), 3);
}

#[test]
fn protocol_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut old = modern(2, "tools/list", json!({}));
    old["params"]["_meta"][META] = json!("1900-01-01");
    let out = server(
        dir.path(),
        &[],
        &[
            modern(1, "nope", json!({})),
            old,
            call(3, "no_such_tool", json!({})),
        ],
    );
    assert_eq!(out[0]["error"]["code"], -32601);
    assert_eq!(out[1]["error"]["code"], -32022);
    assert_eq!(out[1]["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(out[1]["error"]["data"]["supported"][0], "2026-07-28");
    assert_eq!(out[2]["error"]["code"], -32602);
}

#[test]
fn malformed_input() {
    let dir = tempfile::tempdir().unwrap();
    let output = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("perfratio-mcp"))
        .args(["--root", dir.path().to_str().unwrap()])
        .write_stdin("{not json\n[1]\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[0]["id"], Value::Null);
    assert_eq!(lines[1]["error"]["code"], -32600);
}

#[test]
fn capabilities_reports_limits_and_allow_list() {
    let dir = tempfile::tempdir().unwrap();
    let out = server(
        dir.path(),
        &["--allow", "true"],
        &[call(1, "capabilities", json!({}))],
    );
    let result = &out[0]["result"];
    assert_eq!(result["isError"], Value::Null);
    let c = &result["structuredContent"];
    assert_eq!(c["verdictVersion"], "1.0");
    assert_eq!(c["limits"]["max_runs"], 1000);
    assert_eq!(c["allowed_programs"], json!(["true"]));
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(&text, c);
}

#[test]
fn compare_two_exports_gives_a_verdict() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("base.json"), export(1.0, 1_000_000.0)).unwrap();
    std::fs::write(dir.path().join("cur.json"), export(0.5, 2_000_000.0)).unwrap();
    let out = server(
        dir.path(),
        &[],
        &[call(
            1,
            "compare",
            json!({"baseline_file": "base.json", "current_file": "cur.json"}),
        )],
    );
    let v = &out[0]["result"]["structuredContent"];
    assert_eq!(out[0]["result"]["isError"], Value::Null, "{out:?}");
    assert_eq!(v["verdictVersion"], "1.0");
    assert_eq!(v["baseline"], "base.json");
    assert_eq!(v["verdict"], "trade_off");
    let metrics = v["commands"][0]["metrics"].as_array().unwrap();
    let wall = metrics.iter().find(|m| m["metric"] == "wall").unwrap();
    assert_eq!(wall["assessment"], "improved");
    assert!(wall["change"].as_f64().unwrap() < -0.4);
    assert!(wall["ci_low"].as_f64().unwrap() <= wall["ci_high"].as_f64().unwrap());
    let memory = metrics.iter().find(|m| m["metric"] == "memory").unwrap();
    assert_eq!(memory["assessment"], "regressed");
}

#[test]
fn compare_identical_exports_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.json"), export(1.0, 1_000_000.0)).unwrap();
    let out = server(
        dir.path(),
        &[],
        &[call(
            1,
            "compare",
            json!({"baseline_file": "a.json", "current_file": "a.json"}),
        )],
    );
    assert_eq!(
        out[0]["result"]["structuredContent"]["verdict"],
        "unchanged"
    );
}

#[test]
fn paths_stay_below_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(dir.path().join("outside.json"), export(1.0, 1.0)).unwrap();
    let mut messages = Vec::new();
    for (i, path) in ["../outside.json", "/etc/passwd", ""].iter().enumerate() {
        messages.push(call(
            i as u64,
            "compare",
            json!({"baseline_file": path, "current_file": path}),
        ));
    }
    messages.push(call(
        9,
        "run_benchmark",
        json!({"argv": ["true"], "export_file": "../x.json"}),
    ));
    let out = server(&root, &[], &messages);
    for response in &out {
        assert_eq!(response["result"]["isError"], true, "{response}");
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("relative path"), "{text}");
    }
    assert!(!dir.path().join("x.json").exists());
}

#[cfg(unix)]
#[test]
fn symlink_out_of_the_root_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(dir.path().join("outside.json"), export(1.0, 1.0)).unwrap();
    std::os::unix::fs::symlink(dir.path().join("outside.json"), root.join("link.json")).unwrap();
    let out = server(
        &root,
        &[],
        &[call(
            1,
            "compare",
            json!({"baseline_file": "link.json", "current_file": "link.json"}),
        )],
    );
    assert_eq!(out[0]["result"]["isError"], true);
    assert!(out[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("outside the root"));
}

#[test]
fn invalid_arguments_are_tool_errors() {
    let dir = tempfile::tempdir().unwrap();
    let out = server(
        dir.path(),
        &[],
        &[
            call(1, "run_benchmark", json!({"argv": []})),
            call(2, "run_benchmark", json!({"argv": ["true"], "runs": 0})),
            call(3, "run_benchmark", json!({"argv": ["true"], "runs": 1001})),
            call(4, "run_benchmark", json!({"argv": ["true"], "shell": true})),
            call(5, "run_benchmark", json!({})),
            call(6, "compare", json!({"baseline_file": "x.json"})),
        ],
    );
    assert_eq!(out.len(), 6);
    for response in &out {
        assert_eq!(response["result"]["isError"], true, "{response}");
    }
}

#[cfg(unix)]
#[test]
fn allow_list_blocks_other_programs() {
    let dir = tempfile::tempdir().unwrap();
    let out = server(
        dir.path(),
        &["--allow", "true"],
        &[call(
            1,
            "run_benchmark",
            json!({"argv": ["false"], "runs": 1}),
        )],
    );
    assert_eq!(out[0]["result"]["isError"], true);
    assert!(out[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("not allowed"));
}

#[cfg(unix)]
#[test]
fn run_benchmark_exports_a_baseline_that_compare_reads() {
    let dir = tempfile::tempdir().unwrap();
    let out = server(
        dir.path(),
        &[],
        &[
            call(
                1,
                "run_benchmark",
                json!({"argv": ["true"], "runs": 3, "warmup": 1, "export_file": "base.json"}),
            ),
            call(
                2,
                "compare",
                json!({"baseline_file": "base.json", "current_file": "base.json"}),
            ),
            call(
                3,
                "compare",
                json!({"baseline_file": "base.json", "argv": ["true"], "runs": 3}),
            ),
        ],
    );
    let run = &out[0]["result"]["structuredContent"];
    assert_eq!(out[0]["result"]["isError"], Value::Null, "{out:?}");
    assert_eq!(run["result"]["command"], "true");
    assert_eq!(run["result"]["times"].as_array().unwrap().len(), 3);
    assert_eq!(run["exported_to"], "base.json");
    assert!(dir.path().join("base.json").exists());
    // The same data on both sides: no change, whatever the noise of the file
    let same = &out[1]["result"]["structuredContent"];
    assert!(["unchanged", "inconclusive"].contains(&same["verdict"].as_str().unwrap()));
    assert_eq!(same["commands"][0]["metrics"][0]["change"], 0.0);
    // A fresh run of the same command is matched with the baseline by its command line
    let fresh = &out[2]["result"];
    assert_eq!(fresh["isError"], Value::Null, "{fresh}");
    assert_eq!(fresh["structuredContent"]["commands"][0]["command"], "true");
}
