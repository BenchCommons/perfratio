# Spec: Energy on macOS and Windows (#291, #59)

## Status
Shipped: backends `ioreport` (macOS, Apple Silicon) and `windows_emi` (Windows) behind the
`EnergySampler` trait (`src/energy/ioreport.rs`, `src/energy/windows_emi.rs`), selected by
`select_sampler_detailed` and `--energy-backend`. Not run on real EMI hardware. On the
author's Mac (M4, macOS 27.2.1) the IOReport path correctly reports `unavailable`
(`ioreport_frozen`, see below), so macOS joules are not validated on hardware here.

## Problem / Motivation
References: #291, #59, #240. `--energy` only worked on Linux (RAPL). The goal of #291 is
the same feature on macOS and Windows with honest `unavailable` reasons.

## Contract
Sample semantics are the RAPL ones: `start()` opens a window, `stop()` returns the joules
consumed in it (host-level, not per process); mean watts = joules / wall time.

| Backend value (`perfratio.energy_backend`) | `--energy-backend` | OS | Source |
|---|---|---|---|
| `ioreport` | `ioreport` | macOS, arm64 | IOReport group `Energy Model`; sum of channels `*CPU Energy`, `GPU Energy`, `ANE*`, `DRAM*`; units mJ/uJ/nJ converted to J |
| `windows_emi` | `emi` | Windows | `GUID_DEVICE_ENERGY_METER`, `IOCTL_EMI_GET_MEASUREMENT`, pWh to J (x 3.6e-9); channels containing `PKG` only if any, else all |

`auto` picks the native backend of the OS. Forcing a backend of another OS gives
`unsupported_platform`. `PERFRATIO_ENERGY_BACKEND` accepts the same values.

New `measurements.energy_unavailable_reason` values (open enum, MINOR bump to
`schemaVersion` 1.3; `energy_backend` gains the two values above):

| Reason | Meaning |
|---|---|
| `unsupported_cpu` | Intel Mac: no no-root energy source |
| `ioreport_not_found` | `libIOReport` or the `Energy Model` group is missing (VM, old OS) |
| `ioreport_frozen` | IOReport works but the CPU energy counters read 0 under a CPU-bound probe (see below) |
| `emi_not_found` | no EMI device present |
| `emi_unreadable` | EMI devices exist but cannot be opened or queried (usually needs an elevated prompt) |

### macOS notes
- Loaded with `dlopen("/usr/lib/libIOReport.dylib")`; private API, no root. Same source as
  `powermetrics`, macmon and asitop.
- Selection runs a 100 ms CPU-burn probe. On macOS 27 the `Energy Model` CPU/ANE/DRAM
  counters read 0 for processes without Apple's entitlement while `GPU Energy` stays live
  (observed here). Reporting that as energy would rank CPU-bound runs as free, so the
  backend is `unavailable` instead. macmon works around it with firmware-derived CLPC
  report IDs; that is not implemented (private, per-chip, not verifiable here).
- Intel Macs: `unavailable` (`unsupported_cpu`).

### Windows notes
- Device discovery with SetupAPI, device opened read-only; EMI V1 (one channel) and V2
  (channels from `EMI_METADATA_V2`) are supported, picowatt-hours only.
- Counters wrap at 2^64; deltas use wrapping subtraction.

## Tests
Pure unit tests on every OS: unit conversion, channel selection, V2 metadata parsing,
wraparound, frozen-counter detection. The export contract test accepts the new reasons per OS.

## References
- IOReport usage: https://github.com/vladkens/macmon (`src/sources.rs`, `src/metrics.rs`)
- EMI: https://learn.microsoft.com/en-us/windows-hardware/drivers/powermeter/energy-meter-interface
  and `emi.h` structures (`EMI_METADATA_V2`, `EMI_CHANNEL_V2`)
