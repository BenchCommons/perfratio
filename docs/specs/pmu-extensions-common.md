# Spec: PMU extensions, shared background and dependencies (#211, #218, #220, #221, #222)

**Status:** Proposed. Scheduled for 1.x, not 1.0.

This file is the common section for five feature specs. Each of them builds on the hardware
counter pipeline and repeats nothing from here.

| Spec | Issue | Flag / feature |
|---|---|---|
| `simd-breakdown.md` | #211 | `-s, --simd` |
| `counter-scope-modifiers.md` | #218 | `--scope`, `:u` / `:k` modifiers |
| `software-events-migrations.md` | #220 | `cpu-migrations`, `context-switches` (+ jitter warning) |
| `topdown-tma.md` | #221 | `--topdown` |
| `effective-frequency.md` | #222 | effective frequency, throttling warning |

## Dependencies

| Issue | Spec / PR | What these specs take from it |
|---|---|---|
| #49 `--counters` | `hardware-counters.md` (PR #276) | Counter pipeline: per-run fd lifecycle (open per run, enable before `spawn()`, read after `wait4`), `inherit`, overhead calibration, JSON `counters` arrays aligned with `times`, `measurements.counters`. |
| #217 preflight | `pmu-preflight.md` (PR #268) | Permission probe and diagnostics. Its threshold (`perf_event_paranoid` <= 2) covers **user-space only** counting. Kernel-side counting (#218, #220) needs <= 1 or `CAP_PERFMON`; see "Privilege levels" below. |
| #219 multiplexing | `pmu-multiplexing.md` (PR #271) | `read_format = TOTAL_TIME_ENABLED \| TOTAL_TIME_RUNNING`, scaling, `coverage`. Every counter added here goes through it. Groups need extra rules (below). |
| #240 schema | `output-schema-contract.md` | `schemaVersion`, `measurements` groups, additive-change rules. |
| #213 derived metrics | open | IPC and miss rates. #222 reuses its formatting. |

None of these specs can be implemented before #49 lands. #217 and #219 are building blocks
that #49 wires in. If #49 changes shape (for example the default scope), the affected spec
is amended before implementation starts.

## perf_event_open mechanics that every spec relies on

