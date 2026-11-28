# Region timing: client guide

`perfratio --region=NAME ./program` reports only the time the program spends between its own
`start("NAME")` and `stop("NAME")` calls. Start-up of the runtime, JIT warm-up, loading data and
shutdown stay out. The whole-process time is still exported next to it, so both can be compared.
The protocol is specified in [specs/region-protocol.md](specs/region-protocol.md); the shipped
clients are in [`sdk/`](../sdk/README.md) (spec: [specs/region-sdks.md](specs/region-sdks.md)).

## Use a shipped client

```c
#define PERFRATIO_REGION_IMPLEMENTATION      /* in exactly one source file */
#include "perfratio_region.h"

perfratio_region_start("sort");
sort(data);
perfratio_region_stop("sort");
```

```cpp
{ perfratio::Region region("sort"); sort(data); }   // stops at scope exit, also on exceptions
```

```rust
{ let _region = perfratio_region::region("sort"); sort(&mut data); }  // stops on drop and panic
```

```js
pr.region('sort', () => sort(data));                  // JavaScript and TypeScript; stops on throw
```

```go
defer pr.Flush()                                      // Go: first line of main
func() { defer pr.Region("sort")(); sort(data) }()
```

```java
try (PerfratioRegion.Scope s = PerfratioRegion.region("sort")) { sort(data); }
```

```kotlin
PerfratioRegion.region("sort") { sort(data) }
```

```python
with perfratio_region.region("sort"):
    sort(data)
```

Then run:

```console
$ perfratio --region=sort --export-json out.json ./program
$ perfratio --metric region:sort ./impl-a ./impl-b      # rank on the region
```

A bare `--region` measures the region named `default` (the name the clients use when you give
none).

## What you get

- `results[].regions[]` in the JSON export: `mean`, `stddev`, `min`, `max` and the per-run
  `times` of every region, plus `count_per_run` (a loop that starts and stops the same region
  many times is summed per run). Regions of different names may nest or overlap.
- `results[].times` and `mean` keep measuring the whole process.
- CPU time, peak memory, hardware counters and energy are **whole process** values, not
  region values.
- A command that does not call the client fails with an error; a run that skips a requested
  region is exported as `null`, never as 0.

## Guarantees and limits of the clients

- **No-op without perfratio.** Unless `PERFRATIO_REGION_FILE` names a file, every call returns at
  once and nothing is written. Instrumented programs run normally on their own, and warmup runs
  of perfratio are not recorded.
- **Monotonic clock.** C uses `CLOCK_MONOTONIC` (`QueryPerformanceCounter` on Windows), Rust
  `std::time::Instant`, Python `time.monotonic_ns`. Never use a wall clock.
- **Nothing is written inside a region.** Events are buffered in memory and written when the
  program exits, or on `flush()`. A stop reads the clock before the bookkeeping and a start
  after it, so the bookkeeping is not charged to the region. The exception is
  `PERFRATIO_REGION_LIVE=1`, which perfratio sets for `--region-scope`
  (`docs/specs/region-scope.md`): every `start` and `stop` is then flushed at once, so perfratio
  can read the CPU time, energy and counters of the child at the marker. A client for another
  language supports it by calling its `flush()` after recording an event when the variable is `1`.
- **Flush on exit and on error.** Exit handlers write the buffer after `exit()`, `return` from
  `main`, `sys.exit`, an uncaught Python exception and a Rust panic that unwinds. The scoped forms
  stop the region while an exception or panic unwinds. `abort()`, fatal signals, `os._exit` and
  `panic = "abort"` skip exit handlers: call `flush()` first if you need the events then.
  An uncaught C++ exception also ends in `std::terminate`; catch it in `main`.
- **Threads.** Protocol 1 allows one thread per region name. The Rust and Python clients are
  thread-safe; the C client is not (serialize the calls).
- **Names** are 1 to 64 ASCII letters, digits or `_`. A client ignores an invalid name and prints
  one warning on stderr.
- **Version.** Each client sends the protocol version it speaks and an id such as
  `rust/1.1.0`. A client of another protocol version makes perfratio stop with a clear message;
  the id appears as `sdk` in the export.

## Persistent mode

`perfratio --persistent -w 0 -r 100 python3 program.py` starts the program once and sends it
100 iterations; the program times each one itself, so imports, JIT warm-up and data loading
are paid once and the JSON reports the warm-up phase and the steady state
([specs/persistent-mode.md](specs/persistent-mode.md)). The Rust, Python, JavaScript, Go, Java
and Kotlin clients give you the loop:

```python
pr.serve(handle_request)          # calls handle_request() once per iteration
```

```rust
perfratio_region::serve(|| handle_request());
```

```js
await pr.serve(handleRequest);      // JavaScript (also async functions)
```

Go `pr.Serve(handleRequest)`, Java `PerfratioRegion.serve(runnable)`, Kotlin
`PerfratioRegion.serve { handleRequest() }`.

Without perfratio `serve` calls the function once, so the program still runs on its own. A
client for another language reads `run <n>` / `quit` lines on stdin, runs the work, and
answers with `@perfratio start iteration <t_ns>`, `@perfratio stop iteration <t_ns>` and
`@perfratio done <n>` (`fail <n> <message>` on error) after a first `@perfratio hello 1 <id>`
line; see the wire format in the spec.

## Write a client for another language

About 30 lines in most languages. The client must:

1. Read `PERFRATIO_REGION_FILE`. If it is unset or empty, make `start`, `stop` and `flush` do
   nothing.
2. On `start(name)` and `stop(name)`, append `(kind, name, now_ns)` to an in-memory list, where
   `now_ns` is a **monotonic** clock in nanoseconds (an integer; the epoch does not matter).
3. On `flush()` and once more at exit, open the file for appending and write, as UTF-8 text with
   `\n` line ends:

   ```text
   hello 1 mylang/0.1.0        (only the first time)
   start sort 1203441
   stop sort 9921044
   ```

   `hello <protocol> <sdk-id>` comes first and once, even when no region ran: it tells perfratio
   that the program is instrumented. The sdk id is one token without spaces.
4. Never throw or exit because of an I/O error: a benchmark helper must not break the program.

Pseudo-code (Go-like):

```text
path = getenv("PERFRATIO_REGION_FILE")
events = []
func start(name): if path != "": events.append("start " + name + " " + monotonic_ns())
func stop(name):  if path != "": events.append("stop "  + name + " " + monotonic_ns())
func flush():
    if path == "": return
    text = (helloWritten ? "" : "hello 1 mylang/0.1.0\n") + join(events, "\n") + "\n"
    appendToFile(path, text); helloWritten = true; events = []
registerAtExit(flush)
```

Clock sources: Go `time.Since(epoch)` (monotonic), Java `System.nanoTime()`, JavaScript
`process.hrtime.bigint()` or `performance.now()`, Kotlin `System.nanoTime()`, C# `Stopwatch`,
Zig/C `clock_gettime(CLOCK_MONOTONIC)`. Test a new client with the file the real perfratio
parser reads: run the program with `PERFRATIO_REGION_FILE=/tmp/events.log`, then
`perfratio --region=NAME -N 'your-program'`. The `sdk/*/example*` programs show the check used in
CI (sleep 0.1 s, region of 0.3 s, sleep 0.1 s; the reported region time must be about 0.3 s and
the process time at least 0.15 s longer).
