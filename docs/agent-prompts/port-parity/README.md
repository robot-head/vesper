# Port parity wave: the ports adopt the Stage 1 speedups

> Plan written 2026-11-18 against `main` at 69c79b9. The
> [manuscript](../../manuscript/README.md) is the sole specification. Confirm
> live ownership on the issue board before acting.

The [Stage 1 performance wave](../stage1-perf/README.md) is merged: inlining
and `opt_level=speed` (#944), fused sequence reads and local drops (#945),
export ABI v3 (#940), copy/fill/wide byte builtins and bounds-check hoisting
(#942, #947), SIMD byte scans `sum_be16`/`eq`/`find_byte` (#948) and the
register-status `Fail` ABI (#946). Its lanes measured the ports only with
local rewrites, and none of those rewrites was committed. No port declares
`// @export abi 3`, and none calls the new builtins.

This wave puts the speedups into the ports themselves and measures each port
against its Rust build again. The ports are the test suite: whatever gap is
left after a port uses everything the compiler offers is a compiler gap, and
lane 8 turns those gaps into the next compiler wave.

## Owner's standing rule

Rust stays the default build of every port, and no port's Rust code is
deleted, until the Vesper build matches Rust in **both** functionality and
performance. No lane in this wave changes a default build or deletes Rust
code. A lane whose port reaches parity says so in its report, with the
evidence, and the owner decides.

"Parity" for the report: every port test passes on all four platforms, the
divergence tests pass, and the ABBA 95% median interval of the Vesper/Rust
ratio includes 1.0x or lies below 1.05x on the cloud x86-64 setup.

## Where the ports stand

Latest measured ratios (Vesper time / Rust time) on `main`, with the source:

| Lane | Port | Benchmark | P1 | Latest on `main` | Best local experiment |
|---|---|---|---|---|---|
| 1 | warp-wheel | `tests/abba.rs` (`--ignored`) | 8.0x | not re-measured since #944 | none |
| 2 | BBR | `examples/ack_path_abba.rs` | 11-16x | 9.2-12.1x (#946) | ~9x with ABI v3 mirror (#940) |
| 3 | Prism | `tests/malformed.rs` (instructions) | 4.5x wall | 2.6x instr. | 2.49x instr. with `find_byte` (#948) |
| 4 | warp-tcp | `examples/abba.rs` codec / transfer | 10-14x | 7.6x / 5.6x instr. | **1.19x** / 4.3x with `sum_be16` (#948) |
| 5 | Aura | `examples/abba.rs` | 3.8-5.2x | 1.75-2.43x (#946) | none |
| 6 | Ignis | none committed | 5-13x | 4.2-8.2x (local driver, #945) | none |
| 7 | Quarry | `examples/planner_abba.rs` | 4.3-4.9x | 3.17-3.3x (#946) | none |

## Lanes

Claims are #949-#956 (lane n is issue 948+n).

| Lane | Prompt | Claim | Port | Main levers | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [warp-wheel](01-warp-wheel.md) | #949 | warp-wheel | re-measure after #944/#945; `fill`/`copy_from` for slot clears; ABI v3 | now | `crates/vesper-warp-wheel/**` |
| 2 | [BBR](02-bbr.md) | #950 | BBR | ABI v3 with `repr::Bbrv3` state; handler-free `div`/`pow2` so they inline | now | `crates/vesper-warp-bbr/**` |
| 3 | [Prism](03-prism.md) | #951 | Prism | `find_byte`, `copy_from`/`extend_from_slice`, slice `eq`; a wall-clock ABBA | now | `crates/vesper-prism/**` (+ its vesper-bench dev-dependency in `Cargo.lock`) |
| 4 | [warp-tcp](04-warp-tcp.md) | #952 | warp-tcp | commit `sum_be16`, copy/fill and wide-read rewrites; the transfer path | now | `crates/vesper-warp-tcp/**` |
| 5 | [Aura](05-aura.md) | #953 | Aura | ABI v3, scratch in locals, copy/`eq` in encode/audit | now | `crates/vesper-aura/**` |
| 6 | [Ignis](06-ignis.md) | #954 | Ignis | a committed ABBA benchmark first; ABI v3, scratch in locals, copy/fill | now | `crates/vesper-ignis/**` (+ its vesper-bench dev-dependency in `Cargo.lock`) |
| 7 | [Quarry](07-quarry.md) | #955 | Quarry | scratch (`aux*`, `bits`) in locals; mirrored node slices under ABI v3 instead of the word-stream codec | now | `crates/vesper-quarry/vesper/**`, `crates/vesper-quarry/src/port.rs`, the port's tests and examples |
| 8 | [Scoreboard and next wave](08-scoreboard.md) | #956 | all | collect ratios and compiler gaps; plan the next compiler wave | after lanes 1-7 report | `docs/parallel-results/port-parity.md`, NEW `docs/agent-prompts/<next wave>/**` |

**Start now:** lanes 1-7, in any order. They touch disjoint crates.
**Merge order:** any; lane 8 last.

### Not in this wave

- Compiler changes. Lanes 1-7 edit only their port. A gap that only the
  compiler can close goes into the lane report as a compiler gap with a
  minimal `.ves` repro and the measured share of the run it costs. If a gap
  blocks a lane entirely, open an issue for it (search first) and keep going
  with the rest.
- Changing any port's default build, or deleting Rust code (owner's rule).
- Ports that were never measured against Rust in P1 (beacon, simpulse, alloc,
  tensor, grammar, semver, loom, crucible, serde codecs). Their P1 lanes own
  them.

## Rules for every lane

- **Claim first.** Search open issues and PRs for overlapping work. The lane's
  claim issue already exists; link it from the PR ("Closes #NNN"). Branch from
  `origin/main` and open the PR against `main`.
- **Port only.** Edit only the lane's exclusive boundary. Never edit `src/**`,
  `crates/vesper-port-abi/**`, `crates/vesper-port-harness/**`,
  `crates/vesper-port-build/**`, `packages/**`, `lean/**` or `bootstrap/**`.
  Use the builtins and ABI as the manuscript (`docs/manuscript/content.typ`) specifies them (§13 for the byte
  builtins and scans, the export ABI chapter for `// @export abi 3`, and
  `docs/maintenance/porting-a-crate.md`).
- **Default stays Rust.** The `vesper-port` feature stays non-default. Do not
  delete Rust code, even where the port now beats it.
- **Functionality first.** Every existing port test, divergence test and leak
  test (`port_live_allocations()`) passes with the Vesper build after each
  rewrite. A rewrite that changes behaviour is a bug, not a speedup.
- **Measure before and after, same machine, same run.** Use the port's ABBA
  benchmark (both builds in one process). Report the median Vesper/Rust ratio
  with its 95% interval, the epochs, and the setup (CPU, OS, Rust version,
  `optimize = true`). Where wall clock is noisy, add instruction counts
  (callgrind or `perf stat`). Give the ratio after each lever, not only the
  total, so lane 8 can tell which compiler work paid off. Report only what you
  measured.
- **Profile what is left.** After the rewrites, profile one operation of both
  builds and name the top Vesper costs. For each one say whether a port change
  could remove it (then do it) or only the compiler (then record it as a gap
  with a repro).
- **Before pushing:** `cargo +1.100.0 clippy --all-targets -p <port crate>
  --features vesper-port -- -D warnings` and the port's tests with and without
  `--features vesper-port`. Bazel lists files a test reads from outside its
  crate in `BUILD.bazel`; re-run `tools/bazel/sync_build_files.py` if a crate's
  file set changes. Long runs go to GitHub CI from the pushed branch.
- **Merging.** Merge when the four required checks (`Test all targets`,
  `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`,
  `Windows (x86_64 MSVC)`) are green and review comments are addressed. If
  `main` is red and your PR fixes it, cancel the other in-progress Actions runs
  and merge the fix first.
- **Report.** `docs/parallel-results/port-parity-<port>.md`: what changed in
  the port, the ratio before and after each lever with the setup, the profile
  of what is left, the compiler gaps with repros, and whether the port meets
  the parity bar above.

## Collisions

The lanes touch disjoint crates. Shared files:

| Shared file | Lanes | Rule |
|---|---|---|
| `docs/agent-prompts/port-parity/**` | none after this plan | Lanes do not edit the plan; corrections go in the lane report. |
| Root `Cargo.toml`/`Cargo.lock` | a lane adding a dev-dependency (lanes 3 and 6 add benchmarks) | Serialize. Prefer the dev-dependencies the other ports' benchmarks already use (`vesper-bench`). |
| `tools/bazel/**` generated `BUILD.bazel` | any lane that adds files | Regenerate only your crate's file. |
