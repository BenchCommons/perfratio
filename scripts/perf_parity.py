#!/usr/bin/env python3
"""perf-stat parity harness for `perfratio --counters` (#312).

Runs deterministic workloads under `perf stat` (the reference tool) and under
`perfratio --counters`, and checks that

* both report the same user-space instruction and cycle counts within a tolerance,
* the children of a process tree are counted (`inherit`): a command that starts K copies of a
  workload counts K times the workload, and
* perfratio's own fork/exec work adds nothing: `true` is within a small absolute floor.

It needs Linux, `perf` and a readable PMU, so it does not run on hosted CI. Where one of
those is missing it exits with status 77 ("skipped"), never 0. See
`docs/specs/perf-parity-harness.md`.

Exit status: 0 all checks passed, 1 a check failed, 77 skipped, 2 usage error.

Standard library only (Python 3.8+).
"""

import argparse
import json
import os
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile

EXIT_PASS, EXIT_FAIL, EXIT_SKIP, EXIT_USAGE = 0, 1, 77, 2

EVENTS = ("instructions", "cycles")

# A shell loop of about 10^8 instructions: much more than the exec and loader cost of `sh`
# (about 10^6), and short enough to run in tens of milliseconds.
LOOP_ITERATIONS = 300_000
LOOP = f"i=0; while [ $i -lt {LOOP_ITERATIONS} ]; do i=$((i+1)); done"
# fewer instructions than this and the workload is dominated by startup: the check is void
MIN_INSTRUCTIONS = 5_000_000


class Skip(Exception):
    """The host cannot run the harness (not a failure)."""


def sh_c(script):
    return ["sh", "-c", script]


def tree_script(children):
    inner = " ".join(shlex.quote(a) for a in sh_c(LOOP))
    return f"for k in $(seq {children}); do {inner} & done; wait"


def workloads(children):
    """name -> (argv, relative tolerance in percent or None for "informational")."""
    return {
        "loop": (sh_c(LOOP), 5.0),
        "tree": (sh_c(tree_script(children)), 8.0),
        "pipeline": (sh_c("seq 1 300000 | tr 1 2 | wc -l"), 10.0),
        # the child outlives the command: what is counted depends on timing
        "daemon": (sh_c(f"{shlex.join(sh_c(LOOP))} >/dev/null 2>&1 &"), None),
    }


# ---------------------------------------------------------------- parsing


def parse_perf_csv(text):
    """Parses `perf stat -x,` output into {event: (value|None, percent_counted)}.

    Hybrid CPUs prefix the event with the PMU (`cpu_core/instructions:u/`): the counts of the
    PMUs are summed. A `<not supported>` or `<not counted>` value is `None`.
    """
    counts = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        fields = line.split(",")
        if len(fields) < 3:
            continue
        raw, event = fields[0], fields[2]
        percent = None
        if len(fields) > 4:
            try:
                percent = float(fields[4])
            except ValueError:
                percent = None
        name = None
        for wanted in EVENTS:
            if wanted in event:
                name = wanted
        if name is None:
            continue
        try:
            value = int(float(raw))
        except ValueError:
            value = None
        previous, previous_percent = counts.get(name, (None, None))
        if value is None:
            counts.setdefault(name, (None, percent))
            continue
        total = value + (previous or 0)
        low = (
            percent
            if previous_percent is None
            else min(percent or 100.0, previous_percent)
        )
        counts[name] = (total, low)
    return counts


def relative_difference(measured, reference):
    """|measured - reference| / reference in percent."""
    if reference == 0:
        return float("inf") if measured else 0.0
    return abs(measured - reference) / reference * 100.0


def check_parity(name, perfratio, reference, tolerance):
    """Returns (ok, message); `tolerance` None is informational (always ok)."""
    difference = relative_difference(perfratio, reference)
    message = f"{name}: perfratio {perfratio:,} vs perf stat {reference:,} ({difference:+.2f}% apart)"
    if tolerance is None:
        return True, message + " [informational]"
    return difference <= tolerance, message + f" (tolerance {tolerance:g}%)"


def check_tree(tree, single, children, tolerance):
    """K children of `single` instructions each: tree is K*single plus a small parent cost."""
    expected = children * single
    low, high = expected * (1 - tolerance / 100), expected * (1 + tolerance / 100)
    ok = low <= tree <= high
    return ok, (
        f"tree: perfratio {tree:,} vs {children} x loop = {expected:,} "
        f"(allowed {int(low):,}..{int(high):,})"
    )


