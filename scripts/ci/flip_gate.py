#!/usr/bin/env python3
"""The flip gate (Rust-exit wave SH6, lane 1, #1396).

Owner's bar (2026-11-21): a crate or compiler module flips to Vesper, and its
Rust is deleted, once the Vesper build has full function (every test passes
and the differential finds 0 differences) and runs within 1.5x of Rust's
time (the upper end of the ABBA 95% interval). This script checks both and
prints a one-line verdict plus a Markdown row for the census and the
scoreboard in docs/maintenance/rust-exit-plan.md.

Usage:
  flip_gate.py <crate-or-module> [--config FILE] [--only tests,differential,abba]
               [--epochs N] [--threshold 1.5] [--portable] [--json FILE] [--dry-run]

The configuration is TOML. Without --config the gate reads
crates/<name>/flip-gate.toml, then packages/<name>/flip-gate.toml, then
scripts/ci/flip-gate/<name>.toml (for compiler modules). Every command runs
with /bin/sh from the repository root; `env` adds variables to all of them.

  name = "vesper-span"
  lane = "SH0-6"                      # optional, for the Markdown row
  rust = ["crates/vesper-span/src/lib.rs"]  # optional: Rust the flip deletes
  env = { CARGO_PROFILE_DEV_DEBUG = "0" }
  build = ["cargo build --locked --release -p vesper-span --example abba"]

  [[tests]]                           # each must exit 0
  run = "cargo test --locked -p vesper-span -p vesper-span-seed"

  [differential]                      # must exit 0
  run = "cargo test --locked -p vesper-span --test differential"

  [[abba]]                            # one table per workload
  workload = "lookup"
  rust = "target/release/examples/abba once lookup rust 400"
  vesper = "target/release/examples/abba once lookup vesper 400"
  epochs = 30                         # optional, default 30

  [[abba]]                            # or a harness that does its own ABBA
  workload = "index"
  run = "target/release/examples/abba flip-gate index"

Host CPU. The `build` and `[[abba]]` commands run with
RUSTFLAGS="-C target-cpu=native" added to any RUSTFLAGS already set, so the
Rust half and the port objects (which the port harness builds for the outer
build's CARGO_CFG_TARGET_FEATURE, #1454) both get the host's CPU features;
a benchmark that gave only Rust AVX2 or SSSE3 would not compare the two
implementations. Tests and the differential keep the caller's RUSTFLAGS.
`--portable`, or `native = false` in the configuration, turns this off.
Benchmark builds should use their own target directory or `--release`, so
the flag change does not rebuild the test artifacts.

Differential protocol: when the command prints a line
  flip-gate differential differences=<D> cases=<C>
the gate uses it (D must be 0); otherwise the exit status alone decides and
the case count is reported as unknown. A harness that exits 0 must not hide
differences, so a printed D > 0 fails even with exit status 0.

ABBA protocol. With `rust` and `vesper`, the gate times the two commands
itself: each epoch runs Rust, Vesper, Vesper, Rust (wall clock, after one
untimed warm-up of each) and records the pair (Rust total, Vesper total).
Use workloads long enough (tens of milliseconds or more) that process start
does not dominate. With `run`, the harness does the timing and prints
  flip-gate pairs <workload> <rust_ns>:<vesper_ns> <rust_ns>:<vesper_ns> ...
(one pair per epoch, equal operations in both arms). Either way the gate
computes vesper-bench's exact median interval (crates/vesper-bench/src/
statistics.rs, `analyze_pairs`): the epoch ratios Vesper/Rust are sorted, and
the interval is the order statistics at the binomial(n, 1/2) ranks with at
most 2.5% in each tail (at least six epochs). A workload passes when the
interval's upper end is at or below the threshold, is a regression when its
lower end is above it, and is inconclusive otherwise.

Verdict: PASS (exit 0) when every check ran and passed; FAIL (exit 1) on a
failing test, a difference, or an ABBA regression; INCONCLUSIVE (exit 2)
when an interval crosses the threshold, or a check was skipped with --only.
A configuration error exits 3.
"""

import argparse
import json
import math
import os
import re
import subprocess
import sys
import time
import tomllib
from fractions import Fraction
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CHECKS = ("tests", "differential", "abba")
DEFAULT_EPOCHS = 30
MIN_EPOCHS = 6
NATIVE_RUSTFLAGS = "-C target-cpu=native"
DIFF_LINE = re.compile(r"^flip-gate differential differences=(\d+) cases=(\d+)\s*$", re.M)
PAIRS_LINE = re.compile(r"^flip-gate pairs (\S+)((?: \d+:\d+)+)\s*$", re.M)


