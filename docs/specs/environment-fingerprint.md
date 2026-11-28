# Specification: Environment, fingerprint and provenance (`--meta`, `--require-compatible-env`)

**Issue:** [#314](https://github.com/BenchCommons/perfratio/issues/314)
**Status:** Shipped. JSON `schemaVersion` 1.22.
**Related:** #240 (contract), #291, #315 (`container` limits), `check-system.md`, `measurement-quality.md`.

## 1. Problem / Motivation

Two exports with the same command names can come from machines that differ in CPU model, governor,
SMT or container. Results from such machines cannot be compared, and a consumer (tools-benchmark,
catalog-algorithms) could not detect it. pyperf stores system metadata with every result; this
is the perfratio equivalent: a documented environment block, a stable fingerprint, and a check in
`--compare` and `--import-json`.

## 2. Contract

### 2.1 `perfratio.environment`

Every field except `os_family`, `arch` and `perfratio` is omitted when the platform cannot report it.
Nothing is guessed.

| field | meaning | Linux | macOS | Windows |
|:--|:--|:--|:--|:--|
| `os_family` | `std::env::consts::OS` | yes | yes | yes |
| `arch` | `std::env::consts::ARCH` | yes | yes | yes |
| `kernel`, `kernel_major` | release; its first number | `uname` | `uname` | omitted |
| `cpu_model` | as in `perfratio.system` | yes | yes | yes |
| `logical_cores` | | topology, else `available_parallelism` | sysctl | `available_parallelism` |
| `physical_cores` | unique (package, core) pairs | `/sys/devices/system/cpu/cpuN/topology` | `hw.physicalcpu` | omitted |
| `smt` | threads share cores | `smt/active`, else logical > physical | logical > physical | omitted |
| `governor` | cpufreq governor, `mixed` if CPUs differ | `scaling_governor` | omitted | omitted |
| `turbo` | boost enabled | `intel_pstate/no_turbo`, `cpufreq/boost` | omitted | omitted |
| `numa_nodes` | | `/sys/devices/system/node/nodeN` | omitted | omitted |
| `memory_bytes` | | `MemTotal` | `hw.memsize` | omitted |
| `container` | `{ kind }`: `docker`, `podman`, `kubernetes`, `lxc`; present when detected | `/.dockerenv`, `/run/.containerenv`, `/proc/1/cgroup` | omitted | omitted |
| `perfratio` | `{ version, profile, features }`; a `debug` profile is much slower | yes | yes | yes |
| `settings` | `affinity`, `priority`, `concurrency` as given on the command line; omitted when none is given | yes | yes | yes |
| `backends` | `energy` (the energy backend), `counters` (`perf_event_open`, `kperf`) when in use | yes | yes | yes |

Privacy: never exported by default are the host name, user name, environment variables, the working
directory and the full command line (`perfratio.command_line` keeps only the program's file name).
Values of `--meta` and `--label` are what the user types: no secrets in them. As a safety net
(#22, M3), the value of a `--meta` key that contains TOKEN, SECRET, PASSWORD, PASSWD or KEY
(case-insensitive) is exported as the string `"[redacted]"` and a warning names the key. The type
of `perfratio.meta` does not change (string to string), so the schema version stays the same.
Values of other fields, `--label` and the command line are not redacted.

### 2.2 `perfratio.environment_fingerprint`

`sha256:<64 hex digits>` of the text

```
perfratio-environment-fingerprint/1
cpu_model=<value>
arch=<value>
os_family=<value>
kernel_major=<value>
governor=<value>
smt=<true|false>
container=<kind|none>
```

(one `name=value` per line, each ending in a newline, an unknown value is `-`; `container` is `none`
when no container was detected). The first line is the rule version: a change of the field set or
the order is a new rule number and a contract change. Equal fingerprints mean the same hardware
class and kernel generation; `memory_bytes`, `turbo`, `numa_nodes`, `settings`, `backends` and
the perfratio build do not enter the hash.

### 2.3 `perfratio.meta`

`--meta KEY=VALUE` (repeatable, same key rules as `--label`). `--meta git` adds `git_commit` (full
hash) and `git_dirty` (`"true"` when `git status --porcelain` lists anything) of the repository in
the current directory. Outside a repository, or without git, a warning is printed and nothing is
recorded. `git_commit` and `git_dirty` cannot be given by hand together with `git`.

### 2.4 Compatibility check

`--compare FILE` and every `--import-json FILE` compare the environment of the file with this run on
the fingerprint fields. Only fields known on both sides count: a field one side could not report is not
a difference. A mismatch prints on stderr, before benchmarking:

```
Warning: The baseline 'base.json' was measured in a different environment: cpu_model (A -> B), governor (performance -> powersave). Results from different hardware classes or settings may not be comparable.
```

`--require-compatible-env` turns the warning into an error (exit code 1), and also fails for a file
that records no environment (exported before 1.22). Without the flag such a file is accepted silently.

### 2.5 JSON (`schemaVersion` 1.22, additive)

`perfratio.environment` (object), `perfratio.environment_fingerprint` (string), `perfratio.meta`
(object of strings, omitted when empty). Schema: `docs/schema/perfratio-export.schema.json`.

## 3. Implementation

`src/environment.rs` (collection with an injectable `/proc` and `/sys` root, fingerprint, differences,
`--meta`, git), `ExportMetadata`, `import::import_environment`, `check_environments` in `src/main.rs`.

## 4. Not covered

The quality check `environment_compatibility` reserved by `measurement-quality.md` is not produced:
the check runs when a benchmark finishes, the comparison is a property of two files. Compiler and
flags of the benchmarked program are not detected; they are what `--meta` is for. CPU
microarchitecture, a Windows kernel release and Windows memory size are omitted.

## 5. Tests

Unit (`src/environment.rs`): fixture trees for topology, SMT, governors, NUMA, memory, containers; the
fingerprint is stable, follows governor and SMT, ignores other fields, and has a pinned rule; field
differences; `--meta` parsing; git output parsing. Integration (`tests/environment_tests.rs`): the export
block and fingerprint, that no host name, user name or environment variable is exported, `--meta`,
`--meta git` in a temporary repository, `--compare` and `--import-json` mismatches, `--require-compatible-env`.
