# Spec: `--simd` floating-point vectorization breakdown (#211)

**Status:** Implemented for Intel and AMD Zen 4/5 (schemaVersion 1.9). AArch64 is `unavailable` (see "Implementation notes"). Hardware validation pending.

## Problem / Motivation
References: #211, #49, #217, #218, #219, `andrewrk/poop#84`.
Shared background and dependencies: `pmu-extensions-common.md`.

Wall time cannot say whether code vectorized and at which width. `--simd` counts retired
floating-point arithmetic by width and reports a vectorization ratio, for checking
auto-vectorization, `-C target-cpu=native`, and numeric kernels.

## Corrections to the issue's event list
Checked against perf's event JSON (`tools/perf/pmu-events/arch/x86/...`, kernel master):
- **AMD `fp_ret_sse_avx_ops.avx128/avx256/avx512` do not exist.** `fp_ret_sse_avx_ops.*`
  (event `0x03`) counts FLOPs by operation type (add/sub, mul, div, mac), not by width.
  Width comes from `fp_ops_retired_by_width.*` (event `0x08`), which exists on Zen 4 and Zen 5
  with `scalar_uops_retired` (umask `0x04`), `pack_128_uops_retired` (`0x08`),
  `pack_256_uops_retired` (`0x10`), `pack_512_uops_retired` (`0x20`). The Zen 2 and Zen 3 JSON
  has no such event, so those CPUs are unsupported.
- AMD counts **uops**; Intel counts **instructions**. The two are not comparable in absolute
  terms. The export carries a `unit` field and the ratio is only comparable within one
  vendor.
- Intel `FP_ARITH_INST_RETIRED.*` (event `0xC7`) counts floating-point arithmetic only.
  Integer SIMD, shuffles, loads and stores are not counted, so integer or crypto kernels
  report almost nothing. This is a stated limit, not a bug.
- AArch64 has no per-width events. NEON is fixed 128-bit; SVE width is implementation defined.

## Contract

### CLI
```
--simd     Collect a floating-point scalar/vector breakdown (Linux only).
```
- Works with or without `--counters`. Counters requested by both are opened once each.
- `-s` is taken by `--setup`, so only the long form `--simd` ships (see `cli-short-flags.md`).
- Honors `--scope` (`counter-scope-modifiers.md`). Default scope applies; per-event modifiers do
  not (the four events are one unit); `split` is rejected.
- Non-Linux: one warning `'--simd' is only supported on Linux`, then the run continues with
  `measurements.simd = "unavailable"` (reason `unsupported_platform`), like `--counters` and as the
  acceptance criteria require.
- Unsupported CPU, no PMU, denied permission: warn once, continue without the breakdown
  (never abort), `measurements.simd = "unavailable"`.

### Event selection (x86_64, from the allow-list; raw events)
Intel (family 6, models whose perf JSON contains `FP_ARITH_INST_RETIRED.*`: Skylake and later
client and server cores; **not** Haswell/Broadwell, which lack the event; on hybrid parts only
the `cpu_core` PMU). The umask is a bit mask and the kernel ORs it, which `perf`'s JSON uses
itself (`SCALAR` = `0x03`, `VECTOR` = `0xfc`), so four events suffice:

| Bucket | Event | Umask |
|---|---|---|
| `scalar_fp` | `FP_ARITH_INST_RETIRED.SCALAR` (0xC7) | `0x03` (single + double) |
| `simd_128` | `...128B_PACKED_*` | `0x0c` |
| `simd_256` | `...256B_PACKED_*` | `0x30` |
| `simd_512` | `...512B_PACKED_*` | `0xc0` (only models with AVX-512; omitted otherwise) |

Intel's description: each count is one instruction, and FMA and DPP-type instructions count
**twice** (two operations per element). This applies equally to scalar and vector buckets,
so the ratio below is consistent. Single and double precision are not split in v1.

AMD Zen 4 / Zen 5 (family 0x19 / 0x1a, models from the perf `mapfile.csv` regexes):
`fp_ops_retired_by_width` (0x08) with umasks `0x04`, `0x08`, `0x10`, `0x20` for the same four
buckets. x87 (`0x01`) and MMX (`0x02`) uops are read as `x87`/`mmx` only if a spare counter
exists (not in v1) and are not part of the ratio.

