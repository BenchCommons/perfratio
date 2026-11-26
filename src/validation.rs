//! Workload validation (`--expect-output-sha256`, `--validate`): check that a
//! benchmarked command produced the right result, so that an implementation
//! cannot look fast just because it did less work. See
//! `docs/specs/workload-validation.md`.
//!
//! Validation runs after the timed window of a run closes and is never part of
//! the time, memory or energy series.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The most stdout bytes kept per validated run. Larger output cannot be
/// validated and fails the benchmark with an explicit message.
pub const OUTPUT_LIMIT: usize = 16 << 20;

/// What to do with a run that fails validation (`--on-invalid`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnInvalid {
    /// Abort the benchmark
    #[default]
    Fail,
    /// Drop the run from the statistics, loudly
    Omit,
    /// Keep the run and flag it
    Keep,
}

impl OnInvalid {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "fail" => Some(OnInvalid::Fail),
            "omit" => Some(OnInvalid::Omit),
            "keep" => Some(OnInvalid::Keep),
            _ => None,
        }
    }
}

/// Validation settings of a benchmark session
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationConfig {
    /// Expected SHA-256 of stdout (64 lowercase hex digits)
    pub expected_sha256: Option<String>,
    /// Shell command that receives the stdout of each run on stdin
    pub validator: Option<String>,
    /// Check only the first timed run
    pub first_only: bool,
    pub on_invalid: OnInvalid,
}

impl ValidationConfig {
    /// Whether the run of this iteration is checked (warmup runs never are)
    pub fn applies_to(&self, iteration: crate::benchmark::executor::BenchmarkIteration) -> bool {
        use crate::benchmark::executor::BenchmarkIteration;
        match iteration {
            BenchmarkIteration::Benchmark(i) => !self.first_only || i == 0,
            _ => false,
        }
    }

    /// `output_sha256`, `command` or `output_sha256+command`
    pub fn mode(&self) -> &'static str {
        match (&self.expected_sha256, &self.validator) {
            (Some(_), Some(_)) => "output_sha256+command",
            (None, Some(_)) => "command",
            _ => "output_sha256",
        }
    }

    /// Check the stdout of the run `run` (the value of `PERFRATIO_ITERATION`).
    /// `Err` only when the validator cannot be started or the output is too
    /// large; a failed check is an `Ok` outcome.
    pub fn check(&self, run: u64, stdout: &[u8]) -> Result<RunOutcome> {
        if stdout.len() >= OUTPUT_LIMIT {
            anyhow::bail!(
                "the stdout of run {run} has at least {} MiB, more than the {} MiB that can be \
                 validated. Use a smaller workload or '--validate-first-only'.",
                OUTPUT_LIMIT >> 20,
                OUTPUT_LIMIT >> 20
            );
        }
        let sha256 = sha256_hex(stdout);
        let mut failures = Vec::new();
        if let Some(expected) = &self.expected_sha256 {
            if *expected != sha256 {
                failures.push(format!("expected output sha256 {expected}, got {sha256}"));
            }
        }
        if let Some(validator) = &self.validator {
            if let Some(failure) = run_validator(validator, run, stdout)? {
                failures.push(failure);
            }
        }
        Ok(RunOutcome { sha256, failures })
    }
}

/// Result of validating one run
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    /// SHA-256 of the run's stdout
    pub sha256: String,
    /// Why the run is invalid; empty when it is valid
    pub failures: Vec<String>,
}

impl RunOutcome {
    pub fn is_valid(&self) -> bool {
        self.failures.is_empty()
    }
}

/// The `validation` object of a result (`schemaVersion` 1.17)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationSummary {
    /// `output_sha256`, `command` or `output_sha256+command`
    pub mode: String,
    pub on_invalid: OnInvalid,
    /// Only the first timed run was checked
    pub first_only: bool,
    /// Runs that were checked (successful runs only; failed runs are
    /// reported by their exit code)
    pub runs_checked: u64,
    /// Checked runs that failed validation
    pub runs_invalid: u64,
    /// Invalid runs dropped from the statistics (`--on-invalid omit`)
    pub runs_omitted: u64,
    /// Indices of the omitted runs, as in `omitted_failed_runs`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_runs: Vec<usize>,
    /// SHA-256 of the stdout of the first checked run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
}

