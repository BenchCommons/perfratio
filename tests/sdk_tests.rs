//! The reference region clients in `sdk/` against `perfratio --region` (#311).
//!
//! Every language builds the same three-phase example (sleep 0.1 s, region of 0.3 s,
//! sleep 0.1 s) and the tests check that perfratio reports the middle phase. Rust always
//! runs; C and C++ run when `cc`/`c++` exist, Python when `python3` (or `python`) does.
//! A missing toolchain skips the language with a message; a toolchain that fails to build
//! the example fails the test.

mod common;
use common::perfratio;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use perfratio::region;

const ENV_FILE: &str = "PERFRATIO_REGION_FILE";

fn sdk_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk")
}

/// The packaged crate does not ship `sdk/`: nothing to test there.
fn sdk_present() -> bool {
    sdk_dir().join("rust/Cargo.toml").exists()
}

/// Scratch directory shared by the builds, removed when the process exits is not possible
/// for statics, so it lives under the system temp dir and is removed by the OS.
fn scratch() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().unwrap()).path()
}

fn exe(name: &str) -> PathBuf {
    scratch().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Whether `program --version` runs.
fn available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A program that can run the example.
struct Client {
    /// Language label, the prefix of the SDK id in the `hello` line
    lang: &'static str,
    program: String,
    args: Vec<String>,
    /// Argument that makes the example exit inside the region without stopping it,
    /// and the exit code that produces
    exit_mode: (&'static str, i32),
    /// Argument that makes the region end by unwinding (panic/exception), and the exit code
    unwind_mode: Option<(&'static str, i32)>,
}

impl Client {
    fn command(&self, extra: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).args(extra).env_remove(ENV_FILE);
        command
    }

    /// The command line for perfratio (`-N` splits it like a POSIX shell, so use `/`)
    fn perfratio_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(|part| format!("\"{}\"", part.replace('\\', "/")))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn run_checked(command: &mut Command, what: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|e| panic!("{what}: cannot start: {e}"));
    assert!(
        output.status.success(),
        "{what} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn rust_example() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            if !available(&cargo) {
                eprintln!("skipping the Rust client: no cargo");
                return None;
            }
            run_checked(
                Command::new(&cargo)
                    .args([
                        "build",
                        "--locked",
                        "--example",
                        "three_phase",
                        "--manifest-path",
                    ])
                    .arg(sdk_dir().join("rust/Cargo.toml"))
                    .arg("--target-dir")
                    .arg(scratch().join("rust-target")),
                "building the Rust example",
            );
            Some(Client {
                lang: "rust",
                program: scratch()
                    .join("rust-target/debug/examples")
                    .join(format!("three_phase{}", std::env::consts::EXE_SUFFIX))
                    .to_string_lossy()
                    .into_owned(),
                args: vec![],
                // std::process::exit calls ExitProcess on Windows, which skips atexit handlers
                exit_mode: if cfg!(windows) { ("", 0) } else { ("exit", 3) },
                unwind_mode: Some(("panic", 101)),
            })
        })
        .as_ref()
}

fn build_native(
    compiler: &str,
    source: &str,
    standard: &[&str],
    exit_mode: (&'static str, i32),
    unwind_mode: Option<(&'static str, i32)>,
    lang: &'static str,
    out: &str,
) -> Option<Client> {
    if !available(compiler) {
        eprintln!("skipping the {lang} client: no `{compiler}`");
        return None;
    }
    let target = exe(out);
    let mut command = Command::new(compiler);
    command
        .args(standard)
        .args(["-O1", "-Wall", "-Wextra", "-o"])
        .arg(&target)
        .arg(sdk_dir().join("c").join(source));
    if cfg!(all(unix, not(target_os = "macos"))) {
        command.arg("-pthread");
    }
    run_checked(&mut command, &format!("building the {lang} example"));
    Some(Client {
        lang,
        program: target.to_string_lossy().into_owned(),
        args: vec![],
        exit_mode,
        unwind_mode,
    })
}

fn python_example() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let Some(python) = ["python3", "python"].into_iter().find(|p| available(p)) else {
                eprintln!("skipping the Python client: no python3");
                return None;
            };
            Some(Client {
                lang: "python",
                program: python.into(),
                args: vec![sdk_dir()
                    .join("python/example.py")
                    .to_string_lossy()
                    .into_owned()],
                exit_mode: ("", 0),
                unwind_mode: Some(("raise", 1)),
            })
        })
        .as_ref()
}

