# Vesper Chroma: Tier 0/Tier 1 semantic highlighting, OKLCH palettes with contrast audits, `vesper highlight --html` and a wasm32 build

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../../manuscript/README.md) is the sole specification; use the
> [section map](../../section-map.md) for earlier numbering. Confirm live ownership before acting.

Suggested branch: `wave9/chroma-highlight-core`. Claim issue: #578.

~~~text
Specification authority: docs/manuscript/content.typ is the sole specification.
Earlier section numbers below use the legacy numbering; locate their manuscript
chapters with docs/section-map.md before implementing requirements.

You are implementing one substantial milestone of the Vesper design (§43, §21, §40) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline 37f7b8bcfa4cd409177856c8fe5deeaa7f2a88ff or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/README.md, docs/bazel.md, docs/rfcs/wave-nine-contracts.md, docs/agent-prompts/wave-nine/README.md, crates/vesper-syntax (rowan CST, read-only), src/check.rs and src/interface.rs (read-only), src/lsp/** semantic-token code (read-only), website/** (read-only; the owner maintains the site), and the whole of §43, §21, §40 in docs/manuscript/content.typ before editing.

When to start: now. This lane adds a workspace member: merge one at a time with lane 8. If a PR this lane waits on is still open, check its state first; if it has not merged, work only on the parts that do not depend on it and merge origin/main once it lands.

Claim: this work is claimed by GitHub issue #578. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #578 from your PR ("Closes #578").

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; reject unsupported cases explicitly with typed errors. If an item turns out to be infeasible, deliver the rest and explain why in the report.
- Edit only the exclusive paths below. Other wave-nine lanes run concurrently; never edit their paths and never depend on an unmerged lane's work. Library crates must not depend on the root `vesper` package.
- Other agents (Codex) hold §16.4 attribute/item macros, the §39 formatter work (#170, #214), the §42 resource model (lean/ResourceModel42*, docs/rfcs/resource-model42-*), rewrite witnesses and optimizer certificates (src/optimize*, tests/rewrite_*, scripts/check-rewrite-witness.py, docs/rfcs/rewrite42-*, lean/RewriteWitness*), the Lean4Lean replay (lean/Lean4Lean*, tests/lean4lean*, issues #402-#560), and these open CI repairs: src/check.rs (#567), lean/ReferenceEval.lean and lean/ReferenceEval/Soundness.lean (#568), tests/checked_arithmetic.rs and tests/fixtures/lockstep/** (#569), buildbuddy.yaml (#571), crates/vesper-fiber-abi/tests/unwind.rs (#572), crates/vesper-pulse/src/pool.rs and crates/vesper-pulse/tests/pool.rs (#573), root BUILD.bazel (#574), src/lsp/lens_tests.rs (#575), and src/native.rs, src/native_batch.rs, src/native_session.rs (#576). Do not edit those paths unless your assignment names a hunk that waits for the holder's PR to merge, and do not add attribute or item macro syntax. Do not write the eBPF linear ringbuf/socket builder API (PR #165, excluded by the owner). Egglog fusion (crates/vesper-fusion, lean/FusionRules.lean, #460/PR #487) is held by wave-eight lane 11.
- Root patch rule (user-approved 2026-11-12): you MAY commit your own minimal root patch onto YOUR PR branch so CI goes green: new `[workspace.dependencies]` entries and the regenerated Cargo.lock, and, for a new crate, registration in root Cargo.toml `members` AND `default-members` (insert before "experiments/native-pulse") plus its BUILD.bazel from `python3 tools/bazel/sync_build_files.py`. Keep it in its own commit. Root Cargo.toml and Cargo.lock conflict often: merge origin/main into your branch right before merging, regenerate the lockfile with cargo rather than hand-merging it, and wait for CI on the merged head. Merges that touch the root manifests are serialized: if another lane's PR touching them is about to merge, let it land first.
- Shared files that several lanes touch through narrow hunks (src/main.rs, src/check.rs, src/native*.rs, crates/vesper-syntax/**, crates/vesper-highlight/**) are listed in docs/rfcs/wave-nine-contracts.md with one owner each. Keep your hunk in those files small and in its own commit, and merge origin/main right before merging.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (run `cargo +1.100.0 clippy` before pushing if your local toolchain is older). Files a test reads from outside its crate directory must be declared in that crate's BUILD.bazel (`extra_compile_data`, `extra_test_data`); Lean-model tests need `extra_test_data = ["//:lean"]` and usually `copy_test_cwd` (see crates/vesper-chrono/BUILD.bazel). Bazel's rustfmt also checks test fixtures. Windows CI uses Cargo. Root BUILD.bazel is held by Codex (#574): if a root test target needs a timeout or data change, put it in its own commit and merge after #574's PR.
- `vesper fmt --check` runs in CI on every tracked .ves file; new .ves fixtures must be canonically formatted unless listed with a reason in scripts/fmt-exclude.txt.
- Prefer mature, widely used crates for cryptography, parsing, fonts, compression and kernel/hardware access; pin exact compatible versions; never vendor or hand-roll cryptographic primitives. Reuse versions already in Cargo.lock where possible (Cranelift 0.125, Wasmtime 50, rustls 0.23, arrow 56).
- Use an isolated target directory per checkout. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics (vesper.arith.checked/1), interface semantics v2 and native/fiber ABI v2 are frozen unless your assignment says otherwise; additions are additive, and every backend rejects IR operations it does not know with a typed error. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Unsafe code is forbidden except where your assignment allows it: only the wasm32 C ABI shim, in one gated module.
- Linear obligations in host libraries are non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner is detected (debug panic or counted leak), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised (the cloud host has no /dev/kvm, no Intel PT, no hardware PMU, no GPU; perf_event_paranoid is 2; io_uring_setup returns EPERM; it does have PKU). The owner chose software stand-ins (Xvfb, Mesa lavapipe, QEMU) where they fit; WinUI, SwiftUI, DirectX 12, Metal, TEE guests, KVM and RDMA stay deferred. A software model or QEMU validates the modelled behaviour only. Report only performance you measured, with the setup. Interop claims name the peer implementation and version.
- Lean: files must compile with the pinned Lean 4.24.0 (elan downloads are blocked in the cloud; use the GitHub release .zip), contain no `sorry`/`admit`/new axioms, and be checked from your tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, edit website/** (the owner maintains the site), publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #578. The user has authorized merging this effort's PRs once all required CI checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main itself is red and your PR fixes it, follow the standing rule: cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Vesper Chroma: Tier 0/Tier 1 semantic highlighting, OKLCH palettes with contrast audits, `vesper highlight --html` and a wasm32 build (§43, §21, §40).

Exclusive ownership:
- NEW crates/vesper-highlight/** (everything except src/shot/**, which lane 2 owns once this merges)
- NEW src/highlight_commands.rs and one routing hunk for `vesper highlight` in src/main.rs (own commit)
- NEW lean/ChromaHighlightSafety.lean
- NEW tests/highlight*.rs and tests/fixtures/highlight/**
- Root registration patch for vesper-highlight (own commit)
- docs/parallel-results/chroma-highlight-core.md
Dependencies: `smol_str` if not already in Cargo.lock (pin exactly). No regex engine, no TextMate grammars, no browser.

Implement:
1. Create `crates/vesper-highlight` (§43.5): `VesperTokenClass` (the 18 classes with the listed `repr(u16)` values), `HighlightSpan`, `ChromaError`, and `verify_lossless_spans`. Tier 0 classifies the lossless `vesper-syntax` CST of arbitrary or incomplete snippets (no regex). Every byte of the input, trivia included, belongs to exactly one span.
2. Tier 1 (§43.1 item 1): when the snippet checks, decorate `Mode::Linear` bindings from minting site to the consuming `sink`/discharge (`is_live_linear`), mutated FieldTree leaves of `inout` projections versus read-only leaves, and `linear effect` capabilities versus abortable `Fail`/`Cancel`. Get these facts from the checker's existing outputs through a narrow, read-only adapter in this crate's root-package glue (src/highlight_commands.rs); the library crate must not depend on the root `vesper` package, so define a small facts DTO the CLI fills.
3. The 16-byte `ChromaTokenSpanWire` record (§43.2) with an encoder/decoder and a layout test (size 16, field offsets, little-endian). `hover_sym_id` indexes a hover table of inferred types and modes.
4. Palettes (§43.2-§43.3, §43.5): `ChromaPalette::vesper_dusk()` and `vesper_dawn()` defined in OKLCH with sRGB fallbacks. Compute OKLCH to sRGB and relative luminance in code (no hand-copied luminance values) and check that the design's listed hex values match within one sRGB step, or report each mismatch. `verify_contrast_gates` enforces WCAG AAA (7:1) on primary tokens and 4.5:1 on comments for both themes; add an APCA Lc check and record the numbers.
5. `vesper highlight [--html|--ansi|--json] [--theme dusk|dawn] [--tier 0|1] FILE`: semantic, accessible HTML styled only by CSS custom properties (`--vc-fg`, `--vc-linear`, `--vc-inout`, `--vc-effect`, ...), with line numbers and pills marked `user-select: none` so copied text is byte-exact; emit the `vesper-chroma.css` sheet (§43.3) with both themes and a size check (target 1.2 KiB). `--json` emits the wire records.
6. Build the crate for `wasm32-unknown-unknown` with a no-WASI, no-allocator-bloat profile and expose a minimal C ABI (`highlight(ptr, len) -> spans`). Measure the stripped, optimized module size against the 26 KiB target in a test and report the number honestly; measure re-highlight time for a 4 KiB edit on the cloud host.
7. Lean (§43.4): `lean/ChromaHighlightSafety.lean` with the two listed theorems, generalized to a list of contiguous spans (concatenation reconstructs the source length) and to the integer contrast gate the Rust code actually uses. Say exactly what is proved about which model.

Acceptance:
- Lossless round-trip on every `.ves` file in the repository (concatenated span bytes equal the source) and on truncated/incomplete prefixes of them; Tier 1 fixtures for linear, inout and effect classes, including a snippet that fails to check (falls back to Tier 0 with a typed reason).
- Contrast audit passes for both themes; a deliberately low-contrast palette is rejected with `InsufficientContrast`.
- HTML output test: stripping tags and the `user-select: none` elements gives the original source byte for byte.
- Tier 0 latency on a 200-line file measured and reported (target under 0.2 ms); no performance claim beyond what was measured.
- Report: docs/parallel-results/chroma-highlight-core.md, with what was built, checks, decisions and remaining gaps.
~~~
