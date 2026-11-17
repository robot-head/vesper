# Cycle-free port builds and the seed-copy rule (port wave P1, lane 2)

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification. Confirm
> live ownership before acting.

Claim: #899. Baseline: `d523661` (origin/main). Lane prompt:
`docs/agent-prompts/port-p1/02-cycle-free-port-builds.md` (plan PR #917).
This is a porting-kit lane: nothing is ported for its own sake. The recipe
sections it owns are in [porting-a-crate.md](../maintenance/porting-a-crate.md)
(step 3 "Where the compiler comes from", step 7 CI, "Proving the harness can
fail", and the new "Crates the compiler links").

## The problem, reproduced

At `d523661`, `vesper-port-harness` linked the `vesper` library, and `vesper`
depends on ten port-first crates (`vesper-alloc`, `vesper-beacon`,
`vesper-crucible`, `vesper-grammar`, `vesper-loom-calculus`,
`vesper-loom-solver`, `vesper-semver`, `vesper-serde`, `vesper-simpulse`,
`vesper-tensor`). Giving any of them the harness as a build dependency fails
resolution with `error: cyclic package dependency`
(`vesper -> vesper-beacon -> [build] vesper-port-harness -> vesper`). Bazel had
no cycle (`vesper_port_object` runs `//:bin/vesper`), but its rule took only one
`.ves` file.

## What was built

1. **Harness without the compiler library.** `vesper-port-harness` now depends
   only on `cc` and `toml` (`vesper` stays a dev-dependency of its own tests).
   `Port::build` gets the object from `VESPER_PORT_OBJECT` (prebuilt; Bazel),
   else runs the executable in `VESPER_PORT_COMPILER`, else builds the in-tree
   compiler with a nested `cargo build -p vesper --bin vesper` into
   `<target>/vesper-port-compiler` (measured: 2 min 15 s cold on the 4-core
   cloud host, a no-op afterwards; no lock contention with the outer build).
   It checks the committed binding against the one the compiler writes
   (`--rust-binding`), archives with `cc`, and sets `cfg(vesper_port)`. One
   mode replaced the library mode; the pilot passes unchanged (both builds and
   the divergent build).
2. **Package inputs.** `Port::new(lib, "vesper")` takes a Loom package
   directory. The harness emits `cargo:rerun-if-changed` for every package
   directory in the path-dependency closure, and `Port::edit` on a package
   edits a copy of the whole closure (relative layout kept) in `OUT_DIR`.
   `vesper build --lib <dir>` already accepted a package through
   `packages::load` (P0 lane 1), so `src/lib_commands.rs` needed no change.
   In Bazel, the new `vesper_package` rule (manifest, `src/**/*.ves`, `deps` on
   other packages) feeds `vesper_port_object(package = ...)`, which declares
   the transitive files as inputs, supports `edits` plus `edit_src` on a copied
   tree, and exposes the generated binding in output group `rust_binding`.
   `crates/vesper-port-build` is the test: package `@vesper/port-build`
   (modules `lib`, `shapes`) importing `@vesper/port-build-math` (modules
   `lib`, `scale`) by path, linked through Cargo and Bazel, with a divergent
   build (feature `divergent`, Bazel `:port_build_vesper_divergent`).
3. **Seed-copy rule, on Beacon.** `crates/vesper-beacon/seed` is package
   `vesper-beacon-seed`, library `vesper_beacon`, built from `../src` with no
   build script (so `cfg(vesper_port)` is off) and Beacon's integration tests.
   The root package depends on it by rename (`vesper-beacon = { path =
   "crates/vesper-beacon/seed", package = "vesper-beacon-seed" }`), so no
   compiler code changed. `vesper-beacon` is now the port-backed package: its
   `build.rs` always links `vesper/` (stub port of
   `FastLogHistogram::bucket_index`, two modules) and `vesper-beacon-arrow`,
   outside the closure, links it (`tests/beacon_build.rs` asserts
   `IMPLEMENTATION == "vesper"`). In Bazel, `vesper_crate` for Beacon passes
   the prebuilt object to the build script, and `seed/BUILD.bazel` is written
   by hand.
4. **CI.** macOS and Windows pass the `vesper` binary they already build in
   `VESPER_PORT_COMPILER`; `scripts/test-port-harness.sh` builds it when unset
   and now also runs `vesper-port-build` (plus its divergence check), both
   Beacon packages and the arrow link test.
5. **Guard.** `crates/vesper-port-harness/tests/closure_guard.rs` walks
   `cargo metadata` from `vesper` and `vesper-codegen-verified` over normal
   and build edges and fails on any package with a build dependency on the
   harness. `//crates/vesper-port-harness:compiler_closure_has_no_ports` does
   the same on the Bazel graph with `genquery`
   (`kind('vesper_port_object', deps(...))`). A port that slips in anyway
   fails at build time: the nested compiler build sets
   `VESPER_PORT_IN_COMPILER_BUILD` and a port build script that sees it stops.

## The deliberate violation

`guard_fails_when_the_compiler_links_a_port` rewrites, in the real metadata,
the compiler's dependency on `vesper-beacon-seed` to the port-backed
`vesper-beacon` and asserts that the guard reports `["vesper-beacon"]`. The
positive controls are `violations("vesper-beacon-arrow") == ["vesper-beacon"]`
in Cargo and `ports_in_beacon_arrow == //crates/vesper-beacon:beacon_vesper` in
Bazel.

## Report items the plan asks every lane for

- **API inventory.** Ported: `FastLogHistogram::bucket_index(u64) -> usize`
  only (stub). Everything else in Beacon is lane 4 (#901).
- **What stays Rust.** All of Beacon apart from the stub, by design of this
  lane. The seed copy stays Rust until self-hosting Stage 2.
- **Unit tests.** Beacon's two `#[cfg(test)]` tests (including the exhaustive
  bin-boundary check of `bucket_index`) run in both packages unchanged.
- **Divergence checks.** Pilot (existing) and `vesper-port-build` (new,
  package edit). The Beacon stub has none; lane 4 adds one per module.
- **Benchmarks.** None. The stub calls the port through the P0 export ABI,
  which allocates a failure record per call; `FastLogHistogram::record*` in
  the port-backed build pays that. Only crates outside the compiler closure
  (today only `vesper-beacon-arrow`) see it.
- **Platforms run.** Linux locally (Cargo, stable 1.97 and clippy 1.100);
  Bazel, macOS and Windows through CI on the PR.
- **Rust code deleted.** No.
- **Port build is std.** The P0 binding uses `vec!` and `unsafe`, so the
  port-backed Beacon is `std` with `deny(unsafe_code)` (the binding module is
  allowed); the seed stays `no_std` with `forbid(unsafe_code)`.

## Remaining gaps

- The other nine compiler-linked crates get their seed package in their own
  lane, following the recipe.
- `cargo doc` with both Beacon packages in one invocation (so also
  `cargo doc --workspace`) stops with `document output filename collision`,
  because both document library `vesper_beacon` (rust-lang/cargo#6313).
  Document one at a time (`cargo doc -p vesper-beacon`). No CI job runs
  `cargo doc`.