fn c_example() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            build_native(
                "cc",
                "example.c",
                &["-std=c11", "-D_POSIX_C_SOURCE=200809L"],
                ("exit", 3),
                None,
                "c",
                "example_c",
            )
        })
        .as_ref()
}

fn cpp_example() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            build_native(
                "c++",
                "example.cpp",
                &["-std=c++11"],
                ("", 0),
                Some(("throw", 4)),
                "c",
                "example_cpp",
            )
        })
        .as_ref()
}

/// Run the example with the event file set and return its text.
fn events_of(client: &Client, mode: &str) -> (String, i32) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("events.log");
    let mut command = client.command(if mode.is_empty() {
        &[]
    } else {
        std::slice::from_ref(&mode)
    });
    let status = command.env(ENV_FILE, &file).status().unwrap();
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    (text, status.code().unwrap_or(-1))
}

fn check_client(client: &Client) {
    let lang = client.lang;

    // Without perfratio the calls are no-ops: the program runs and writes nothing.
    let dir = tempfile::tempdir().unwrap();
    let output = run_checked(
        client.command(&[]).current_dir(dir.path()),
        &format!("{lang}: running without perfratio"),
    );
    assert!(
        output.stderr.is_empty(),
        "{lang}: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "{lang}: wrote a file without perfratio"
    );
    // An empty variable disables the client as well
    run_checked(
        client.command(&[]).env(ENV_FILE, ""),
        &format!("{lang}: empty {ENV_FILE}"),
    );

    // The wire format: parsed by perfratio's own parser, in order, on a monotonic clock
    let (text, code) = events_of(client, "");
    assert_eq!(code, 0, "{lang}");
    assert!(
        text.starts_with(&format!("hello 1 {lang}/")),
        "{lang}: {text:?}"
    );
    let report = region::parse(&text).unwrap().unwrap();
    let middle = report.regions["middle"];
    assert_eq!(middle.count, 1, "{lang}");
    assert!(
        (0.28..0.6).contains(&middle.seconds),
        "{lang}: {}",
        middle.seconds
    );
    assert!(report.sdk.unwrap().starts_with(lang));
    let stamps: Vec<u64> = text
        .lines()
        .skip(1)
        .map(|l| l.rsplit(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(
        stamps.windows(2).all(|w| w[0] <= w[1]),
        "{lang}: {stamps:?}"
    );

    // Flush on exit: leaving the region open and exiting still writes the events
    if !client.exit_mode.0.is_empty() {
        let (text, code) = events_of(client, client.exit_mode.0);
        assert_eq!(code, client.exit_mode.1, "{lang}");
        assert!(text.starts_with("hello 1 "), "{lang}: {text:?}");
        assert!(text.contains("start middle "), "{lang}: {text:?}");
        assert!(!text.contains("stop middle"), "{lang}: {text:?}");
    }
    // Flush on panic/exception: the guard stops the region while unwinding
    if let Some((mode, expected)) = client.unwind_mode {
        let (text, code) = events_of(client, mode);
        assert_eq!(code, expected, "{lang}");
        let report = region::parse(&text).unwrap().unwrap();
        assert_eq!(report.regions["middle"].count, 1, "{lang}: {text:?}");
    }

    // End to end: perfratio reports the middle phase, the process is longer
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("out.json");
    perfratio()
        .args([
            "-N",
            "--runs=2",
            "--warmup=0",
            "--region=middle",
            "--export-json",
        ])
        .arg(&json_path)
        .arg(client.perfratio_line())
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    let result = &json["results"][0];
    let region = &result["regions"][0];
    assert_eq!(region["name"], "middle", "{lang}");
    assert_eq!(region["status"], "measured", "{lang}");
    assert!(region["sdk"].as_str().unwrap().starts_with(lang), "{lang}");
    let region_mean = region["mean"].as_f64().unwrap();
    let process_mean = result["mean"].as_f64().unwrap();
    assert!(
        (0.28..0.6).contains(&region_mean),
        "{lang}: region {region_mean}"
    );
    assert!(
        process_mean >= region_mean + 0.15,
        "{lang}: process {process_mean} s should include the two outer phases of region {region_mean} s"
    );

    // --metric region:NAME ranks on the region
    let output = perfratio()
        .args(["-N", "--runs=1", "--warmup=0", "--metric=region:middle"])
        .arg(client.perfratio_line())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout).into_owned();
    assert!(stdout.contains("region:middle (mean"), "{lang}: {stdout}");
}

#[test]
fn rust_client() {
    if !sdk_present() {
        return;
    }
    let Some(client) = rust_example() else { return };
    check_client(client);
}

#[test]
fn rust_client_unit_tests() {
    if !sdk_present() {
        return;
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    if !available(&cargo) {
        eprintln!("skipping the Rust client unit tests: no cargo");
        return;
    }
    run_checked(
        Command::new(&cargo)
            .args(["test", "--locked", "--manifest-path"])
            .arg(sdk_dir().join("rust/Cargo.toml"))
            .arg("--target-dir")
            .arg(scratch().join("rust-unit-target")),
        "the Rust client unit tests",
    );
}

#[test]
fn c_client() {
    if !sdk_present() {
        return;
    }
    let Some(client) = c_example() else { return };
    check_client(client);
}

#[test]
fn cpp_client() {
    if !sdk_present() {
        return;
    }
    let Some(client) = cpp_example() else { return };
    check_client(client);
}

#[test]
fn python_client() {
    if !sdk_present() {
        return;
    }
    let Some(client) = python_example() else {
        return;
    };
    check_client(client);
}

#[test]
fn python_client_unit_tests() {
    if !sdk_present() {
        return;
    }
    let Some(client) = python_example() else {
        return;
    };
    run_checked(
        Command::new(&client.program)
            .args(["-m", "unittest", "test_perfratio_region"])
            .current_dir(sdk_dir().join("python"))
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env_remove(ENV_FILE),
        "the Python client unit tests",
    );
}

/// `--region-scope` with the Python client (#371): the CPU time inside the region is read
/// when the markers arrive, so the idle phases around it (and the interpreter start-up)
/// stay out. Linux and macOS read the child's CPU time; elsewhere the test is skipped.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn python_client_region_scope_cpu_time() {
    if !sdk_present() {
        return;
    }
    let Some(client) = python_example() else {
        return;
    };
    let script = scratch().join("scope_burn.py");
    std::fs::write(
        &script,
        r#"import time
import perfratio_region as pr
time.sleep(0.2)
with pr.region("burn"):
    end = time.process_time() + 0.3
    while time.process_time() < end:
        pass
time.sleep(0.3)
"#,
    )
    .unwrap();
    let out = scratch().join("scope_burn.json");
    let python_path = sdk_dir().join("python");
    let line = format!(
        "\"{}\" \"{}\"",
        client.program.replace('\\', "/"),
        script.to_string_lossy().replace('\\', "/")
    );
    perfratio()
        .env("PYTHONPATH", &python_path)
        .args([
            "-N",
            "--runs=3",
            "--warmup=1",
            "--region=burn",
            "--region-scope",
        ])
        .args(["--export-json", out.to_str().unwrap(), &line])
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let result = &json["results"][0];
    let region = &result["regions"][0];
    let metrics = region["metrics"].as_array().expect("region metrics");
    let cpu = |name: &str| {
        metrics
            .iter()
            .find(|m| m["name"] == name)
            .unwrap_or_else(|| panic!("no {name} in {metrics:?}"))
    };
    for name in ["cpu_user", "cpu_system"] {
        assert_eq!(cpu(name)["scope"], "region");
        assert_eq!(cpu(name)["unit"], "s");
        assert_eq!(cpu(name)["values"].as_array().unwrap().len(), 3);
    }
    let in_region =
        cpu("cpu_user")["mean"].as_f64().unwrap() + cpu("cpu_system")["mean"].as_f64().unwrap();
    // 0.3 s of busy work; the marker latency is far below the tolerance
    assert!(
        (0.25..0.45).contains(&in_region),
        "region CPU time {in_region}"
    );
    // the whole-process values keep covering everything (interpreter start-up included)
    let whole = result["user"].as_f64().unwrap() + result["system"].as_f64().unwrap();
    assert!(whole > in_region, "whole {whole} <= region {in_region}");
    assert!(region["times"][0].as_f64().unwrap() > 0.25);
}
