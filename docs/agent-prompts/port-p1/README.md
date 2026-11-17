# Port wave P1: the port-first crates move to Vesper

> Plan written 2026-11-17 against `main` at d523661. The
> [manuscript](../../manuscript/README.md) is the sole specification. The order
> and the crate classes come from the
> [port survey](../../maintenance/vesper-port-survey.md) (#869), and every lane
> follows the [porting recipe](../../maintenance/porting-a-crate.md) (#878).
> Confirm live ownership on the issue board before acting.

Wave P1 ports the 16 crates the survey classed as **port-first**, plus the
scalar half of `vesper-serde`, which the owner asked to bring forward. It also
adds two porting-kit lanes that every port needs and that P0 did not cover.
Each port lane writes the crate's public API in Vesper, runs the crate's
existing Rust tests against both builds, and deletes the Rust implementation
once they match on all four required CI platforms (with the exception in
"Deleting the Rust code" below).

The survey split these crates into its waves P1 (pilots and the three crates
with specs) and P2 (pure protocol and logic crates). This plan runs both as one
wave of independent lanes, gated per lane by the features each crate needs,
because the gating features are now known exactly and most of them have merged.

## What P0 left, and what this wave adds

Merged on `main`: packages and `use` (F0, #881), `ChunkSeq`/`Slice` (F1, #882),
integer widths (F3, #885), `f32` and math (F4, #893), loops (F6, #884), bounded
generics with builtin `Eq`/`Ord`/`Hash` (F7a, #883), and the export ABI with the
harness (F8, #879).

Starting now, in parallel with this plan:

- **Strings** (F2), P0 lane 7, claim **#896**: `String`/`Str`.
- **`@vesper-std`** (F5 + kit), P0 lane 8, claim **#897**: `core`, `collections`
  (`CacheBTree`, `FlatTable`, sets), `bytes`, `fmt`.

Two gaps this plan found, which get kit lanes here:

1. **The export ABI is too narrow for any real crate** (lane 1, #898). Today
   `vesper build --lib` refuses generic functions, `inout` parameters, strings,
   sequences, boxes, closures, any effect other than `Fail`, and every
   non-`copy` type (recipe, "What is exportable today"). Every P1 crate passes
   `Vec<T>`, `&[u8]` or `&str`, or keeps its state in a struct with private
   collection fields that the tests mutate through `&mut self` methods. None of
   them can be forwarded until the ABI carries sequences, slices, strings and
   opaque owned handles.
2. **Ten of the crates cannot use the harness as built** (lane 2, #899).
   `vesper-port-harness` links the `vesper` library, and the root `vesper`
   package depends, directly or transitively, on `vesper-alloc`,
   `vesper-beacon`, `vesper-crucible`, `vesper-grammar`, `vesper-loom-calculus`,
   `vesper-loom-solver`, `vesper-semver`, `vesper-serde`, `vesper-simpulse` and
   `vesper-tensor`. Turning on `vesper-port` in any of them fails with
   `error: cyclic package dependency` (checked by adding the feature to
   `vesper-beacon` at d523661). Bazel is not affected, because
   `vesper_port_object` runs the prebuilt `//:bin/vesper`.

## Lanes

"Needs" uses the feature names of the survey. A lane starts when every entry in
its **Starts** column has merged. The lane may write Vesper code against an
unmerged dependency's branch to get ahead, but it never merges first.

| Lane | Prompt | Claim | Crate (src LOC, tests) | Needs | Starts after | Compiler links it | Exclusive boundary |
|---|---|---|---|---|---|---|---|
| 1 | [Export ABI for ports](01-export-abi-ports.md) | #898 | kit | F1, F2 | now (strings part after #896) | - | `src/native_export.rs`, `src/native_export/**`, `src/lib_commands.rs`, `crates/vesper-port-abi/**`, `crates/vesper-port-pilot/**`, `tests/native_export*.rs`, `tests/fixtures/native_export/**`, `docs/maintenance/porting-a-crate.md` (ABI sections) |
| 2 | [Cycle-free port builds](02-cycle-free-port-builds.md) | #899 | kit | F8 | now | - | `crates/vesper-port-harness/**`, NEW `crates/vesper-port-build/**`, `tools/bazel/vesper_port.bzl`, `scripts/test-port-harness.sh`, the port-harness steps in `.github/workflows/`, `docs/maintenance/porting-a-crate.md` (build sections), a narrow `--lib <package dir>` hunk in `src/lib_commands.rs`, root patch for seed packages |
| 3 | [vesper-warp-wheel (pilot)](03-warp-wheel.md) | #900 | 1,555, 17 | F1, F3, F7a | lane 1 | no | `crates/vesper-warp-wheel/**` |
| 4 | [vesper-beacon (pilot)](04-beacon.md) | #901 | 919, 12 | F1, F3, F2 (text forms only) | lanes 1, 2; #896 for traceparent text | yes | `crates/vesper-beacon/**` (not `vesper-beacon-arrow`) |
| 5 | [vesper-simpulse](05-simpulse.md) | #902 | 712, 18 | F1, F5, F2 | lanes 1, 2, #896, #897 | yes (via sim-session, doc, chrono) | `crates/vesper-simpulse/**` |
| 6 | [vesper-alloc (Strata collections)](06-strata-alloc.md) | #903 | 8,400, 223 | F1, F5, F7a | lanes 1, 2, #897 | yes | `crates/vesper-alloc/**` |
| 7 | [vesper-quarry planner and catalog](07-quarry-planner.md) | #904 | 12,436 (planner half ~1.7k), 92 | F1, F2, F5, F3 | lane 1, #896, #897 | no | `crates/vesper-quarry/src/{parse,plan,execute}.rs` and their replacements, `crates/vesper-quarry/vesper/**`, `crates/vesper-quarry/port/**`, `crates/vesper-quarry/{Cargo.toml,build.rs,BUILD.bazel}` |
| 8 | [vesper-tensor formats, BPE, sampling](08-tensor-formats.md) | #905 | 7,317 (ported part ~4.3k), 89 | F1, F2, F3, F4, F5 | lanes 1, 2, #896, #897 | yes | `crates/vesper-tensor/**` except `src/{simd,mmap,host}.rs` |
| 9 | [vesper-grammar](09-grammar.md) | #906 | 4,976, 125 | F1, F2, F3 | lanes 1, 2, #896 | yes | `crates/vesper-grammar/**` |
| 10 | [vesper-warp-bbr](10-warp-bbr.md) | #907 | 3,050, 52 | F1, F3, F5 | lane 1, #897 | no | `crates/vesper-warp-bbr/**` |
| 11 | [vesper-warp-tcp](11-warp-tcp.md) | #908 | 3,926, 63 | F1, F3 | lane 1 | no | `crates/vesper-warp-tcp/**` |
| 12 | [vesper-ignis render-graph planner](12-ignis.md) | #909 | 5,084, 69 | F1, F2, F5 | lane 1, #896, #897 | no | `crates/vesper-ignis/**` (not `vesper-ignis-vulkan`) |
| 13 | [vesper-aura](13-aura.md) | #910 | 2,491, 28 | F1, F5 | lane 1, #897 | no | `crates/vesper-aura/**` (not `vesper-aura-gtk`) |
| 14 | [vesper-prism](14-prism.md) | #911 | 2,478, 55 | F1, F2, F5 | lane 1, #896, #897, and #609 closing | no | `crates/vesper-prism/**` (not `vesper-prism-h3`) |
| 15 | [vesper-semver](15-semver.md) | #912 | 2,157, 48 | F2, F5 | lanes 1, 2, #896, #897 | yes | `crates/vesper-semver/**` |
| 16 | [vesper-loom-calculus](16-loom-calculus.md) | #913 | 1,032, 29 | F2, F5 | lanes 1, 2, #896, #897 | yes | `crates/vesper-loom-calculus/**` |
| 17 | [vesper-loom-solver](17-loom-solver.md) | #914 | 1,285, 36 | F2, F5 | lane 16 | yes | `crates/vesper-loom-solver/**` |
| 18 | [vesper-crucible](18-crucible.md) | #915 | 4,120, 59 | F1, F2, F5 | lanes 5, 6, and #596 closing | yes | `crates/vesper-crucible/**` |
| 19 | [vesper-serde scalar codecs](19-serde-scalar.md) | #916 | 7,153 (scalar part ~5.2k), 142 | F1, F2, F3, F4 | lanes 1, 2, #896, and #608 closing | yes | `crates/vesper-serde/**` except `src/simd/**` |

**Depends on strings (#896):** lanes 4 (traceparent text API only), 5, 7, 8, 9,
12, 14, 15, 16, 17, 18, 19, and lane 1's string step.

**Depends on `@vesper-std` (#897):** lanes 5, 6, 7, 8, 10, 12, 13, 14, 15, 16,
17, 18.

**Depends on neither:** lanes 2, 3 and 11, and lane 1 apart from its string
step. These three are the first to start after approval: the kit lanes now, the
two pilots as soon as lane 1 merges.

**Waits on another claim:** lane 14 on wave-nine lane 32 (#609, holds
`crates/vesper-prism/**` and `crates/vesper-warp/**`), lane 18 on wave-nine
lane 19 (#596, holds `crates/vesper-crucible/**`), lane 19 on wave-nine lane 31
(#608, holds `crates/vesper-serde/**`). None of the three has an open PR. Ask
the owner whether to close or narrow those claims before starting the lane.

**Crate order inside the wave:** lane 17 after lane 16 (`vesper-loom-solver`
imports `vesper-loom-calculus`). Lane 18 after lanes 5 and 6 (`vesper-crucible`
imports `vesper-simpulse` and `vesper-alloc`). A ported Vesper package imports
another port through `use @vesper/<name>::...` and a Loom path dependency.

### Not in this wave

- The survey's port-later and toolchain crates (waves P3 and P4).
- `dyn Trait` (F7b), linkable Pulse (F10, #585), source `VVec` (F11, #587),
  shared-memory concurrency (F12). Ports keep the parts that need them in Rust,
  as listed per lane.
- Richer Bridge imports (F9) stay with #611. Lane 1 lets exported functions
  call `.vsys` imports, which is all P1 needs (Tensor's memory map, Strata's
  page reservations).

## Rules for every lane

These apply on top of the project rules and the
[porting recipe](../../maintenance/porting-a-crate.md).

- **Claim first.** Search open issues and PRs for the crate. The lane's claim
  issue already exists; link it from the PR ("Closes #NNN"). Branch from
  `origin/main` and open the PR against `main`.
- **Freeze.** From the day the lane starts, the crate's Rust code takes bug
  fixes only. Add one line to the crate README (create a minimal one if missing)
  that records the freeze and links the claim.
- **Re-measure first.** The survey's counts are grep counts. Before writing
  code, list the crate's public API (types, functions, methods, traits), every
  `#[cfg(test)]` unit test, and the parts that must stay Rust. Put the list at
  the top of the lane report.
- **Where the port lives.** Vesper source goes in `crates/<crate>/vesper/` as a
  Loom package (`Loom.toml` plus `src/**/*.ves`), named `@vesper/<short-name>`.
  It may import `@vesper-std` and earlier ported packages as path dependencies.
  Run `vesper fmt` on every file. The generated binding goes in
  `crates/<crate>/port/binding.rs`, outside `src/`.
- **Self-hosted parser.** `tests/self_host_front_end.rs` parses every `.ves`
  file in the repository with the Stage 1 parser. It cannot parse `use` (#880),
  slice intervals (#887) or inline bounds (`T: Ord`; write `where` clauses).
  Add the port's `vesper/` directory to the corpus skip predicate in
  `fixtures()` in `tests/fixtures/self_host/driver.rs` (which
  `tests/self_host_front_end.rs` and `tests/self_host_check.rs` call), the same
  way `tests/fixtures/packages` is skipped, with a comment naming #880, rather
  than avoiding the syntax. Do not edit `packages/vesper-compiler/**` or
  `bootstrap/**`.
- **Keep the API, change the inside.** The Rust crate's public types stay in the
  crate root, so both builds share them and its dependents do not change. A
  public function or method forwards to the binding under `vesper-port`. Use
  the ABI patterns from lane 1 (owned handles for stateful types, token slabs
  for Rust generic payloads, returned sequences instead of sink closures,
  recorded command plans instead of calls into a Rust trait object). Where a
  public signature cannot be forwarded even so, keep a thin Rust adapter that
  calls the port, and list it in the report.
- **What stays in Rust.** Only the parts named in the lane prompt, or parts you
  show need a feature this wave does not have. Each one gets a line in the
  report: file, reason, and the feature or lane that unblocks it.
- **Unit tests.** Every in-crate `#[cfg(test)]` test is translated, ported or
  deleted with a reason, per the recipe's unit-test rule, before deletion.
- **Divergence check.** Add one deliberately divergent build per ported module
  (`Port::edit` in Cargo, `edits` in Bazel) and assert that the tests catch it.
- **Performance.** Crates on a hot path (Strata, Quarry, Tensor, Grammar,
  Serde, Warp TCP and BBR) add an ABBA benchmark with `vesper-bench` comparing
  both builds before deletion, and report the numbers with the setup. A port
  slower than 2x the Rust build on a benchmark the crate already has does not
  delete the Rust code: report it and stop before deletion.
- **CI.** Linux through Bazel (`vesper_port_object` + `rust_test`, as in
  `crates/vesper-port-pilot/BUILD.bazel`); macOS and Windows through
  `scripts/test-port-harness.sh`. Run `tools/bazel/sync_build_files.py` if the
  crate's BUILD file changes shape.
- **Before pushing:** `cargo +1.100.0 clippy --all-targets -p <crate> -- -D warnings`,
  `cargo test -p <crate>` both with and without `--features vesper-port`, and
  `vesper fmt --check` on the new `.ves` files. Long runs go to GitHub CI from
  the pushed branch; local cloud runs cover only the crate's own tests. The
  cloud disk fills up if you build all integration tests of the root package.
- **Merging.** Merge when the four required checks (`Test all targets`,
  `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`,
  `Windows (x86_64 MSVC)`) are green and review comments are addressed. Lanes
  that touch the root `Cargo.toml`/`Cargo.lock` merge one at a time with fresh
  `main` merged in. If `main` is red and your PR fixes it, cancel the other
  in-progress Actions runs and merge the fix first.
- **Report.** `docs/parallel-results/port-p1-<lane slug>.md` (the path in the lane prompt): API inventory, what
  stays Rust and why, unit-test treatment, divergence checks, benchmarks,
  platforms run, and whether the Rust code was deleted.

## Deleting the Rust code

For the seven crates the compiler does not link (lanes 3, 7, 10, 11, 12, 13,
14), the lane deletes the ported Rust code once both builds pass on all four
platforms, makes `vesper-port` the only build (the feature goes away and
`build.rs` always builds the port), and keeps every test.

For the ten crates the compiler links (lanes 4, 5, 6, 8, 9, 15, 16, 17, 18,
19), deleting the Rust code would make the compiler need itself to build. The
default in this plan is: **the lane proves parity, keeps both builds, and makes
the Vesper build the one every dependent outside the compiler's dependency
closure uses**. The Rust code then stays only as the compiler's seed copy,
frozen, until self-hosting Stage 2 moves the compiler's own use of the crate.
A Cargo feature cannot do this, because resolver 2 unifies features per package
across the build: one crate in the closure enabling the port would give the
compiler the port too. So the frozen Rust becomes a separate seed package
(`<crate>-seed`, same library name) that every crate in
`cargo tree -p vesper -e normal,build` depends on by rename, and the original
package becomes the port build. Lane 2 builds and guards this in Cargo and
Bazel. Today most dependents of these crates are themselves inside the closure
(for example the Loom crates and Kinetic), so for several lanes the port build
is exercised only by the crate's own tests until Stage 2. The owner can choose otherwise in the plan review.

## Merge order and collisions

| Shared file | Lanes | Rule |
|---|---|---|
| Root `Cargo.toml`/`Cargo.lock` | 2 (new crate), any lane adding a dependency | Serialize. Lane 2 merges before any compiler-linked port. |
| `src/native_export.rs`, `crates/vesper-port-abi/**` | 1 only | Ports needing an ABI change ask lane 1 (or open a follow-up after it merges) instead of editing them. |
| `crates/vesper-port-harness/**`, `scripts/test-port-harness.sh` | 2 owns; ports add one line each to the script | Append only; merge `main` before merging. |
| `fixtures()` skip predicate in `tests/fixtures/self_host/driver.rs` | every port | One line per port; merge `main` before merging. |
| Dependents' `Cargo.toml` | compiler-linked ports | Seed package inside the compiler closure, port package outside it, in its own commit, as lane 2 documents. |
| `packages/vesper-std/**` | #897 only | Ports needing a std function request it on #897 or put a private helper in their own package. |

Suggested order: lanes 1 and 2 first; then the pilot 3 and lane 11; then 4 and 10;
then the rest as their dependencies land, with 16 before 17 and 5 and 6 before
18.