def check_floor(perfratio, reference, floor):
    ok = abs(perfratio - reference) <= floor
    return ok, (
        f"runner floor: `true` perfratio {perfratio:,} vs perf stat {reference:,}, "
        f"difference {abs(perfratio - reference):,} (allowed {floor:,})"
    )


# ---------------------------------------------------------------- running tools


def pinned(argv, cpu):
    return ["taskset", "-c", str(cpu), *argv] if cpu is not None else argv


def run_perf_stat(argv, runs, cpu):
    """Median {event: count} over `runs` separate `perf stat` runs of `argv`."""
    series = {event: [] for event in EVENTS}
    for _ in range(runs):
        with tempfile.NamedTemporaryFile("r", suffix=".csv") as out:
            cmd = ["perf", "stat", "-x,", "-o", out.name]
            cmd += ["-e", ",".join(f"{e}:u" for e in EVENTS), "--", *argv]
            subprocess.run(
                pinned(cmd, cpu),
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            counts = parse_perf_csv(out.read())
        for event in EVENTS:
            value, percent = counts.get(event, (None, None))
            if value is None:
                raise Skip(
                    f"perf stat could not count {event} (no PMU or not permitted)"
                )
            if percent is not None and percent < 99.0:
                print(
                    f"warning: perf stat counted {event} for {percent:.1f}% of the run "
                    "(multiplexed); results may differ",
                    file=sys.stderr,
                )
            series[event].append(value)
    return {event: int(statistics.median(values)) for event, values in series.items()}


def run_perfratio(binary, argv, runs, cpu):
    """Median {event: count} from `perfratio --counters` over `runs` runs of `argv`."""
    with tempfile.TemporaryDirectory() as tmp:
        export = os.path.join(tmp, "out.json")
        cmd = [
            binary,
            "-N",
            f"--runs={runs}",
            "--warmup=1",
            "--style=none",
            "--counters=" + ",".join(f"{e}:u" for e in EVENTS),
            "--export-json",
            export,
            shlex.join(argv),
        ]
        done = subprocess.run(
            pinned(cmd, cpu), capture_output=True, text=True, check=False
        )
        if done.returncode != 0 or not os.path.exists(export):
            raise RuntimeError(
                f"perfratio failed ({done.returncode}):\n{done.stderr.strip()}"
            )
        with open(export, encoding="utf-8") as handle:
            document = json.load(handle)
    status = document["perfratio"]["measurements"]["counters"]
    if status != "measured":
        reason = document["perfratio"]["measurements"].get(
            "counters_unavailable_reason"
        )
        raise RuntimeError(
            f"perfratio reports counters `{status}` ({reason}) while perf stat works"
        )
    summary = document["results"][0]["counters_summary"]
    return {event: int(summary[f"{event}:u"]["median"]) for event in EVENTS}


# ---------------------------------------------------------------- host checks


def check_host(binary, cpu):
    if not sys.platform.startswith("linux"):
        raise Skip("needs Linux (perf_event_open)")
    if shutil.which("perf") is None:
        raise Skip("`perf` is not installed")
    if cpu is not None and shutil.which("taskset") is None:
        raise Skip("`taskset` is needed for --pin")
    if cpu is None and os.path.isdir("/sys/devices/cpu_core"):
        raise Skip(
            "hybrid CPU (P/E cores): counts depend on the core type, pass --pin <cpu> to "
            "run both tools on one core"
        )
    if shutil.which(binary) is None and not os.path.exists(binary):
        raise RuntimeError(f"perfratio binary not found: {binary}")
    probe = subprocess.run(
        ["perf", "stat", "-x,", "-e", "instructions:u,cycles:u", "--", "true"],
        capture_output=True,
        text=True,
        check=False,
    )
    counts = parse_perf_csv(probe.stderr + probe.stdout)
    if probe.returncode != 0 or any(counts.get(e, (None,))[0] is None for e in EVENTS):
        raise Skip(
            "perf stat cannot count instructions:u and cycles:u here (VM without a PMU, "
            "kernel.perf_event_paranoid too high, or a container without perf_event_open)"
        )


# ---------------------------------------------------------------- main


def run(args):
    check_host(args.perfratio, args.pin)
    results = []

    def record(ok, message):
        results.append({"ok": ok, "message": message})
        print(("PASS  " if ok else "FAIL  ") + message)

    wl = workloads(args.children)
    measured = {}
    for name, (argv, tolerance) in wl.items():
        reference = run_perf_stat(argv, args.runs, args.pin)
        ours = run_perfratio(args.perfratio, argv, args.runs, args.pin)
        measured[name] = (ours, reference)
        for event in EVENTS:
            if (
                name != "daemon"
                and reference[event] < MIN_INSTRUCTIONS
                and event == "instructions"
            ):
                record(
                    False,
                    f"{name}: only {reference[event]:,} instructions, the workload is void",
                )
                continue
            ok, message = check_parity(
                f"{name} {event}", ours[event], reference[event], tolerance
            )
            record(ok, message)

    # child-tree accounting: K children each running the loop
    loop_single = measured["loop"][1]["instructions"]
    ok, message = check_tree(
        measured["tree"][0]["instructions"],
        loop_single,
        args.children,
        args.tree_tolerance,
    )
    record(ok, message)

    # runner floor: `true` under perfratio is within an absolute floor of perf stat
    true_ref = run_perf_stat(["true"], args.runs, args.pin)
    true_ours = run_perfratio(args.perfratio, ["true"], args.runs, args.pin)
    ok, message = check_floor(
        true_ours["instructions"], true_ref["instructions"], args.floor
    )
    record(ok, message)

    failed = [r for r in results if not r["ok"]]
    if args.json:
        with open(args.json, "w", encoding="utf-8") as handle:
            json.dump(
                {"passed": not failed, "checks": results, "children": args.children},
                handle,
                indent=2,
            )
    print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
    return EXIT_FAIL if failed else EXIT_PASS


def self_test():
    """Checks the parsing and comparison logic; needs neither perf nor a PMU."""
    text = "1234567,,instructions:u,1000,100.00,,\n2000000,,cycles:u,1000,100.00,,\n"
    assert parse_perf_csv(text) == {
        "instructions": (1234567, 100.0),
        "cycles": (2000000, 100.0),
    }
    hybrid = (
        "100,,cpu_core/instructions:u/,1,100.00,,\n"
        "50,,cpu_atom/instructions:u/,1,100.00,,\n"
    )
    assert parse_perf_csv(hybrid)["instructions"][0] == 150
    unsupported = "<not supported>,,instructions:u,0,100.00,,\n"
    assert parse_perf_csv(unsupported)["instructions"][0] is None
    multiplexed = "10,,instructions:u,1,62.50,,\n"
    assert parse_perf_csv(multiplexed)["instructions"] == (10, 62.5)
    assert parse_perf_csv("# started on Mon\n\n") == {}

    assert relative_difference(105, 100) == 5.0
    assert relative_difference(0, 0) == 0.0
    assert check_parity("x", 104, 100, 5.0)[0]
    assert not check_parity("x", 106, 100, 5.0)[0]
    assert check_parity("x", 1, 100, None)[0]
    assert check_tree(4_100_000, 1_000_000, 4, 8.0)[0]
    assert not check_tree(1_100_000, 1_000_000, 4, 8.0)[0], "children not counted"
    assert not check_tree(5_000_000, 1_000_000, 4, 8.0)[0]
    assert check_floor(300_000, 200_000, 500_000)[0]
    assert not check_floor(900_000, 100_000, 500_000)[0]
    assert tree_script(3).count("& done") == 1 and "seq 3" in tree_script(3)
    assert set(workloads(2)) == {"loop", "tree", "pipeline", "daemon"}
    print("self-test ok")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument(
        "--perfratio",
        default=os.environ.get("PERFRATIO_BIN", "perfratio"),
        help="path of the perfratio binary (default: $PERFRATIO_BIN or PATH)",
    )
    parser.add_argument(
        "--runs", type=int, default=10, help="runs per tool and workload"
    )
    parser.add_argument(
        "--children", type=int, default=4, help="children of the `tree` workload"
    )
    parser.add_argument(
        "--tree-tolerance",
        type=float,
        default=8.0,
        help="percent around children x loop (default 8)",
    )
    parser.add_argument(
        "--floor",
        type=int,
        default=500_000,
        help="absolute instructions allowed for `true` (default 500000)",
    )
    parser.add_argument(
        "--pin",
        type=int,
        metavar="CPU",
        help="run both tools on one CPU with taskset (required on hybrid CPUs)",
    )
    parser.add_argument("--json", metavar="FILE", help="write the checks as JSON")
    parser.add_argument(
        "--self-test", action="store_true", help="test the harness logic only"
    )
    args = parser.parse_args(argv)
    if args.runs < 1 or args.children < 2:
        parser.error("--runs must be >= 1 and --children >= 2")
    if args.self_test:
        self_test()
        return EXIT_PASS
    try:
        return run(args)
    except Skip as reason:
        print(f"SKIPPED: {reason}")
        return EXIT_SKIP
    except RuntimeError as error:
        print(f"FAIL  {error}")
        return EXIT_FAIL


if __name__ == "__main__":
    sys.exit(main())
