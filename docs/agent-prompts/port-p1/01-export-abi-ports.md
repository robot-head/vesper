# Export ABI for ports: sequences, slices, strings, opaque owned handles, generic instances and `.vsys` imports

> Port wave P1, lane 1. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p1/export-abi-ports`. Claim issue: #898.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are building part of the porting kit for port wave P1 of https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline d523661 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md (the recipe), docs/maintenance/vesper-port-survey.md, crates/vesper-port-pilot (the worked example).

When to start: now. The string step waits for P0 lane 7 (#896) to merge; do everything else first and merge origin/main once #896 lands, or ship the string step as a follow-up PR if #896 is late. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #898 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #898").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-p1/README.md: freeze, re-measure first, where the port lives, the self-hosted parser skip list, keep the API, what stays in Rust, unit tests, divergence checks, performance, CI, checks before pushing, merging and the report.
- Edit only the exclusive paths below, plus one line each in the shared lists the plan names (scripts/test-port-harness.sh, the tests/self_host_front_end.rs skip list) and, for a compiler-linked crate, the one-line feature switch in each non-compiler dependent's Cargo.toml (own commit). Other P1 lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Do not edit src/verified_codegen/**, packages/vesper-compiler/**, bootstrap/**, packages/vesper-std/** (P0 lane 8, #897) or the strings module (P0 lane 7, #896). Ask those lanes for what you need.
- Root patch rule: you MAY commit your own minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock; for a new crate, registration in root Cargo.toml `members` and `default-members` plus its BUILD.bazel from `python3 tools/bazel/sync_build_files.py`) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100. Files a test reads from outside its crate must be declared in the crate's BUILD.bazel. Bazel's rustfmt checks every file under src/ and ignores #[rustfmt::skip]: keep generated bindings in port/. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file.
- Unsafe code is forbidden in the port; existing unsafe that stays in a Rust shim is counted in the report. Malformed, overflowed and over-budget inputs are typed errors, never panics on untrusted bytes. "0 Alloc" claims need a counting-allocator test over the warmed-up steady state. Report only performance you measured, with the setup.
- Lean: any Lean file must compile with the pinned Lean 4.24.0 (use the GitHub release .zip), with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #898. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what was ported, what stays Rust, checks, root patch, decisions and remaining gaps.

Assignment: Export ABI for ports: sequences, slices, strings, opaque owned handles, generic instances and `.vsys` imports.

Exclusive ownership:
- src/native_export.rs and NEW src/native_export/**
- src/lib_commands.rs (the `vesper build --lib` flags)
- crates/vesper-port-abi/**
- crates/vesper-port-pilot/** (extend the pilot to exercise every new ABI form)
- tests/native_export*.rs and tests/fixtures/native_export/**
- docs/maintenance/porting-a-crate.md: the sections "The ABI" and "What is exportable today", plus a new section "Patterns for Rust APIs"
- A narrow hook in src/native.rs only if the export path needs one (own commit)
- docs/parallel-results/port-p1-export-abi-ports.md
Dependencies: Merged: #879 (export ABI v1), #882 (ChunkSeq/Slice), #885, #893, #883. Strings: #896.

Implement:
1. Sequences and slices across the boundary: `ChunkSeq[T]` results become `Vec<T>` (the binding copies out and frees the Vesper buffer through an exported free symbol), `in Slice[T]` parameters accept `&[T]`, and `inout Slice[T]`/`inout ChunkSeq[T]` accept `&mut [T]`/`&mut Vec<T>` where the element is a word layout. Bump the interface schema to `vesper.native.export-abi/2` and keep v1 output byte-identical for v1-only modules.
2. Strings (after #896): `in Str` accepts `&str`, `String` results become `String`; the binding checks nothing again that Vesper guarantees (valid UTF-8).
3. Opaque owned handles: an exported non-`copy` struct or enum (affine or with collection fields) crosses as a handle. The binding generates a Rust newtype that owns it, with `Drop` calling an exported `vesper_<lib>_<Type>_drop`. Functions taking `inout T` become `&mut self` methods, `in T` become `&self`, and by-value `T` consumes. Linear types get no `Drop`: their binding is `#[must_use]` and only a consuming call releases them, as the house rules for linear obligations require.
4. Boxes and recursive enums as owned handles, with accessor functions, so trees (Quarry plans, Grammar ASTs) can be returned.
5. Generic instances: an `export` list in the package (or a CLI flag, `--instance f[u64]`) names monomorphic instances to export, with stable mangled symbol names. Generic Rust APIs over an arbitrary payload `T` use the token-slab pattern below instead.
6. `.vsys` imports in exported functions: allow `Ffi[Lib]` rows when the library is a `.vsys` surface whose shim the Rust crate links. Document how the crate provides the shim symbols.
7. Write the section "Patterns for Rust APIs" in the recipe, each with a pilot example: (a) owned handles for stateful types; (b) token slabs: a Rust generic payload `T` stays in a Rust-side slab in the wrapper and Vesper stores a `u64` token; (c) sink closures (`impl FnMut(..)`) become a returned `ChunkSeq` that the wrapper feeds to the closure; (d) a Rust trait the Vesper code must call (Clock, Backend) becomes either a value passed in (a `now` argument) or a recorded command plan the wrapper applies; (e) errors map to the crate's own error enum through `Fail[E]`.

Stays in Rust: Callbacks from Vesper into arbitrary Rust closures and trait objects (F7b/F9, #611); Pulse runtime calls (F10, #585).

Acceptance:
- The pilot exercises every new form and its tests pass with and without `vesper-port` on all four required checks.
- A divergence check per new form (sequence result, slice parameter, handle method, string) fails the pilot tests as expected.
- Every still-refused form has a refusal test naming the function.
- Memory: a test runs 10^5 handle create/drop cycles and sequence returns and checks with a counting allocator on the Rust side that nothing leaks.
- Report: docs/parallel-results/port-p1-export-abi-ports.md, with the API inventory, what was ported, what stays Rust and why, unit-test treatment, divergence checks, benchmarks, platforms run, and whether the Rust code was deleted.
~~~
