# Opaque Bridge handles for .vsys imports (F9)

> Port wave P3b, lane 3. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3b/bridge-handles`. Claim issue: #1065.

A port reaches Rust only through `.vsys` imports, which carry scalars, wire structs and `CSlice` tuples; a `ForeignVal[Ptr[..]]` result has no source carrier (src/ffi_lowering/types.rs). The manuscript already names the construct, `linear extern type` and `affine extern type` (§13), but nothing implements it. The Warp port's handshake and endpoint need it: `vesper-warp-crypto` sealers and openers keep an expanded AEAD key schedule, `vesper-warp-tls` keeps a rustls session, and UMEM frames are owned buffers, so each is an opaque handle the port holds and passes back. This is the import-handle half of wave-nine lane 34 (#611), which has had no PR or report since 2026-11-16; #611 keeps pointer results, narrow integers and the sandbox stack.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are adding a compiler feature that the port wave P3b plan of https://github.com/robot-head/vesper needs for the next crates to port.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 8212ddf or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3b/README.md (the wave plan), "Rules for every lane" in docs/agent-prompts/port-speed/README.md (the compiler-lane rules and the Stage 1 guard), the manuscript's §13 Bridge sections (`linear extern type`, finalizers, `Ffi[Lib]`), src/ffi_lowering.rs and src/ffi_lowering/**, crates/vesper-ffi, the `.vsys` import text of docs/maintenance/porting-a-crate.md, and docs/parallel-results/bridge-native-lowering.md.

When to start: now. First check #611: if it has an open PR, stop and report instead of duplicating it. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency. Stacking a branch on an open dependency's branch is allowed; retarget it to main once the dependency merges.

Claim: GitHub issue #1065 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1065").

Delivery and coordination:
- Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch). The verified backend and WebAssembly refuse the new construct with a typed diagnostic.
- Edit only the exclusive paths below. The port speed wave (#1021-#1028) owns the native backend's performance hunks and the port lanes of this wave own their crates; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port but Aura. Do not change any port's default build or delete Rust code. You may try the feature in a port locally; do not commit port changes.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy --all-targets -- -D warnings` on what you touched before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need `python3 tools/bazel/sync_build_files.py`. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file, and the Stage 1 self-hosted parser must accept or skip every new .ves file (one line in the `fixtures()` skip predicate in tests/fixtures/self_host/driver.rs).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch; local runs cover only targeted tests.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1065. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the manuscript change, the Stage 1 guard evidence, tests, the benchmark, decisions and remaining gaps.

Assignment: Opaque Bridge handles for .vsys imports (F9).

Exclusive ownership:
- the extern-handle and slice-parameter hunks of src/ffi_lowering.rs and src/ffi_lowering/** (types.rs, native.rs)
- the `.vsys` handle declarations in crates/vesper-ffi/**
- the extern-handle hunks of src/check.rs (modes and finalizer obligations)
- tests/ffi_handles*.rs, tests/fixtures/ffi_handles/**
- the §13 text for import handles and slices, the `.vsys` import paragraph of docs/maintenance/porting-a-crate.md, and docs/parallel-results/port-p3b-bridge-handles.md

Dependencies: None. Coordinate with #611 as above.

Implement:
1. Specify what §13 leaves open, in the smallest change: a `linear extern type` / `affine extern type` in a `.vsys` library is one opaque word, an import may return it and take it `in`, `inout` or `sink`, a `sink` import is its finalizer, and a linear handle must reach exactly one finalizer on every path (an affine one may be dropped, which calls its declared drop import).
2. Byte slices as import parameters: `in Slice[u8]` and `inout Slice[u8]` pass `(ptr, len)` directly instead of a tuple, so an AEAD seal or open works in place on a packet buffer.
3. Checker: handle modes follow the linearity rules; a handle is never copied, compared or inspected in source.
4. Native backend and JIT; the evaluator gets a stand-in so `vesper run` tests work. The export ABI may pass a handle through as an opaque word or refuse it with a typed diagnostic; say which.
5. A worked example in tests: a stateful Rust AEAD context (create, seal in place, open in place, destroy) driven from Vesper, with the finalizer counted exactly once on every path including `Fail` exits.
6. Measure a seal through the handle against a stateless call that re-derives the key each time, on 1,200-byte packets.

Stays in Rust: The Rust side of every handle (the shim crate). Pointer results, narrow integers and the sandbox stack stay with #611.

Acceptance:
- Import handles and slice parameters work in the native backend and the JIT on the four required checks; the finalizer runs exactly once on every path (counting test).
- Every misuse (copy, double finalize, leak of a linear handle) is a typed diagnostic with a fixture.
- Stage 1 lock reproduces unchanged.
- Report: docs/parallel-results/port-p3b-bridge-handles.md with the manuscript change, the ABI, the measurement, and what the Warp port needs from it.
~~~
