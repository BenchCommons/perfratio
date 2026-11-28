# Spec: GPU energy and utilization (`--gpu`) (#380)

**Status:** Implemented, **not validated on GPU hardware** (see "Hardware check"). JSON
`schemaVersion` 1.42 (additive).

## Problem / Motivation
References: #380 (parent #364), #240, #59, `rapl-domains.md`, `energy-macos-windows.md`;
NVML API reference (`nvmlDeviceGetTotalEnergyConsumption`, `nvmlDeviceGetUtilizationRates`);
AMD SMI (`amdsmi_get_energy_count`, `amdsmi_get_gpu_activity`).

RAPL, IOReport and EMI measure the CPU package and DRAM. A GPU workload (training step, CUDA or
ROCm kernel) spends most of its energy on the device, which `--energy` does not see, so the
host-only joules rank such programs as nearly free.

## Contract
`--gpu` (requires `--energy`) adds a GPU reading to every run window, taken from the vendor
management library that is **loaded with `dlopen` at run time**. There is no link-time
dependency and no new crate. Libraries tried (first hit per vendor wins, both can load):

| Backend value | Library | Energy source | Utilization source |
|---|---|---|---|
| `nvml` | `libnvidia-ml.so.1`, `libnvidia-ml.so` | `nvmlDeviceGetTotalEnergyConsumption` (mJ, cumulative; Volta and newer) | `nvmlDeviceGetUtilizationRates().gpu` |
| `amd_smi` | `libamd_smi.so`, `/opt/rocm/lib/libamd_smi.so` | `amdsmi_get_energy_count` (accumulator x resolution in uJ) | `amdsmi_get_gpu_activity().gfx_activity` |

- **Per run:** energy = counter at `stop()` minus counter at `start()` (a decrease, e.g. driver
  reload, clamps to 0). It is aligned to the child lifetime like every other domain
  (`align_to_child`).
- **Devices** are numbered `gpu:0, gpu:1, ...` across backends in load order (NVML first).
- **Total energy is unchanged.** `energy_joules`, `mean_watts` and `--metric energy` stay the
  host figure. GPU joules are separate (the RAPL package does not include the device), so a
  consumer that wants a system total adds `gpu:*` itself. `--metric` selection by GPU energy is
  not part of this change (the keys appear as `energy.domain.gpu:<n>` in `perfratio.metrics[]`).
- **Utilization** is sampled once when the run ends. NVML reports the average over its own
  sample window (1/6 s to 1 s), so for runs shorter than that it includes time before the run.
  Treat it as a coarse load indicator, not a per-run average.
- A device whose library reports no energy counter (pre-Volta NVIDIA) contributes utilization
  only. If the set of devices with an energy reading differs between runs, `energy_domains_joules`
  is omitted, as for RAPL domains.
- **Limitation:** the console line is printed inside the host energy block, so on a machine
  where the host sampler is unavailable the GPU values appear only in the JSON export.
- **Console:** the `Domains (mean)` line gains `gpu:<n> <J> J, util <p>%`.

### Library loading

The vendor libraries are opened with `dlopen` by soname (`libnvidia-ml.so.1`, `libamd_smi.so`,
then `/opt/rocm/lib/libamd_smi.so`), with `RTLD_NOW | RTLD_LOCAL`. The dynamic loader search
order applies: `RPATH`, `LD_LIBRARY_PATH`, `RUNPATH`, `ld.so.cache`, then the default
directories. A library found earlier in `LD_LIBRARY_PATH` runs its constructors inside the
perfratio process. Do not run `--gpu` (often with `--energy` and elevated privileges) with an
untrusted `LD_LIBRARY_PATH`. A symbol of an unexpected size makes the backend unavailable
with a reason; it does not panic (audit L4).

### Degradation
Without a library, a driver, or a device, perfratio prints one warning
(`Warning: --gpu: ... (gpu_library_not_found); continuing without GPU measurements.`) and the run
goes on with host energy only. `--gpu` is not an error on a machine without a GPU. Non-Unix
platforms report `unsupported_platform` (the Windows `nvml.dll` loader is not implemented).

### JSON (schemaVersion 1.41 to **1.42**, MINOR, additive)
| Field | Type | Presence |
|---|---|---|
| `perfratio.gpu` { `backends`: string[], `devices`: int, `unavailable_reason`?: string } | open enums: backends `nvml`, `amd_smi`; reasons `gpu_library_not_found`, `gpu_init_failed`, `gpu_no_devices`, `unsupported_platform` | only with `--gpu` |
| `results[].energy_domains_joules["gpu:<index>"]` | joules per run (new keys of an existing map) | only with `--gpu` |
| `results[].gpu_utilization_percent["gpu:<index>"]` | percent per run, 0 to 100 | only with `--gpu`, when every run has it |

Nothing is removed or renamed and nothing is written without `--gpu`, so a 1.41 consumer that
ignores unknown keys is unaffected. The description of `energy_domains_joules` now says that
`gpu:*` keys are not overlapping with RAPL keys. `docs/schema/1.42/` holds the schema snapshot.

**Version coordination:** this PR was renumbered from 1.37 to 1.42 after #368 (1.37) to #369 (1.41) merged into master.

## Tests
- `src/energy/gpu.rs`: `extern "C"` fake NVML and AMD SMI libraries behind the real
  function-pointer tables (init order and shutdown, device enumeration, mJ to J, uJ conversion,
  unsupported devices, `u32::MAX` activity, init failure vs. no devices), the sampler (domain
  naming across backends, host total untouched, dummy host, counter reset), and graceful absence
  of the libraries on the test host.
- `tests/json_schema_tests.rs`: `--debug-mode --gpu` uses a simulated backend (2 devices, 2 J,
  60 %); the strict schema accepts the output, and the fields are absent without `--gpu`.

## Hardware check (not done)
No NVIDIA or AMD GPU was available to the author, so the dynamic loading, the symbol names and
the struct layouts (`nvmlUtilization_t`, `amdsmi_engine_usage_t`) are checked only against the
headers and against mocks. To validate on a machine with a GPU:
1. `perfratio --energy --gpu --runs 5 -- <gpu workload>`: expect `gpu:0` joules above idle
   (compare with `nvidia-smi -q -d POWER` or `amd-smi metric -p` average power x time).
2. `--export-json out.json`: `perfratio.gpu.backends` lists the backend, `devices` matches
   `nvidia-smi -L` or `amd-smi list`.
3. An idle GPU run should give small joules and utilization near 0; a saturating kernel near 100.
4. AMD: confirm the `amdsmi_get_energy_count` signature against the installed ROCm version
   (it changed between releases); `amd_smi` with `gpu_init_failed` on a machine that has the
   library points to an ABI mismatch.
Hardware check results belong in this section.

## References
- NVML: https://docs.nvidia.com/deploy/nvml-api/
- AMD SMI: https://rocm.docs.amd.com/projects/amdsmi/en/latest/
