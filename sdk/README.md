# perfratio region clients

Small clients of the [region timing protocol](../docs/specs/region-protocol.md). Bracket the
code you want to measure, run the program under `perfratio --region=NAME`, and perfratio
reports only that window, without start-up, JIT warm-up or shutdown.

| Language | Client | Use |
|---|---|---|
| C, C++ | [`c/perfratio_region.h`](c/perfratio_region.h) | copy the header; in one file define `PERFRATIO_REGION_IMPLEMENTATION` before including it |
| Rust | [`rust/`](rust/) (`perfratio-region`) | depend on the crate or copy `src/lib.rs` as a module |
| Python | [`python/perfratio_region.py`](python/perfratio_region.py) | copy the file next to your program |

Every client has the same shape: `start(name)`, `stop(name)`, a scoped form (RAII guard,
`with` block) and `flush()`. Each does nothing when the program does not run under perfratio.
Every directory has a runnable three-phase example (sleep 0.1 s, region of 0.3 s, sleep 0.1 s)
that CI runs against `perfratio --region`.

See the [client guide](../docs/region-sdk.md) for usage, guarantees and how to write a client
for another language.
