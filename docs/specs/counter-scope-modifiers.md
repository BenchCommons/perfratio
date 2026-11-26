# Spec: user/kernel counter scope: `--scope` and `:u` / `:k` modifiers (#218)

**Status:** Implemented (schemaVersion 1.9) on top of `--counters`: `--scope user|kernel|all|split`,
`:u` / `:k` / `:uk` modifiers, per-scope preflight probe with fallback or error, suffixed JSON
keys, `counters_scope` (now also `kernel`) and `counters_requested_scope`. D1 resolved as the
recommendation: default `all` with fallback. Not run on hardware (macOS dev host, CI has no PMU):
the `PERFRATIO_TEST_PERF=1` comparisons against `perf stat` below are still to be done on a
bare-metal runner. Not yet done: `--simd` / `--topdown` scope (those flags do not exist yet).
Details that the spec left open: `--scope kernel` or `split` with a denied kernel scope is an
error (explicit), `--scope all` or no `--scope` falls back; when the fallback makes `E` and `E:u`
the same event they are merged; with `split` (or any pair of `E:u` + `E:k` without `E`) the
terminal shows `instr 1.70M [user 421k, kernel 1.28M]`; derived rates never mix scopes (the JSON
`efficiency` uses the unsuffixed scope, or the only scope in use, and is absent otherwise);
`--debug-mode` mock values are user = 1/4 and kernel = 3/4 of the `all` value.

## Problem / Motivation
References: #218, #49, #217, `andrewrk/poop#42`, `perf-stat(1)`, `perf_event_open(2)`.
Shared background and dependencies: `pmu-extensions-common.md`.

`poop` reported about 4x fewer instructions than `perf stat` for `ls` because it sets
`exclude_kernel = 1`, and #49 as specified does the same. For syscall-, I/O- and page-fault-heavy
commands most instructions run in the kernel. The user needs both views, as with
`[User: x, System: y]` for time.

## Contract

### CLI
```
--scope <user|kernel|all|split>
    Which privilege levels hardware counters count. Default: see decision D1.
    user    user space only        (exclude_kernel = 1)
    kernel  kernel space only      (exclude_user = 1)
    all     user + kernel          (no exclude bits), like `perf stat` without a modifier
    split   user and kernel counted separately, total = user + kernel
```
Per-event modifiers inside `--counters`, as in `perf stat -e`:
```
--counters=instructions:u,instructions:k,cycles:u,cycles:k
```
- `:u` user only, `:k` kernel only, `:uk` or no modifier = the `--scope` default. `:h`
  (hypervisor) is rejected: `modifier ':h' is not supported`.
- A modifier on an event overrides `--scope` for that event. `--scope split` expands every
  unmodified event `E` to `E:u` plus `E:k`.
- Duplicates are detected after expansion (`instructions` and `instructions:uk` under
  `--scope all` are one event).
- The modifiers also apply to event names added later by other specs, except where a spec
  says an event cannot be scoped (software events, `task-clock`: see below).