Sources: `perf_event_open(2)` <https://man7.org/linux/man-pages/man2/perf_event_open.2.html>;
kernel `kernel/events/core.c` and `include/linux/perf_event.h`
(<https://github.com/torvalds/linux>, master when written); `perf-security.rst`
<https://docs.kernel.org/admin-guide/perf-security.html>.

### Privilege levels
- `perf_event_open` calls `perf_allow_kernel()` whenever `attr.exclude_kernel == 0`, for
  every event type including software events. It returns `EACCES` if
  `perf_event_paranoid > 1` and the caller lacks `CAP_PERFMON` (`core.c`, `perf_allow_kernel`).
- So: `exclude_kernel = 1` (user-space counting of the own process tree) works up to
  `perf_event_paranoid = 2`; `exclude_kernel = 0` needs `<= 1` or `CAP_PERFMON`
  (`CAP_SYS_ADMIN` before Linux 5.8). Debian and Ubuntu kernels carry a patch that adds
  levels 3 and 4 and forbids all unprivileged use. Stock Linux treats 3 as "no unprivileged use".
- `perf stat` copes by retrying with `:u`. perfratio must do the same visibly: it records
  the effective scope (see `counter-scope-modifiers.md`) and never reports a user-only value
  under a name that means "user + kernel".

### Per-run lifecycle, inheritance and groups
- Counters are opened per run with `inherit = 1`, `pid = 0`, `cpu = -1` as in #49.
- `PERF_FORMAT_GROUP` does not work together with `inherit` on all kernels
  (man page: "Inherit does not work for some combinations of read_format values, such as
  PERF_FORMAT_GROUP"). #49 therefore uses one fd per event. This stays.
- A **group** is a different thing from `PERF_FORMAT_GROUP`: events opened with `group_fd`
  set to a leader are scheduled on the PMU together or not at all. #221 needs a group
  (`slots` leader plus metric events). Each member is still read with its own `read(2)`, with
  `read_format` limited to `TOTAL_TIME_*`, so no `PERF_FORMAT_GROUP` read is needed. Whether
  `inherit` is accepted for such a group must be verified on the oldest supported kernel
  during implementation (test plan item "group + inherit").
- Software events do not use PMU registers. They are never multiplexed and work in VMs that
  have no vPMU.

### Fixed and programmable counters (hardware budget)
Counts of physical counters decide when multiplexing starts. Figures are typical and must be
read from the machine at run time, not hard-coded.

| CPU family | Fixed | Programmable | Notes |
|---|---|---|---|
| Intel Skylake to Cascade Lake | 3 (instructions, core cycles, ref cycles) | 4 per thread with SMT on, 8 with SMT off | Source: Intel SDM vol. 3, ch. 18 / 19. |
| Intel Ice Lake / Sunny Cove and later | 4 (adds `slots`) plus the `perf_metrics` register | 8 per thread on Golden Cove, 4 with SMT on older | Linux support for the topdown metrics: `arch/x86/events/intel/core.c`. |
| AMD Zen 1 to Zen 4 | none | 6 core counters | AMD PPR for each family. |
| AArch64 (PMUv3) | cycle counter (`PMCCNTR`) | implementation defined, commonly 6 | Check `/sys/bus/event_source/devices/armv8_pmuv3*/`. |

A software event costs nothing here. An event with exclude bits different from another event
cannot share a fixed counter, so `instructions:u` and `instructions:k` together take one fixed
and one programmable counter (see `counter-scope-modifiers.md`).

### Hybrid CPUs
On Intel hybrid parts (Alder Lake and later) there are two PMUs, `cpu_core` and `cpu_atom`
(`/sys/devices/cpu_core/`, `/sys/devices/cpu_atom/`), with different event sets. Several
Arm systems expose one PMU per core type. A task event bound to one PMU counts nothing while
the task runs on the other core type. First iteration: counters whose meaning depends on the
core type (`--simd`, `--topdown`, effective frequency) are valid only on non-hybrid systems or
when `--affinity` restricts the command to one core type. Otherwise they are reported
`unavailable` with reason `hybrid_cpu_unpinned`, and a warning explains `--affinity`.
Opening per-PMU events and combining them is a follow-up (open question in `hardware-counters.md`).

### Raw events must be allow-listed, never probed
Raw event codes (`PERF_TYPE_RAW`) are model specific. Opening code `0xC7` on a CPU where it
means something else succeeds and returns garbage. Specs #211 and #221 therefore select events
from an allow-list keyed on CPU vendor, family and model (read from `/proc/cpuinfo` or CPUID,
no new dependency), derived from the regexes in perf's `tools/perf/pmu-events/arch/x86/mapfile.csv`
(and the arm64 equivalent). An unknown model is `unavailable` with reason `unsupported_cpu`.
Sysfs capability files are used where the kernel provides them, for example
`/sys/devices/cpu/events/slots` and `topdown-retiring` exist only when the kernel supports
`perf_metrics`.

### Virtual machines and containers
- No vPMU (typical for cloud instances that are not metal): `perf_event_open` for hardware
  and raw events returns `ENOENT`/`ENODEV`/`EOPNOTSUPP`. Software events still work, subject to
  `perf_event_paranoid`.
- A vPMU may expose generic counters and fixed `instructions`/`cycles` but not `slots`, the
  topdown metrics, or all raw events. Availability must be probed per event, not assumed from
  "a PMU exists".
- Docker's default seccomp profile blocks `perf_event_open` (`EPERM`).
- CI covers time and memory only (see `AGENTS.md`). All PMU tests are gated like #49:
  `PERFRATIO_TEST_PERF=1` on bare metal or a self-hosted runner.

## JSON: additive fields and the `schemaVersion` MINOR impact

Rules from `output-schema-contract.md`: a new optional field, a new key in an object, or a new
value in an open enum is a **MINOR** change. Consumers ignore unknown keys.

- All new fields in these five specs are optional and additive. None changes the meaning of an
  existing field. Together they need **one MINOR bump per release that ships any of them**
  (for example 1.1 to 1.2). Shipping several features in one release is one bump. Each
  feature's PR adds its fields to the schema table in `output-schema-contract.md` and a
  CHANGELOG line, and the bump is applied once per release.
- The measurement contract is owned by perfratio and consumed by tools-benchmark, so each spec
  names its fields exactly and lists the status values consumers can see.
- New `perfratio.measurements` groups: `simd` (#211) and `topdown` (#221). `counters` (from #49)
  gains software events and scoped names. Status values are the existing ones:
  `measured`, `unavailable`, `not_requested`, plus `partial` as defined by #49.
  `<group>_unavailable_reason` is an open enum; this set adds `unsupported_cpu`,
  `hybrid_cpu_unpinned`, `kernel_scope_denied`, `unsupported_kernel`. Adding values to an open
  enum is MINOR.
- **One thing would be MAJOR:** changing what an existing counter name means. If #49 ships
  `instructions` as user-only and a later release makes the default scope `all`, the same key
  changes meaning. `counter-scope-modifiers.md` removes the risk by recording the effective
  scope in the export from the first release that has `--counters`; see its decision D1.
- Per-run arrays stay aligned with `times`, filtered by `--omit-failed-runs` and outlier
  discarding exactly like the #49 arrays. `null` marks a run where a value could not be read.
- `--import-json` round-trips every new field and ignores absent ones.

## Group-level rules for multiplexing (#219)

- A counter that is not in a group is scaled as in `pmu-multiplexing.md`.
- Events of one group are scheduled together. Their `time_running` values are equal, so
  scaling by `time_enabled / time_running` is the same factor for all of them and cancels in
  ratios. Topdown fractions (#221) are computed from unscaled group values; the group's
  `coverage` is still reported so users see that the group ran only part of the time.
- Specs that add events must state how many programmable counters they need so the
  implementer can warn before the first run: when the sum for the requested features exceeds
  the machine's budget, print one warning naming the features and suggest dropping some.
  Counter budget per feature (programmable counters, x86):

| Feature | Intel Skylake-class | Intel Ice Lake+ | AMD Zen 4 |
|---|---|---|---|
| #49 defaults (instructions, cycles, cache-misses, branch-misses) | 2 | 2 | 4 |
| #211 `--simd` | 4 | 4 | 4 |
| #221 `--topdown` | not supported | 0 (uses `slots` + metrics) | 6 (includes cycles) |
| #220 software events | 0 | 0 | 0 |
| #222 frequency | 0 (reuses cycles, fixed) | 0 | reuses cycles |

## Verification against `perf stat`
Every spec's hardware test compares perfratio with `perf stat` on the same command
(`-e` list built from the event names in the spec). Tolerances are in each test plan. The
reason for differences is always listed (enable point, overhead calibration, scope), because
`andrewrk/poop#42` shows how easily a user-space-only counter looks "4x too low".

## References
- #49, #211, #217, #218, #219, #220, #221, #222, #240.
- `perf_event_open(2)`; `perf-stat(1)`; kernel `Documentation/admin-guide/perf-security.rst`.
- `andrewrk/poop#17`, `#42`, `#84`.