AArch64 (Armv8/9, from the Arm ARM; **not verified against perf's arm64 JSON when this spec
was written, confirm per core's TRM**): `VFP_SPEC` (`0x75`) as `scalar_fp`, `ASE_SPEC` (`0x74`)
as `simd_128`, `SVE_INST_SPEC` (`0x8006`) as `sve`. Counted as speculatively executed
operations (`unit: "ops"`). No 256/512 buckets. Only on cores that implement the event
(`/sys/bus/event_source/devices/armv8_pmuv3*/events/` shows what the driver exposes).

### Selection rules
- Raw events are chosen by vendor/family/model from the allow-list (see common doc). An unknown
  model is `unavailable (unsupported_cpu)`. Do not probe-and-see.
- Bucket sets are fixed per model; buckets the CPU lacks are absent from the JSON, not 0.
- Each bucket is one fd, `inherit`, per run, same lifecycle and calibration as #49
  (`true`'s FP count is ~0, so calibration is a formality).
- Counter budget: 4 programmable counters (Intel) or 4 of 6 (AMD). With SMT on and 4 counters,
  `--simd --counters` with the default list multiplexes; #219 scaling applies and the warning
  from #219 explains it. Without `--counters`, 4 fit on every supported model.

### Vectorization ratio
`vectorization_ratio = (simd_128 + simd_256 + simd_512) / (scalar_fp + simd_128 + simd_256 + simd_512)`
over the same run, in `[0, 1]`; `null` when the denominator is 0 (no FP arithmetic). The issue's
example (1.2M scalar, 0.1M and 32.4M vector) gives 96.4%, which matches this definition.
On AArch64: `(simd_128 + sve) / (scalar_fp + simd_128 + sve)`.
The ratio is per run; the terminal shows the mean of per-run ratios and the pooled ratio is not
exported (consumers can recompute from counts).

### Terminal output
Under the benchmark's counters line, only when measured:
```
  SIMD / FP breakdown (x86_64, instructions):
    Scalar FP:             1.2M ± 0.02M
    128-bit SIMD:          0.1M ± 0.01M
    256-bit SIMD:         32.4M ± 0.15M
    Vectorization ratio:   96.4%
```
Rows for absent buckets are not printed. On AMD the heading says `uops`. When FP total is 0:
`no floating-point arithmetic retired (integer SIMD is not counted)`.

### JSON (additive: MINOR)
Per result; per-run arrays aligned with `times`, `null` for a run that could not be read:
```jsonc
"simd": {
  "unit": "instructions",            // "instructions" (Intel) | "uops" (AMD) | "ops" (AArch64)
  "arch": "x86_64",
  "scalar_fp":  [1203441, ...],
  "simd_128":   [  98211, ...],
  "simd_256":   [32411002, ...],
  "simd_512":   [0, ...],             // only when the CPU has the bucket
  "vectorization_ratio": [0.964, ...]
},
"simd_summary": { "scalar_fp": { "mean": ..., "stddev": ... }, "vectorization_ratio": { "mean": 0.964 } }
```
- Host-level status in `perfratio.measurements.simd` (`measured | unavailable | not_requested`),
  `simd_unavailable_reason` (`unsupported_platform`, `unsupported_cpu`, `perf_paranoid`,
  `no_pmu`, `hybrid_cpu_unpinned`, `other`), like energy.
- Where the buckets were scaled for multiplexing the #219 fields (`scaled`, `coverage`) apply to
  `simd` the same way as to counters, kept under `simd_scaling` per bucket.
- Issue text says "JSON and CSV". CSV and other tabular exports are not changed in the first
  iteration, consistent with #49; a follow-up may add columns.
- `schemaVersion`: MINOR (new optional keys and a new measurements group). `--import-json`
  round-trips the object.

## Edge cases
- Intel hybrid: `--simd` valid only pinned to P-cores (`--affinity`); E-core `cpu_atom`
  lacks the 256/512 buckets. Otherwise `hybrid_cpu_unpinned`.
- AVX-512 frequency licences or double-pumped 512 on Zen 4: counts are correct; only speed
  differs. Not covered by this spec.
- Multi-threaded commands: counts sum over all threads (`inherit`), so the ratio is a pooled
  per-run ratio.
- Emulated or translated code (Rosetta-like, QEMU user mode): counts reflect the host.
- VM: `FP_ARITH_INST_RETIRED` is a programmable raw event and works only where the hypervisor
  passes the PMU through. Otherwise `no_pmu`.
- `-C target-cpu=native` with AVX-512 not used by the compiler gives `simd_512 = 0`, which is a
  valid answer (bucket exists, count 0).

## Test plan
- Unit: allow-list decision from fixture `/proc/cpuinfo` texts (Skylake client, Skylake-X,
  Ice Lake, Sapphire Rapids, Haswell, Zen 3, Zen 4, Zen 5, unknown); bucket-to-event table; ratio
  maths including zero denominator and AArch64 form; JSON shape and import round-trip.
- Mocked opener: denied permission, missing PMU, partial event failure.
- Hardware, per vendor, `PERFRATIO_TEST_PERF=1`: small C or Rust kernels built three ways
  (scalar `-O1 -fno-tree-vectorize`, SSE2 `-O3`, AVX2 `-O3 -mavx2`, AVX-512 if available):
  ratio near 0, bucket `simd_128` dominant, `simd_256` dominant. Compare bucket counts with
  `perf stat -e fp_arith_inst_retired.256b_packed_single,...` (Intel) or the AMD names within 2%.
- Sanity: a kernel with N packed AVX2 single adds (known loop count) reports about N in
  `simd_256` (exact for non-FMA).

## Implementation breakdown
1. CPU identification and allow-list module shared with #221 (`src/counters/cpu.rs`).
2. Event tables per vendor (`src/counters/simd.rs`).
3. `--simd` flag, `Options`, validation.
4. Open/read buckets through the #49 sampler, with #219 scaling.
5. Ratio, summary, terminal block.
6. JSON, measurements group, import, schema table, CHANGELOG, README, man page.
7. Tests above.

## Acceptance criteria
- `-s, --simd` parses and validates.
- Supported Intel, AMD (Zen 4+) and AArch64 CPUs measured; others `unavailable` with a reason.
- Per-run series, ratio and `unit` in JSON; terminal block with ratio.
- Warning, not failure, on VMs, denied permissions and non-Linux.

## References
#211, #49, #217, #218, #219, #240; `andrewrk/poop#84`;
Intel SDM vol. 3 (events `FP_ARITH_INST_RETIRED`); AMD PPR (FP retired ops by width);
perf JSON: `tools/perf/pmu-events/arch/x86/skylakex/floating-point.json`,
`.../icelake/floating-point.json`, `.../amdzen4/floating-point.json`; Arm Architecture
Reference Manual (PMU events `ASE_SPEC`, `VFP_SPEC`, `SVE_INST_SPEC`).

## Implementation notes (#211)
Checked against perf's event JSON on kernel master with `gh api`:
- **Broadwell has `FP_ARITH_INST_RETIRED.*`** (broadwell and broadwellx JSON, same umasks as
  Skylake), contrary to the correction above. Haswell (haswell, haswellx) does not. The
  allow-list therefore includes Broadwell (models 0x3D, 0x47, 0x4F, 0x56) and no Haswell.
- Intel allow-list (family 6, from `mapfile.csv`): Broadwell, Skylake client (0x4E, 0x5E, 0x8E,
  0x9E, 0xA5, 0xA6), 0x55 (Skylake-X, Cascade Lake, Cooper Lake), Ice Lake (0x7D, 0x7E, 0x6A,
  0x6C), Tiger Lake (0x8C, 0x8D), Rocket Lake (0xA7), Sapphire and Emerald Rapids (0x8F, 0xCF),
  Granite Rapids (0xAD, 0xAE). Buckets with 512 bits only from Skylake-X on.
- Intel hybrid parts (Alder, Raptor, Meteor, Arrow, Lunar Lake, or `/sys/devices/cpu_core`
  present) are `unavailable (hybrid_cpu_unpinned)` for now: the events live on the `cpu_core`
  PMU type, and reading them pinned to P-cores is a follow-up.
- AMD: Zen 4 is family 0x19 outside perf's Zen 3 ranges (models 0x00-0x0F, 0x20-0x2F, 0x40-0x5F).
  Zen 5 is family 0x1a models 0x00-0x2F, 0x40-0x4F, 0x60-0x7F. Other models are
  `unsupported_cpu` until their JSON is checked.
- **AArch64 is unavailable.** `ASE_SPEC` (0x74) and `VFP_SPEC` (0x75) are `ARMV8_IMPDEF_*` in
  `include/linux/perf/arm_pmuv3.h`, not architectural, and perf's common arm64 JSON lists only
  `ASE_INST_SPEC` (0x8005) and `SVE_INST_SPEC` (0x8006). A per-core table needs each core's TRM
  and hardware to test on, so the spec's AArch64 set stays unimplemented (`unsupported_cpu`).
- Raw config is `event[7:0] | umask << 8 | event[11:8] << 32` with `PERF_TYPE_RAW` (4).
- A bucket that cannot be opened makes the whole breakdown `unavailable` (all or nothing),
  because a missing bucket would bias the ratio. A run's ratio is `null` unless every bucket of
  that run could be read.
- `--simd` alone does not set `measurements.counters`. `simd_scope` records `all` or `user`.
- The delta table gets a "Vectorization ratio" row with a p-value but no verdict, since more
  vectorization is neither better nor worse.
- Not run on hardware (development was on macOS, CI has no PMU). The event tables, the
  allow-list, the raw encoding, the ratio and the export are unit-tested with injected CPU
  text and readings. The kernel-compare test in the test plan still needs an Intel and an AMD host
  with `PERFRATIO_TEST_PERF=1`.
