//! Example collector plugin for `perfratio --collector-plugin` (protocol 1,
//! `docs/specs/collector-plugins.md`).
//!
//! It measures the time between `start` and `stop` as `elapsed` and counts runs as `runs`.
//! A real collector would read a sensor in the same places.
//!
//! The variable `EXAMPLE_COLLECTOR_MODE` makes it misbehave, so that the tests can check how
//! perfratio reacts: `bad_hello`, `undeclared`, `error`, `exit_at_start`, `nan`.
//!
//! ```text
//! cargo build --example collector_plugin
//! perfratio --collector-plugin target/debug/examples/collector_plugin 'sleep 0.1'
//! ```

use std::io::{BufRead, Write};
use std::time::Instant;

use serde_json::{json, Value};

fn main() {
    let mode = std::env::var("EXAMPLE_COLLECTOR_MODE").unwrap_or_default();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut started: Option<Instant> = None;
    let mut elapsed: Option<f64> = None;
    let mut runs = 0u64;

    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            return;
        };
        let reply = match request["type"].as_str() {
            Some("hello") if mode == "bad_hello" => json!({"type": "hello", "protocol": 99}),
            Some("hello") => json!({
                "type": "hello",
                "protocol": 1,
                "name": "example",
                "metrics": [
                    {"id": "elapsed", "unit": "s", "direction": "lower_is_better",
                     "kind": "measured", "scope": "process"},
                    {"id": "runs", "unit": "count", "direction": "neutral"}
                ]
            }),
            Some("start") if mode == "exit_at_start" => return,
            Some("start") => {
                started = Some(Instant::now());
                runs += 1;
                json!({"type": "ok"})
            }
            Some("stop") => {
                elapsed = started.take().map(|s| s.elapsed().as_secs_f64());
                json!({"type": "ok"})
            }
            Some("read") if mode == "error" => {
                json!({"type": "error", "message": "sensor offline"})
            }
            Some("read") if mode == "undeclared" => {
                json!({"type": "metrics", "values": {"surprise": 1}})
            }
            Some("read") if mode == "nan" => {
                json!({"type": "metrics", "values": {"elapsed": "NaN"}})
            }
            Some("read") => {
                json!({"type": "metrics", "values": {"elapsed": elapsed, "runs": runs}})
            }
            Some("shutdown") => return,
            _ => json!({"type": "error", "message": "unknown request"}),
        };
        if writeln!(stdout, "{reply}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return;
        }
    }
}