class ConfigError(Exception):
    pass


def find_config(name, root):
    for path in (root / "crates" / name / "flip-gate.toml",
                 root / "packages" / name / "flip-gate.toml",
                 root / "scripts/ci/flip-gate" / f"{name}.toml"):
        if path.is_file():
            return path
    raise ConfigError(f"no flip-gate.toml for {name} (crates/{name}/, packages/{name}/ "
                      f"or scripts/ci/flip-gate/{name}.toml); pass --config")


def load_config(path):
    try:
        config = tomllib.loads(Path(path).read_text())
    except (OSError, tomllib.TOMLDecodeError) as err:
        raise ConfigError(f"{path}: {err}") from err
    for key in ("build",):
        if not isinstance(config.get(key, []), list):
            raise ConfigError(f"{path}: {key} must be a list of commands")
    for test in config.get("tests", []):
        if "run" not in test:
            raise ConfigError(f"{path}: every [[tests]] needs run")
    if "differential" in config and "run" not in config["differential"]:
        raise ConfigError(f"{path}: [differential] needs run")
    for bench in config.get("abba", []):
        if "workload" not in bench:
            raise ConfigError(f"{path}: every [[abba]] needs workload")
        timed = "rust" in bench and "vesper" in bench
        if timed == ("run" in bench):
            raise ConfigError(f"{path}: abba {bench['workload']} needs rust and vesper, or run")
    return config


# ---------------------------------------------------------------- statistics


