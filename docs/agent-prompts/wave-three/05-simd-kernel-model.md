# Kernel-checked SIMD scanner algorithm and bounds model

Suggested branch: `wave3/simd-kernel-model`.

~~~text
You are implementing one substantial milestone in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline 02b3a1d175efb5d2561b7fa5b0aaf80121ae1bc6 or a descendant; record the actual SHA. Read AGENTS.md if present, docs/specification.txt, docs/implementation.md, docs/parallel-results/wave-two-integration.md, docs/rfcs/parallel-contracts.md and docs/rfcs/wave-three-contracts.md before editing.

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; explicitly reject unsupported cases.
- Edit only the exclusive paths below. Root Cargo.toml/Cargo.lock, src/main.rs/src/lib.rs, CI and global docs belong to the coordinator. Supply a precise registration/dependency patch separately. Temporary UNCOMMITTED root dependency/module edits for local testing are allowed; do not include them in your commits.
- Existing crates stay registered. Only a NEW unregistered crate may temporarily declare its own [workspace]/Cargo.lock; integration removes those. Preserve the separate sandbox guest workspace and current validated dependency versions.
- Use an isolated target directory per checkout/worktree. Do NOT share CARGO_TARGET_DIR between source-divergent compiler worktrees. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Integer arithmetic is vesper.arith.checked/1; possible +, -, *, negation and MIN/-1 overflow uses typed Fail[ArithOverflow]. Explicit wrapping_* calls are total. RuntimeError remains separate. Interface semantics version is 2. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics and native ABI v2 are frozen for this wave. Lower new surface constructs to existing core nodes before backend consumption. Required schema changes go to the coordinator; do not silently add variants or patch another lane.
- src/lean.rs, src/lean_ir.rs, src/lean_nodes.rs and crates/vesper-lean-syntax/** are coordinator-owned. Lean emission now builds a full AST and renders once; never reintroduce raw Lean fragments. crates/vesper-loom-tuf/** is also coordinator-owned.
- Preserve owners on failed consuming operations. Unsupported, ambiguous, invalid evidence, overflow and exhausted budgets are errors, never success. Do not weaken safety checks to fit an example.
- Content hashes are not provenance, public residual effect rows are not authority audits, replay is not DPOR, and executable Lean is not a preservation/termination theorem. Report actual hardware/kernel/toolchain evidence and unavailable environments accurately.
- Read other lanes' APIs without editing them. Keep new tests/examples/report files in your boundary. Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run formatting, focused tests and strict all-target Clippy. For changed execution paths, use real execution; compilation/object generation alone is a narrower result. Record exact commands/results and limitations.
- Commit and push YOUR branch only; do not merge main. Return base/commit/PR, public API, checks, coordinator patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Kernel-checked SIMD scanner algorithm and bounds model.

Exclusive ownership:
- NEW lean/SimdScanner.lean and optional supporting NEW lean/SimdScanner/** files.
- NEW crates/vesper-simd/model/** and NEW crates/vesper-simd/tests/model.rs.
- docs/parallel-results/simd-kernel-model.md.
Scanner codegen/src/lib.rs, existing scanner tests/examples, compiler Lean emitter, proof-audit helper and CI are read-only. Request module/dependency wiring separately; prefer std-only Rust fixtures and the existing pinned Lean 4.24 toolchain.

Baseline: real Baseline128 x86_64/AArch64 count-delimiter/XOR scanner, 16-byte blocks, checked scalar tail, explicit CPU feature evidence and exact code metadata. Native/guard-page tests pass; the AArch64 harness executes 9600 scalar-reference cases under QEMU. There are no current algorithm proofs or verified Cranelift lowering claims.

Implement:
1. An explicit Lean model of the scalar reference and the actual fixed-width block/mask-count/XOR reduction plus scalar-remainder algorithm. Match the code's unsigned byte equality, per-lane mask conversion and XOR reduction, not a different convenient algorithm.
2. Kernel-checked equivalence for arbitrary byte sequences/delimiter and legal block decomposition, including count bounds, XOR composition, block/tail invariants, zero length and all remainder sizes. Establish the representability assumptions for u64 length/count and loop increments rather than hiding machine overflow under Nat.
3. Prove each full-width read range is within the logical slice and each tail read is legal; state valid-memory/ABI assumptions explicitly. Unaligned reads are permitted; no vector overread justified by padding.
4. Use ordinary kernel-checked BitVec lemmas, induction/algebra and decide where appropriate. No sorry, new axioms, admissions, native_decide or bv_decide compiler-trust shortcut: Lean 4.24's bv_decide introduces Lean.ofReduceBool and does not meet this lane's kernel-only boundary. Compile with the actual pinned Lean and report transitive axioms. If proof-audit is used, audit real artifacts with the exact justified standard-axiom policy; do not weaken it.
5. Rust model fixtures compare the same model/reference to REAL JitScanner outputs, including unaligned slices, every tail, all delimiter values and seeded larger inputs. Keep proof model/specification identifiers and metadata explicit so they cannot be mistaken for a machine-code certificate.

Acceptance:
- lean lean/SimdScanner.lean succeeds; theorem statements cover both count and XOR for the whole block+tail loop, plus concrete bounds lemmas.
- Actual axiom reports contain no sorryAx or unapproved axiom.
- Real JIT/model differential tests and existing scanner tests remain green.
- Report precisely that this proves the algorithm/model, not Cranelift/ISA lowering, object linking, CPU discovery, source Isa typing, scalable vectors or performance.

~~~
