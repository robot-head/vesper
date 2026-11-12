# Vesper Crucible: ChoiceTape fuzzing and obligation-safe shrinking

Suggested branch: `wave5/crucible`. Claim issue: #125.

~~~text
You are implementing one substantial milestone of the Vesper design (§29 (Vesper Crucible), excluding §29.3 migrate) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline af03122ea74d403f203170926945f9d35786b289 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/rfcs/wave-five-contracts.md, docs/agent-prompts/wave-five/README.md and the whole of §29 (Vesper Crucible), excluding §29.3 migrate in docs/specification.txt (the snapshot matches the Google Doc revision of 2026-11-12 09:18 UTC) before editing.

Claim: this work is claimed by GitHub issue #125. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #125 from your PR ("Closes #125").

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; reject unsupported cases explicitly with typed errors.
- Edit only the exclusive paths below. Other lanes run concurrently; never edit their paths and never depend on an unmerged lane's crate. The wave-five contracts list the only allowed internal dependencies. Do not depend on the root `vesper` package.
- Other agents own §16.4 source macros/Forge (#103-#110) and §39 formatting (#111-#120). Do not edit crates/vesper-forge*, crates/vesper-fmt, crates/vesper-lint, crates/vesper-loom-sandbox or root macro/formatter code.
- Root patch rule (user-approved 2026-11-12): you MAY commit your own minimal root patch onto YOUR PR branch so CI goes green and the PR merges in one piece: register your new crate in root Cargo.toml `members` AND `default-members` (insert before "experiments/native-pulse"), add any new `[workspace.dependencies]` entries you need, and regenerate Cargo.lock. Nothing else at the root (no src/** CLI wiring, no CI workflow edits, no global docs) unless your assignment says so. Keep the patch in its own commit. If another lane merged first, rebase and regenerate the lockfile rather than hand-merging it.
- Prefer mature, widely used crates for cryptography, parsing and kernel/hardware access; pin exact compatible versions; never vendor or hand-roll cryptographic primitives. Reuse versions already in Cargo.lock where possible (Cranelift 0.125, Wasmtime 50, rustls 0.23).
- Use an isolated target directory per checkout/worktree. Do NOT share CARGO_TARGET_DIR between source-divergent worktrees. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics (vesper.arith.checked/1), interface semantics v2 and native/fiber ABI v2 are frozen. src/**, crates/vesper-lean-syntax/** and crates/vesper-loom-tuf/** are integration-owned. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Rust crates in this wave forbid unsafe code except the gated module the contracts allow for your lane. Linear obligations are modelled as non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner must be detected (debug panic or counted leak reported by an auditor), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes. All arithmetic on untrusted values is checked or explicitly wrapping with a documented reason.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised; a software model validates the modelled protocol only. Performance numbers in the design are proposals; report only what you measured, with the setup.
- Apply the corrections in docs/rfcs/expansion-37-review.md that concern your section.
- Lean: if your assignment owns a Lean file, it must compile with the pinned Lean 4 toolchain (`lean <file>`), contain no `sorry`/`admit`/new axioms, and be checked from your crate's tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). Do not edit the CI workflow; the workspace test job already sets VESPER_LEAN=lean. State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #125. The user has authorized merging this effort's PRs once all required CI checks (`check`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Vesper Crucible (§29 except §29.3, which is lane 11): unified choice tapes, coverage-guided concolic fuzzing and obligation-safe shrinking.

Exclusive ownership:
- NEW crates/vesper-crucible/**, lean/CrucibleShrink.lean (plus lean/CrucibleShrink/ if needed).
- docs/parallel-results/crucible.md.
- Root registration patch for vesper-crucible.
May depend on vesper-simpulse (read-only).

Implement:
1. `ChoiceTape`: a byte/choice stream from which generators draw structured values (integers in ranges, booleans, collections, enum choices) and SimPulse scheduling decisions, with deterministic replay from a saved tape.
2. Shortlex tape reducer (§29.5): delete/zero/minimise spans, re-running the property and keeping only reductions that still fail and whose regenerated values still satisfy the generator's validity (so linear protocol state machines like a WriteTxn begin/commit sequence stay valid without type-specific shrinkers). Prove termination in Lean; do not claim global minimality.
3. Coverage-guided fuzz loop: edge coverage and comparison-operand feedback reported by the target through a small `Probe` API (later wired to Cranelift instrumentation by integration), corpus management, and a solver step for magic-number, checksum-equality and length-prefix comparisons (use a mature SMT crate such as `z3` only if it builds in CI on all three platforms; otherwise implement bounded comparison-operand replacement and report the limitation).
4. Concolic SimPulse fuzzing: a property over a SimPulse program explored by schedule choices drawn from the tape, with failures shrunk to a minimal schedule and replayed.
5. `refute(predicate)` API: search for a counterexample to a predicate over generated inputs and return a minimal deterministic reproduction (the `vesper verify --refute` CLI is integration B's job).
6. lean/CrucibleShrink.lean: the shortlex order is well-founded and every accepted reduction step decreases it, so shrinking terminates.

Acceptance:
- Finds a planted magic-number bug and a planted length-prefix bug that pure random search misses within the same budget (record both); finds and shrinks a planted SimPulse interleaving bug to a short schedule; shrinking preserves linear-protocol validity in a WriteTxn-style test; deterministic replay.
- No claim of nightly-free Cranelift instrumentation of Vesper source or MPK shadowing (integration and lane 11).
~~~