def median_interval(pairs):
    """vesper-bench's exact median interval of the ratios vesper/rust.

    Returns (lower, upper, median) as Fractions, or None for lower/upper when
    there are too few epochs for a finite interval (fewer than six)."""
    if not pairs:
        raise ValueError("no ABBA pairs")
    ratios = sorted(Fraction(v, r) for r, v in pairs)
    n = len(ratios)
    median = (ratios[(n - 1) // 2] + ratios[n // 2]) / 2
    total = 1 << n
    coefficient, tail, rank = 1, 0, None
    for j in range(n // 2):
        tail += coefficient
        if tail * 200 <= total * 5:
            rank = j + 1
        else:
            break
        coefficient = coefficient * (n - j) // (j + 1)
    if rank is None:
        return None, None, median
    return ratios[rank - 1], ratios[n - rank], median


def decide(lower, upper, threshold):
    if upper is None:
        return "inconclusive"
    if upper <= threshold:
        return "pass"
    if lower > threshold:
        return "regression"
    return "inconclusive"


# ---------------------------------------------------------------- running


def bench_env(env, native):
    """The environment of the build and ABBA commands: the host's CPU for
    both halves unless --portable."""
    if not native:
        return env
    flags = env.get("RUSTFLAGS", "").split()
    if "target-cpu=native" not in " ".join(flags):
        flags += NATIVE_RUSTFLAGS.split()
    return {**env, "RUSTFLAGS": " ".join(flags)}


class Runner:
    def __init__(self, env, dry_run, log, native=True):
        self.env = {**os.environ, **{k: str(v) for k, v in env.items()}}
        self.bench = bench_env(self.env, native)
        self.dry_run = dry_run
        self.log = log

    def run(self, command, capture=True, bench=False):
        env = self.bench if bench else self.env
        prefix = f"RUSTFLAGS={env.get('RUSTFLAGS', '')!r} " if bench and env is not self.env else ""
        self.log(f"$ {prefix}{command}")
        if self.dry_run:
            return 0, ""
        proc = subprocess.run(command, shell=True, cwd=ROOT, env=env, text=True,
                              stdout=subprocess.PIPE if capture else None,
                              stderr=subprocess.STDOUT if capture else None)
        return proc.returncode, proc.stdout or ""

    def time(self, command):
        start = time.perf_counter_ns()
        status, out = self.run(command, bench=True)
        elapsed = time.perf_counter_ns() - start
        if status != 0:
            raise RuntimeError(f"`{command}` exited {status}\n{out[-2000:]}")
        return max(elapsed, 1)


def timed_pairs(runner, bench, epochs):
    rust, vesper = bench["rust"], bench["vesper"]
    if runner.dry_run:
        runner.run(rust, bench=True)
        runner.run(vesper, bench=True)
        return []
    runner.time(rust)
    runner.time(vesper)
    pairs = []
    for _ in range(epochs):
        a1 = runner.time(rust)
        b1 = runner.time(vesper)
        b2 = runner.time(vesper)
        a2 = runner.time(rust)
        pairs.append((a1 + a2, b1 + b2))
    return pairs


def harness_pairs(runner, bench):
    status, out = runner.run(bench["run"], bench=True)
    if runner.dry_run:
        return []
    if status != 0:
        raise RuntimeError(f"`{bench['run']}` exited {status}\n{out[-2000:]}")
    for match in PAIRS_LINE.finditer(out):
        if match.group(1) == bench["workload"]:
            pairs = []
            for item in match.group(2).split():
                r, v = (int(x) for x in item.split(":"))
                if r == 0 or v == 0:
                    raise RuntimeError(f"abba {bench['workload']}: a zero time in {item}")
                pairs.append((r, v))
            return pairs
    raise RuntimeError(f"abba {bench['workload']}: no `flip-gate pairs {bench['workload']} ...` "
                       f"line in the output of `{bench['run']}`")


def rust_lines(config, root):
    """Non-blank lines of .rs files under the configured `rust` paths."""
    paths = config.get("rust")
    if not paths:
        return None
    count = 0
    for entry in paths:
        path = root / entry
        if not path.exists():
            raise ConfigError(f"rust path {entry} does not exist")
        files = [path] if path.is_file() else sorted(path.rglob("*.rs"))
        for f in files:
            count += sum(1 for line in f.read_text(errors="replace").splitlines() if line.strip())
    return count


def gate(name, config, only, epochs_override, threshold, runner, lines=None):
    result = {"name": name, "lane": config.get("lane", ""), "threshold": str(threshold),
              "tests": None, "differential": None, "abba": [], "skipped": [],
              "rust_lines": lines, "rustflags": runner.bench.get("RUSTFLAGS", "")}
    for command in config.get("build", []):
        status, out = runner.run(command, bench=True)
        if status != 0:
            result["error"] = f"build `{command}` exited {status}\n{out[-2000:]}"
            return finish(result)

    if "tests" in only and config.get("tests"):
        rows = []
        for test in config["tests"]:
            status, out = runner.run(test["run"])
            rows.append({"run": test["run"], "ok": status == 0,
                         "tail": "" if status == 0 else out[-2000:]})
        result["tests"] = rows
    else:
        result["skipped"].append("tests")

    if "differential" in only and "differential" in config:
        command = config["differential"]["run"]
        status, out = runner.run(command)
        matches = DIFF_LINE.findall(out)
        diff = {"run": command, "status": status, "differences": None, "cases": None}
        if matches:
            diff["differences"] = sum(int(d) for d, _ in matches)
            diff["cases"] = sum(int(c) for _, c in matches)
        diff["ok"] = status == 0 and (diff["differences"] or 0) == 0
        if not diff["ok"]:
            diff["tail"] = out[-2000:]
        result["differential"] = diff
    else:
        result["skipped"].append("differential")

    if "abba" in only and config.get("abba"):
        for bench in config["abba"]:
            row = {"workload": bench["workload"]}
            try:
                if "run" in bench:
                    pairs = harness_pairs(runner, bench)
                else:
                    pairs = timed_pairs(runner, bench,
                                        epochs_override or bench.get("epochs", DEFAULT_EPOCHS))
            except RuntimeError as err:
                row.update(decision="error", error=str(err))
                result["abba"].append(row)
                continue
            if runner.dry_run:
                row.update(decision="dry-run")
            else:
                lower, upper, median = median_interval(pairs)
                row.update(epochs=len(pairs), median=float(median),
                           lower=None if lower is None else float(lower),
                           upper=None if upper is None else float(upper),
                           decision=decide(lower, upper, threshold))
            result["abba"].append(row)
    else:
        result["skipped"].append("abba")
    return finish(result)


def finish(result):
    failed = ("error" in result
              or any(not t["ok"] for t in result["tests"] or [])
              or (result["differential"] is not None and not result["differential"]["ok"])
              or any(b["decision"] in ("regression", "error") for b in result["abba"]))
    if failed:
        verdict = "FAIL"
    elif result["skipped"] or any(b["decision"] != "pass" for b in result["abba"]):
        verdict = "INCONCLUSIVE"
    else:
        verdict = "PASS"
    result["verdict"] = verdict
    return result


# ---------------------------------------------------------------- reporting


def ratio(x):
    return "-" if x is None else f"{x:.2f}x"


def describe_differential(diff):
    if diff is None:
        return "not run"
    if diff["differences"] is not None:
        return f"{diff['differences']} differences on {diff['cases']:,} cases"
    return "0 differences" if diff["ok"] else f"failed (exit {diff['status']})"


def describe_abba(rows):
    if not rows:
        return "not run"
    parts = []
    for b in rows:
        if "median" in b:
            parts.append(f"{b['workload']} {ratio(b['median'])} "
                         f"[{ratio(b['lower'])}, {ratio(b['upper'])}] n={b['epochs']}")
        else:
            parts.append(f"{b['workload']} {b['decision']}")
    return "; ".join(parts)


def describe_tests(rows):
    if rows is None:
        return "not run"
    return f"{sum(t['ok'] for t in rows)}/{len(rows)} pass"


def verdict_line(result):
    line = (f"flip-gate {result['name']}: {result['verdict']} (bar: 0 differences, every test, "
            f"95% upper <= {float(Fraction(result['threshold'])):.2f}x) | tests "
            f"{describe_tests(result['tests'])} | differential "
            f"{describe_differential(result['differential'])} | ABBA vesper/rust "
            f"{describe_abba(result['abba'])}")
    if result["abba"] and result.get("rustflags"):
        line += f" | RUSTFLAGS={result['rustflags']}"
    if "error" in result:
        line += f" | {result['error'].splitlines()[0]}"
    return line


def markdown_row(result):
    """A row for the scoreboard of docs/maintenance/rust-exit-plan.md:
    | Flip | Lane | Differential | Speed (ABBA) | Rust deleted | PR |"""
    lines = result["rust_lines"]
    speed = describe_abba(result["abba"]).replace("|", "/")
    return (f"| `{result['name']}` ({result['verdict']}) | {result['lane']} | "
            f"{describe_differential(result['differential'])}; tests "
            f"{describe_tests(result['tests'])} | {speed} | "
            f"{'' if lines is None else f'{lines:,}'} | |")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("name", help="crate or compiler module")
    parser.add_argument("--config", type=Path)
    parser.add_argument("--only", default=",".join(CHECKS),
                        help="comma-separated subset of tests,differential,abba")
    parser.add_argument("--epochs", type=int, help="override every timed workload's epochs")
    parser.add_argument("--threshold", default="1.5", help="Vesper/Rust bar (default 1.5)")
    parser.add_argument("--portable", action="store_true",
                        help=f"do not add RUSTFLAGS={NATIVE_RUSTFLAGS!r} to build and ABBA commands")
    parser.add_argument("--json", type=Path, help="write the full result as JSON")
    parser.add_argument("--dry-run", action="store_true", help="print the commands only")
    parser.add_argument("--quiet", action="store_true", help="do not echo commands")
    args = parser.parse_args(argv)

    try:
        only = {c.strip() for c in args.only.split(",") if c.strip()}
        if not only or only - set(CHECKS):
            raise ConfigError(f"--only takes {','.join(CHECKS)}")
        if args.epochs is not None and args.epochs < MIN_EPOCHS:
            raise ConfigError(f"--epochs must be at least {MIN_EPOCHS}")
        try:
            threshold = Fraction(args.threshold)
        except ValueError as err:
            raise ConfigError(f"--threshold: {err}") from err
        if threshold <= 0 or not math.isfinite(float(threshold)):
            raise ConfigError("--threshold must be positive")
        config = load_config(args.config or find_config(args.name, ROOT))
        lines = rust_lines(config, ROOT)
    except ConfigError as err:
        print(f"flip-gate {args.name}: configuration error: {err}", file=sys.stderr)
        return 3

    log = (lambda _msg: None) if args.quiet else (lambda msg: print(msg, file=sys.stderr))
    native = not args.portable and config.get("native", True)
    runner = Runner(config.get("env", {}), args.dry_run, log, native)
    result = gate(config.get("name", args.name), config, only, args.epochs, threshold, runner,
                  lines)
    for test in result["tests"] or []:
        if not test["ok"]:
            print(f"--- failing test command: {test['run']}\n{test['tail']}", file=sys.stderr)
    if result["differential"] and not result["differential"]["ok"]:
        print(f"--- differential:\n{result['differential'].get('tail', '')}", file=sys.stderr)
    for bench in result["abba"]:
        if "error" in bench:
            print(f"--- abba {bench['workload']}: {bench['error']}", file=sys.stderr)
    print(verdict_line(result))
    print(markdown_row(result))
    if args.json:
        args.json.write_text(json.dumps(result, indent=2) + "\n")
    return {"PASS": 0, "FAIL": 1, "INCONCLUSIVE": 2}[result["verdict"]]


if __name__ == "__main__":
    sys.exit(main())
