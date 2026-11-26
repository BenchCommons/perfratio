# Specification: cgroup quota, thermal throttling and `--cooldown`

**Issue:** [#315](https://github.com/BenchCommons/perfratio/issues/315)
**Status:** Shipped. JSON `schemaVersion` 1.19 (re-bumped on top of master when merged).
**Related:** #314 (`environment-fingerprint.md`, `environment.container`), #222, #291, `check-system.md`,
`measurement-quality.md`, `interference-diagnostics.md`.

## 1. Problem / Motivation

Two effects change time, energy and counters without any change in the program: a **CPU quota** of the
cgroup (Docker `--cpus`, Kubernetes limits, `systemd-run -p CPUQuota=`) and **thermal
throttling or drift**. The benchmark tools run perfratio inside containers. `--check-system` only
looks before the benchmark. This observes the environment **during** the runs, as diagnostics.
Nothing here changes a statistic, and a source that cannot be read is omitted, never "no throttling".

## 2. Contract

### 2.1 cgroup (Linux, v1 and v2)

Read from `/proc/self/cgroup` and `/sys/fs/cgroup`, for the process's cgroup **and its ancestors**:

| | v2 | v1 |
|:--|:--|:--|
| CPU quota | `cpu.max` (`max` is none), the smallest of the chain | `cpu.cfs_quota_us` / `cpu.cfs_period_us` (-1 is none) |
| memory limit | `memory.max` | `memory.limit_in_bytes` (values from 2^60 are none) |
| throttling | `cpu.stat`: `nr_throttled`, `throttled_usec`, summed over the chain | `cpu.stat`: `nr_throttled`, `throttled_time` (ns), summed |
| OOM kills | `memory.events`: `oom_kill` | `memory.oom_control`: `oom_kill` |

A v2 cgroup whose `cpu.stat` has no `nr_throttled` (the `cpu` controller is off there) gives no counter:
the diagnostic is omitted. Other platforms: not applicable (nothing is read).

### 2.2 Observation during the benchmark

Before and after every timed run (outside the timed window) the counters are read. Per result:

```json
"interference": {
  "cgroup_throttled_usec": 345000,
  "cgroup_throttled_runs": 12,
  "cgroup_oom_kills": 0,
  "thermal": { "start_c": 52.0, "end_c": 71.0, "max_c": 73.0, "throttle_events": 0 }
}
```

- `cgroup_throttled_usec`: throttled time that grew during the runs; `cgroup_throttled_runs`: runs in which it grew.
  It is the cgroup's counter: it includes other processes in the same cgroup. `0` means the counter was read and did not grow.
- `thermal` (only with `--thermal`, off by default since the reads cost about 1 s on GitHub-hosted runners and delayed perfratio itself; sampled on a background thread once per second, never on the benchmark thread): the hottest of the thermal zones (`/sys/class/thermal/thermal_zone*/temp`) and hwmon sensors
  (`/sys/class/hwmon/hwmon*/temp*_input`) before the first run, after the last, and the highest read
  in between (at most every 100 ms). Readings of 0 or below, or from 150 °C, are sensors that are not working and are ignored.
  `throttle_events` is the growth of Intel's `thermal_throttle/core_throttle_count` and
  `package_throttle_count`; omitted when the CPUs expose none.
- The object and each field are omitted when their source cannot be read. In round-robin mode a runner observes
  only around its own runs.

### 2.3 Quality check `environment_stability` (metric `environment`)

| status | when |
|:--|:--|
| `warn` | the cgroup throttled any run; the OOM killer ran; thermal throttling events grew; the temperature rose by 10 °C or more; or the per-run effective frequency (`--counters`) fell by 5% or more with Mann-Kendall p < 0.001 |
| `pass` | something was read and none of the above |
| `not_applicable` | nothing can be read on this host (no cgroup counters, no sensor), or `--debug-mode` |

### 2.4 `--cooldown <DURATION|until-temp=C>` and `--cooldown-timeout`

Before every run (warmup and timed) and so between commands, outside the timed window.

- `DURATION` (`2s`, `500ms`): wait that long. Not before the very first run of the session.
- `until-temp=C`: wait until the hottest sensor reads at most C °C (checked every 0.5 s), also before the first run.
  After `--cooldown-timeout` (default 5 minutes) it prints
  `Warning: '--cooldown': the CPU is still at 90 °C after waiting 300 s ('--cooldown-timeout'); continuing.`
  and goes on. Without a readable sensor it warns once and does not wait.
- The wait counts toward `--duration`. Ctrl-C interrupts it.

### 2.5 `perfratio.environment.container` (#314)

`container` gains `cpu_quota` (CPUs, e.g. 0.2), `memory_limit` (bytes), `cgroup_version` (1 or 2) and
`effective_cpus` (the smaller of the CPUs the process may run on and the quota). The object is present
when a container was detected or a limit exists. They do not enter the environment fingerprint. Fields
without a source are omitted.

### 2.6 One thermal reader

`--check-system` ("Thermal" check), `interference.thermal` and `--cooldown` all use `thermal::temperatures`.

### 2.7 Test roots

`PERFRATIO_SYSTEM_CHECK_ROOT` (hidden, as for `--check-system`) makes every reader above look below that
directory instead of `/`, on any platform.

## 3. Implementation

`src/cgroup.rs`, `src/thermal.rs`, `src/environment_watch.rs` (`Watch`, `Interference`),
`src/cooldown.rs`, `BenchmarkRunner` (hooks around each run), `quality::environment_stability`.

## 4. Not covered

Per-cgroup attribution when several benchmarks share one cgroup. GPU temperature. macOS and Windows
thermal state (`pmset -g therm`, WMI). Real throttling and real temperature behaviour are manual
bare-metal tests: `systemd-run --scope -p CPUQuota=20% perfratio -N --runs 20 'sh -c "while :; do :; done" &'`-style
loads show `cgroup_throttled_usec` growing; on a laptop under sustained load `thermal.end_c` rises.

## 5. Tests

Unit: fixture trees for v1 and v2 layouts, parent quotas, unlimited values, unreadable counters, snapshot
arithmetic (`src/cgroup.rs`); sensors, broken readings, throttle counters, the monitor (`src/thermal.rs`);
`Watch` over a changing fixture (`src/environment_watch.rs`); cooldown parsing, waiting, give-up, no sensor
(`src/cooldown.rs`). Integration (`tests/cgroup_thermal_tests.rs`): a command that bumps the fixture's
counters is reported as throttled (Unix), `--cooldown` timing and messages, schema (`tests/json_schema_tests.rs`).
