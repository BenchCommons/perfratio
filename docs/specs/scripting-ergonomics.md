# Spec: Logarithmic scans, commands from stdin, `--raw-ratios`, automatic names (#327)

## Problem / Motivation
References: #327, #240, `docs/specs/parameter-scan-combinations.md`, `docs/specs/cli-short-flags.md`.

Scans over input sizes (n = 10 .. 10^7) need geometric spacing and a fixed number of points.
Hundreds of commands do not fit on a command line. Scripts want the ratios without parsing
text or JSON. Long commands that differ in one argument give unreadable tables. The ideas
come from the documented behaviour of megafine (GPL-3.0); this is an independent
implementation. No existing short flag changes.

## Contract

### Scans: `--parameter-step-n <N>` and `--parameter-step-log`
- `--parameter-step-n N` (requires `-P`, conflicts with `-D`) gives every `-P NAME MIN MAX`
  N+1 values including `MIN` and `MAX`. `N` is 1 to 100000.
- Linear value `i`: `MIN + i (MAX - MIN) / N`. With `--parameter-step-log`: `MIN (MAX / MIN)^(i/N)`;
  `MIN` must be greater than 0. `--parameter-step-log` requires `--parameter-step-n`.
- **Rounding:** if `MIN` and `MAX` are integers (`i32`), values are rounded to the nearest
  integer (half away from zero) and neighbouring duplicates are removed. Otherwise linear
  values are decimals rounded to 12 decimal places, geometric values to 6 significant
  digits, both without trailing zeros. `MIN` and `MAX` are always exact.
- **Note:** if rounding changed a value or a duplicate was removed, a line
  `Note: '-P NAME': the scan values were rounded to integers; K duplicates removed` goes to
  stderr. Example: `1..1000000` is exact for `-P n 10 1000000 --parameter-step-n 5 --parameter-step-log`
  (`10, 100, 1000, 10000, 100000, 1000000`, no note).
- Generated values appear in the existing `parameters` fields. JSON is unchanged.

### Commands from stdin: `-`
- A single `-` command argument reads one command per line from stdin. Blank lines and lines
  starting with `#` are ignored; lines are trimmed. All options apply, including parameters
  (`{name}` in a line) and `--command-name`.
- `-` with other commands, or an empty stdin (no command after filtering), is an error.
  The benchmarked commands do not read this stdin (the default input is null).

### `--raw-ratios`
- stdout holds one line per result in command order (a `--reference` command is a result
  and comes first): mean of the primary metric (`--metric`) divided by the mean of the
  reference, or of the best result without `--reference`. Format `{:.6}`, `.` as the decimal
  point regardless of locale, `nan` when the value or the reference mean is unavailable,
  zero or the command timed out. The reference is exactly `1.000000`.
- It implies `--style none` (no progress or summary). Warnings stay on stderr. It conflicts
  with `--show-output` and `--output=inherit`, and with any export to `-`.
- Not part of the JSON contract; `schemaVersion` is unaffected.

### Automatic names: `--command-name` without a value
- Applies when `--command-name` has no value or an empty one (`--command-name=`), because the
  next argument would otherwise be taken as the name; put the option after the commands or use
  `=`. Mixing automatic and explicit names is an error.
- The names are computed from the final command lines (parameters substituted): the longest
  common prefix and suffix of whole words are stripped (words split on whitespace and
  joined with single spaces), so `sort -n data.txt` and `sort -r data.txt` become `-n`, `-r`.
- If nothing can be stripped, the commands are identical or a command would be empty
  (it is a prefix of another), the names are `#1`, `#2`, ... A single command is not named.
  Names depend only on the command lines, so they are stable.

## Tests
`src/parameter/spacing.rs` (values, rounding, dedup, errors), `src/command.rs` (stdin line
parsing, name derivation incl. unicode, single and identical commands),
`tests/scan_stdin_tests.rs` (CLI, stdout purity, exports, errors).
