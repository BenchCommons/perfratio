# Spec: RAPL domain breakdown, overhead alignment and permission message (#59)

## Problem / Motivation
References: #59, #106 (`rapl-psys-double-counting.md`), `energy-accuracy.md`, #240
(`output-schema-contract.md`), `sharkdp/hyperfine#92`.

`--energy` reports one number, the package energy. Memory-bound tools spend a visible share in
DRAM, and users cannot see it. The energy window also opens before the spawn and closes after the
wait, so it includes the shell start-up that `time_real` already removes, which inflates
`mean_watts` for fast commands. When `energy_uj` is root-only the user only sees
"unavailable", with no way to fix it.

## Contract

### `--energy-domains` (requires `--energy`)
Per run, the Linux sysfs backend also reads these zones (under `/sys/class/powercap/intel-rapl`):

| Reported name | Zone | Sum over |
|---|---|---|
| `package` | top-level zone named `package-N` | all packages |
| `core` | sub-zone `intel-rapl:N:M` named `core` (PP0) | all packages |
| `uncore` | sub-zone named `uncore` (PP1, graphics) | all packages |
| `dram` | sub-zone named `dram` | all packages |
| `psys` | top-level zone named `psys` | one platform zone |

A domain the hardware or the permissions do not expose is left out (never reported as 0). A
counter wrap uses `max_energy_range_uj`; a wrap with an unknown range leaves the domain out for
that run.

**No double counting (#106 rule kept).** The total (`energy_joules`, `mean_energy_joules`,
`mean_watts`) is still the sum of the `package-*` zones only. `core`, `uncore` and `dram` are
sub-zones of a package, and `psys` overlaps the package, so the domains are a breakdown and are
never added to each other or to the total. When no `package-*` zone is readable the total falls
back to `psys`, as before. Other backends (perf PMU, IOReport, EMI) ignore the flag.

Console, after the Energy line:
```
Domains (mean):      core 0.710 J · uncore 0.020 J · dram 0.180 J · package 1.284 J · psys 2.100 J
```

JSON, new in `schemaVersion` 1.5 (MINOR, additive): `results[].energy_domains_joules`, an object
`{domain: [joules per run]}`, each array aligned with `times`. Present only when every run has
a reading of the same set of domains.

### Overhead alignment
The sampling window starts just before the spawn and ends after the wait. The reported energy is
the window energy scaled to the child's lifetime:

```
E_child = E_window * min(1, time_real / window)
```

`time_real` is the value already used for the run (shell spawn time subtracted, on Linux measured
from the child's pre-exec timestamp). The same factor is applied to every domain. This assumes the
power is the same during the overhead and during the child.

**Error bound.** `|E_child - E_true| <= (window - time_real) * |P_overhead - P_child|`. The
overhead is about 1 to 3 ms for a shell and under 1 ms without one. At a power difference of
5 W that is at most 5 to 15 mJ per run, and below the RAPL quantization (about 60 µJ) only for
overheads under 12 µs, so runs under 10 ms stay noisy (the existing warning). The factor is
not applied when `time_real >= window` (debug mode, mock executor).

### Permission message
When sysfs `energy_uj` exists but cannot be read and no other backend works, one warning is
printed per process:

```
Warning: Cannot read /sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj (permission denied).
Run as root, or grant read access: sudo chmod o+r ... (resets on reboot; the README has a udev
rule). Granting read access ... lets any local user infer secrets from the power signal (PLATYPUS) ...
```

The README has the udev rule. Granting read access re-opens CVE-2020-8694, so the text says to
do it only on machines the user controls.

## Verification
- Unit tests with an injectable sysfs root (`LinuxRaplSampler::try_new_at`): domains with one and
  two packages, `psys`, total unchanged, missing zones, off by default, permission helper.
- Debug-mode CLI tests (mock sampler) for the flag, the JSON export and the unchanged total.
- Layout test: `schemaVersion` is `1.5`.
- `#[ignore]` test `bare_metal_rapl_domains` (Linux with readable `energy_uj`, run with
  `cargo test --test energy_domains_tests -- --ignored`). **Not run on real hardware by the author**
  (macOS dev machine, CI has no RAPL).
