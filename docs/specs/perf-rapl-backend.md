# Spec: Linux perf PMU RAPL energy backend (#223)

## Problem / Motivation
References: #223, #240, #59, `docs/specs/output-schema-contract.md`.

`-E/--energy` reads `/sys/class/powercap/intel-rapl:*/energy_uj`. Since CVE-2020-8694
(PLATYPUS) most distributions make that file root-only, and containers often do not mount
it. The kernel also exposes RAPL through the `power` PMU
(`/sys/bus/event_source/devices/power`), which any user can read when `perf_event_open`
is allowed (`CAP_PERFMON` or `kernel.perf_event_paranoid <= 0`), as `perf stat -e power/energy-pkg/` does.

## Contract

### Backends
| Value | Source |
|---|---|
| `sysfs_powercap` | existing `LinuxRaplSampler` (unchanged) |
| `perf_pmu` | `power/energy-pkg/` via `perf_event_open` (`src/energy/perf_rapl.rs`, Linux only) |

PMU discovery (root is injectable in code, default `/sys/bus/event_source/devices/power`):
- `type`: integer for `perf_event_attr.type`.
- `events/energy-pkg`: `event=0x02`, used as `config`.
- `events/energy-pkg.scale`: Joules per count (typically `2.3283064365386962890625e-10`); must be positive and finite.
- `events/energy-pkg.unit`: if present it must be `Joules`.
- `cpumask`: one CPU per package; a counter is opened per listed CPU (`pid=-1`, `PERF_FLAG_FD_CLOEXEC`) and the package energies are summed.
- Energy (J) = `(end_count - start_count) * scale`. Only `energy-pkg` is used (same domain as the sysfs `package-*` selection, no `psys` double counting).

### Selection
- `--energy-backend <auto|sysfs|perf>`, or env `PERFRATIO_ENERGY_BACKEND` (the flag wins). Default `auto`.
- `auto`: use sysfs if every selected `energy_uj` is readable, otherwise try the perf PMU.
  The sysfs path is not modified.
- `sysfs` / `perf`: only that backend; no fallback.
- An invalid value is an error, but only checked when `--energy` is given; without `--energy` the
  choice and `PERFRATIO_ENERGY_BACKEND` are ignored. On non-Linux every choice results in "unavailable".

### JSON metadata (additive, `schemaVersion` MINOR bump)
`perfratio.energy_backend`: `"sysfs_powercap"` or `"perf_pmu"`. Omitted when `--energy` is not
given or no backend is usable. It lives in the `perfratio` metadata object, next to `version`.
Adding an optional key is a MINOR change under the #240 rules, so a file that carries it is
`schemaVersion` 1.1 once #240 lands (1.0 → 1.1); consumers must ignore it if they do not know it.

### Interaction with #240 `measurements.energy_unavailable_reason`
The reason is computed from both backends under the effective preference:
- a backend produced samples: `measurements.energy = "measured"` (+ `energy_backend`);
- `auto`, neither works: `rapl_unreadable` if sysfs files or the PMU exist but could not be read/opened
  (EACCES, paranoid), else `rapl_not_found`; non-Linux: `unsupported_platform`;
- forced `sysfs` or `perf`: the reason refers to that backend only (forcing `perf` under a
  strict `perf_event_paranoid` gives `rapl_unreadable`, not a sysfs fallback).
The reason set stays as in #240; no new value is needed. The #240 implementation must call
`energy::select_sampler` (or its successor) so status, reason and `energy_backend` agree.
Implemented: `Measurements::new` and `perfratio.energy_backend` both use the result of `energy::select_sampler_detailed`.

## Testing
- Unit: parsing of `type`, `event=`, `.scale`, `.unit`, `cpumask`; fake PMU tree via the
  injectable root; count-to-Joule conversion incl. u64 wrap; backend preference/env resolution;
  auto fallback selection and the `rapl_unreadable`/`rapl_not_found` classification via an injected
  sysfs root and perf opener (errno mapped through `pmu::decide`: EACCES/EPERM and other errors are
  unreadable, ENOENT/ENODEV are not found).
- Not run on real hardware (CI and development machine are not bare-metal Linux with
  `perf_event_open`): the `perf_event_open`/`read` calls and the equality of both backends against
  `perf stat -e power/energy-pkg/`. The Linux-only code was not compiled locally on macOS; CI covers it.