/// Normalize and check a hash given to `--expect-output-sha256`
pub fn parse_sha256(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
}

/// Run `validator` through the platform shell, feeding it `stdout`.
/// `Ok(None)` when it exits 0, else the reason.
fn run_validator(validator: &str, run: u64, stdout: &[u8]) -> Result<Option<String>> {
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.arg("/C").arg(validator);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(validator);
        c
    };
    let mut child = command
        .env("PERFRATIO_RUN_ID", run.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run the validator '{validator}'"))?;
    let mut stdin = child.stdin.take().context("validator stdin")?;
    let input = stdout.to_vec();
    // A validator that does not read its input closes the pipe early: ignore that
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let output = child
        .wait_with_output()
        .with_context(|| format!("Failed to wait for the validator '{validator}'"))?;
    let _ = writer.join();
    if output.status.success() {
        return Ok(None);
    }
    let code = output
        .status
        .code()
        .map_or("a signal".to_string(), |c| format!("exit code {c}"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Ok(Some(if stderr.is_empty() {
        format!("validator '{validator}' failed with {code}")
    } else {
        let tail: String = stderr
            .chars()
            .rev()
            .take(300)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("validator '{validator}' failed with {code}: {tail}")
    }))
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    let (words, _) = block.as_slice().as_chunks::<4>();
    for (slot, word) in w.iter_mut().zip(words) {
        *slot = u32::from_be_bytes(*word);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

/// SHA-256 (FIPS 180-4) of `data` as 64 lowercase hex digits
pub fn sha256_hex(data: &[u8]) -> String {
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let (blocks, rest) = data.as_chunks::<64>();
    for block in blocks {
        compress(&mut state, block);
    }
    let mut tail = rest.to_vec();
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in tail.as_chunks::<64>().0 {
        compress(&mut state, block);
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_fips_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // crosses a block boundary exactly (64 bytes) and the padding edge (56)
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        assert_eq!(
            sha256_hex(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
    }

    #[test]
    fn parse_sha256_normalizes_and_rejects() {
        let hash = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855";
        assert_eq!(parse_sha256(hash), Some(hash.to_lowercase()));
        assert_eq!(parse_sha256("abc"), None);
        assert_eq!(parse_sha256(&"g".repeat(64)), None);
    }

    #[test]
    fn hash_mismatch_names_expected_and_actual() {
        let config = ValidationConfig {
            expected_sha256: Some(sha256_hex(b"right")),
            ..Default::default()
        };
        assert!(config.check(0, b"right").unwrap().is_valid());
        let wrong = config.check(3, b"wrong").unwrap();
        assert!(!wrong.is_valid());
        assert!(wrong.failures[0].contains(&sha256_hex(b"right")));
        assert!(wrong.failures[0].contains(&sha256_hex(b"wrong")));
    }

    #[test]
    fn oversized_output_is_an_error() {
        let config = ValidationConfig {
            expected_sha256: Some(sha256_hex(b"")),
            ..Default::default()
        };
        assert!(config.check(0, &vec![0; OUTPUT_LIMIT]).is_err());
    }

    #[test]
    fn only_benchmark_runs_are_checked() {
        use crate::benchmark::executor::BenchmarkIteration::*;
        let all = ValidationConfig::default();
        assert!(all.applies_to(Benchmark(5)));
        assert!(!all.applies_to(Warmup(0)));
        assert!(!all.applies_to(NonBenchmarkRun));
        let first = ValidationConfig {
            first_only: true,
            ..Default::default()
        };
        assert!(first.applies_to(Benchmark(0)));
        assert!(!first.applies_to(Benchmark(1)));
    }

    #[cfg(unix)]
    #[test]
    fn validator_sees_stdin_and_run_id() {
        let config = ValidationConfig {
            validator: Some(
                "test \"$(cat)\" = hello && test \"$PERFRATIO_RUN_ID\" = 7".to_string(),
            ),
            ..Default::default()
        };
        assert!(config.check(7, b"hello").unwrap().is_valid());
        let bad = config.check(7, b"nope").unwrap();
        assert!(!bad.is_valid());
        assert!(bad.failures[0].contains("exit code 1"));
    }
}
