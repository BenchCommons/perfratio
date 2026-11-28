# perfratio region clients

Small clients of the [region timing protocol](../docs/specs/region-protocol.md). Bracket the
code you want to measure, run the program under `perfratio --region=NAME`, and perfratio
reports only that window, without start-up, JIT warm-up or shutdown.

| Language | Client | Use |
|---|---|---|
| C, C++ | [`c/perfratio_region.h`](c/perfratio_region.h) | copy the header; in one file define `PERFRATIO_REGION_IMPLEMENTATION` before including it |
| Rust | [`rust/`](rust/) (`perfratio-region`) | depend on the crate or copy `src/lib.rs` as a module |
| Python | [`python/perfratio_region.py`](python/perfratio_region.py) | copy the file next to your program |
| JavaScript, TypeScript | [`js/perfratio_region.js`](js/perfratio_region.js) + [`.d.ts`](js/perfratio_region.d.ts) | copy both files next to your program (Node.js 12+) |
| Go | [`go/`](go/) (module `github.com/BenchCommons/perfratio/sdk/go`) | import the package or copy `perfratio_region.go`; `defer Flush()` in `main` |
| Java | [`java/perfratio/PerfratioRegion.java`](java/perfratio/PerfratioRegion.java) | copy the file (Java 8+) |
| Kotlin | [`kotlin/PerfratioRegion.kt`](kotlin/PerfratioRegion.kt) | copy the file (Kotlin/JVM) |
Every client has the same shape: `start(name)`, `stop(name)`, a scoped form (RAII guard,
`with` block) and `flush()`. Each does nothing when the program does not run under perfratio.
Go has no exit handlers, so a Go program defers `Flush()`. Every directory has a runnable three-phase example (sleep 0.1 s, region of 0.3 s, sleep 0.1 s)
that CI runs against `perfratio --region`.

Every client also supports `--persistent` (`serve`, examples
`rust/examples/persistent.rs`, `python/persistent_example.py`, `js/persistent_example.js`,
`go/persistent_example/`, `java/PersistentExample.java` and `kotlin/PersistentExample.kt`): the program starts once and
perfratio drives many iterations, see [persistent mode](../docs/specs/persistent-mode.md).

See the [client guide](../docs/region-sdk.md) for usage, guarantees and how to write a client
for another language.