- Applies to `--simd` (#211) and `--topdown` (#221) as `--scope` only (no per-event modifiers);
  `--topdown` rejects `split`.

### Mapping to `perf_event_attr`
| Scope | `exclude_user` | `exclude_kernel` | `exclude_hv` | `exclude_guest` |
|---|---|---|---|---|
| user | 0 | 1 | 1 | 1 |
| kernel | 1 | 0 | 1 | 1 |
| all | 0 | 0 | 1 | 1 |

`exclude_hv = 1` always: on x86 the bit has no effect, and hypervisor-level counting
(POWER, s390) is not a goal. `exclude_guest = 1` so that on a KVM host the counts do not include
guests. `exclude_idle` is never set (the arm64 PMU driver rejects it; verify when implementing). The issue text proposes `exclude_hv = 0` for `all`; this spec sets it to 1 because
it changes nothing on x86 (the bit is ignored there) and keeps one attribute shape for all scopes.
Source for the semantics of each bit: `perf_event_open(2)`; kernel filtering in
`perf_exclude_event()` (`kernel/events/core.c`) and the privilege check in
`perf_event_open()` / `perf_allow_kernel()`.

### Privilege requirement and fallback
`exclude_kernel = 0` (scopes `kernel`, `all`, `split`) needs `perf_event_paranoid <= 1` or
`CAP_PERFMON`. At the default sysctl value 2 it returns `EACCES`.
- Startup probe (one `perf_event_open` per distinct scope in use, `pmu-preflight.md`): if the
  kernel scope is denied and the user did **not** ask for it explicitly (`--scope` absent or
  `all`), fall back to `user`, print **one** warning
  `kernel counters need kernel.perf_event_paranoid <= 1 or CAP_PERFMON; counting user space only`
  with the copy-pasteable command from #217, and record the effective scope in the export.
- If the user asked for `kernel`, `split` or a `:k` modifier explicitly and it is denied: error
  (exit code 1) with the same guidance. A silent downgrade would invalidate the request.
- Note that #217 currently probes with a user-only counter, so its "allowed" answer does not
  imply kernel counting works. The probe in this spec opens the real scope.

### Decision D1: default scope (maintainer decision needed before #49 ships)
The issue asks for `all`, matching `perf stat`. #49 as specified in PR #276 uses user-only.
- If `all` is the default, `instructions` means user+kernel on privileged hosts and falls back
  to user-only on unprivileged ones. Two hosts then produce different meanings for one key.
  Mitigation (required if D1 = `all`): the per-result field `counters_scope` (below) always
  states what was counted.
- If `user` stays the default, the key means the same everywhere, but results disagree with
  `perf stat` by default (the `poop#42` surprise) and the default cannot later change to `all`
  without a MAJOR bump.

**Recommendation:** `all` with fallback and `counters_scope`, adopted by #49 before it ships,
so that `counters_scope` exists from its first release. If #49 ships earlier with user-only
and without the field, then an absent `counters_scope` means `user` forever and this spec's
default stays `user`. Either way no existing field changes meaning.

### Measurement semantics
- One fd per (event, scope) pair as in #49. `split` doubles the fd count.
- Hardware budget: fixed counters have independent user/OS enable bits, but each fixed counter
  holds one event at a time, so `X:u` and `X:k` take one fixed and one programmable counter.
  `split` with the four default events needs up to 4 programmable counters plus fixed ones
  (see budget in `pmu-extensions-common.md`); on SMT-on Intel with 4 programmable counters it
  fits only without other features. Multiplexing is handled by #219 per fd.
- `total` for `split` is `user + kernel` computed from the two scaled counts. It can differ
  slightly from a third `all` counter because of multiplexing windows; `split` never opens one.
- Overhead calibration (#49) runs per `(event, scope)`: the calibration command's median is
  subtracted from the matching key, clamped at 0. Kernel-scope overhead is larger (fork,
  exec, page-table setup are kernel work).
- Counting starts before `spawn()` (as in #49), so the kernel scope includes the kernel part of
  `fork`/`execve`. `perf stat` uses `enable_on_exec` and excludes the pre-exec part. The
  calibration subtraction removes the constant part; the remaining difference is documented and
  bounded by the test below.
- Scope cannot be applied to software events and `task-clock`: they are not filtered by
  privilege level in a useful way (see `software-events-migrations.md`). They ignore the scope.

### Terminal output
With `split`, or when the same event appears with both `:u` and `:k`:
```
  Counters (mean):
    Instructions:  1.70M   [User: 421k, Kernel: 1.28M]   (IPC 1.18)
    Cycles:        1.43M   [User: 372k, Kernel: 1.06M]
```
Otherwise the one-line form of #49, with the scope as a suffix when it is not `all`:
`instr:u 421k · cycles:u 372k`. Derived rates (#213) use matching scopes only; IPC is shown
for a scope when both `instructions` and `cycles` exist in that scope.

### JSON (additive: MINOR)
Existing flat shape from #49 is kept. The issue proposes nested `{total, user, kernel}` objects;
that would change the type of `counters.<name>` from an array to an object, a MAJOR change
against the #49 shape, so it is not used. Instead the scope is part of the key, using the
`perf stat` modifier syntax:
```jsonc
"counters": {
  "instructions:u": [421000, 420874, ...],     // per run, aligned with "times"
  "instructions:k": [1279000, 1278100, ...],
  "cycles":         [1430000, ...]              // no suffix = effective default scope
},
"counters_scope": "all",                        // effective default scope: user|kernel|all
"counters_summary": { "instructions:u": { "mean": ..., "stddev": ... } }
```
- Key rule: a name without suffix has the scope in `counters_scope`; with `:u` / `:k` it has
  that scope. Consumers that only know #49 names see the unsuffixed keys and nothing else.
- `counters_scope` (string, optional, present whenever `counters` is): effective scope after
  fallback.
- `perfratio.measurements.counters_requested_scope` (optional, only when it differs from the
  effective scope): `"all"` while `counters_scope` is `"user"` means the kernel scope was
  denied.
- Kernel totals for `split` are not stored; consumers sum `:u` and `:k`.
- `schemaVersion`: MINOR (new optional keys and key suffixes). No existing key changes.

## Edge cases
- `:k` with `--scope user` and vice versa: the modifier wins for that event.
- All events kernel-only on a command that never enters the kernel: small non-zero values from
  the exec path; not an error.
- Non-Linux: same error as #49.
- Containers (seccomp): `EPERM`, same message path as `EACCES`.
- `perf_event_paranoid >= 3` (Debian/Ubuntu): nothing is allowed; #217's diagnostic applies.
- Intel `perf stat` fallback hides the downgrade only in the event name suffix (`cycles:u`).
  perfratio shows the same suffix and the warning.
- Idle and interrupts: kernel counts include interrupt handlers that run while the task is
  current, as in `perf stat`; this is not filtered.

## Test plan
- Unit: modifier parser (`:u`, `:k`, `:uk`, `:h` rejected, unknown suffix, duplicates after
  expansion); `--scope` expansion; mapping scope to exclude bits; key naming; aligned-array
  filtering for suffixed keys; overhead subtraction per key; JSON with and without `:k` data.
- Mocked opener: `EACCES` on the kernel scope gives fallback plus one warning plus
  `counters_scope = "user"` for implicit scope, and an error for explicit `:k`.
- Hardware (`PERFRATIO_TEST_PERF=1`, `perf_event_paranoid <= 1`):
  - `ls -R /usr` style syscall-heavy command: `instructions:k` greater than `instructions:u`.
  - `perfratio --counters instructions:u -N CMD` vs `perf stat -e instructions:u CMD`: median
    over 20 runs within 3% (the difference is the pre-exec window).
  - `instructions:u + instructions:k` vs `perf stat -e instructions`: within 5%.
  - Same checks with `perf stat --all-user` for `--scope user`.
- Document how to run them on a bare-metal runner; CI keeps time and memory only.

## Implementation breakdown
1. Parser: `CounterSpec { kind, scope }` in `src/counters/mod.rs` (extends #49's parser).
2. `--scope` in `src/cli.rs` and `Options`; validation (`--topdown` rejects `split`).
3. Scope-to-attr mapping and per-scope probe in `src/counters/linux.rs`.
4. Fallback logic and warnings; `counters_scope` in `BenchmarkResult`.
5. Calibration keyed by `(event, scope)`.
6. Terminal formatting for split lines.
7. JSON, import round-trip, schema table and CHANGELOG.
8. Tests above; README and `doc/perfratio.1`.

## Acceptance criteria
- `--scope <user|kernel|all|split>` and `:u` / `:k` parse and validate.
- Terminal output shows the user/kernel breakdown when both are collected.
- JSON exports suffixed keys and `counters_scope`; nothing existing changes meaning.
- Hardware test agrees with `perf stat` within the stated tolerances.
- Denied kernel scope degrades with one warning (implicit) or errors (explicit).

## References
#218, #49, #217, #219, #213, #240; `andrewrk/poop#42`;
`perf_event_open(2)` (`exclude_user`, `exclude_kernel`, `exclude_hv`, `exclude_idle`);
`kernel/events/core.c` (`perf_allow_kernel`, `perf_exclude_event`).
