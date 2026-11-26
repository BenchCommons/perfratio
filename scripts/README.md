This folder contains scripts that can be used in combination with perfratio's `--export-json` option.

### Example:

```bash
perfratio 'sleep 0.020' 'sleep 0.021' 'sleep 0.022' --export-json sleep.json
./plot_whisker.py sleep.json
```

### Pre-requisites

To make these scripts work, you will need `numpy`, `matplotlib` and `scipy`.

If you have a Python package manager that understands [PEP-723](https://peps.python.org/pep-0723/)
inline script requirements like [`uv`](https://github.com/astral-sh/uv) or [`pipx`](https://github.com/pypa/pipx),
you can directly run the scripts using

```bash
uv run plot_whisker.py sleep.json
```

Otherwise, install the dependencies via your system package manager or using `pip`:

```bash
pip install numpy matplotlib scipy  # pip3, if you are using python3
```

### `perf_parity.py`: check `--counters` against `perf stat`

Linux, `perf` and a readable PMU (bare metal or a self-hosted runner). It runs a shell loop, a
fork tree, a pipeline and `true` under both `perf stat -e instructions:u,cycles:u` and
`perfratio --counters`, and checks the counts within a tolerance and that child processes are
counted. No third-party packages are needed.

```bash
cargo build --release
python3 scripts/perf_parity.py --perfratio target/release/perfratio
# the same through cargo:
cargo test --locked --test perf_parity_tests -- --ignored --nocapture
```

Exit status `0` means every check passed, `1` a check failed and `77` means skipped (no `perf`,
no PMU, a hybrid CPU without `--pin <cpu>`). A skip is never reported as a pass.
`python3 scripts/perf_parity.py --self-test` tests the harness logic without a PMU. Details:
[docs/specs/perf-parity-harness.md](../docs/specs/perf-parity-harness.md).
