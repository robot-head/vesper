# Checked arithmetic across all compiler backends

Suggested branch: next/checked-arithmetic.

~~~text
You are implementing one bounded milestone in https://github.com/robot-head/vesper.

Start from a fresh fetch of origin/main in a NEW cloud checkout or dedicated git worktree. Minimum baseline: 0e6caf9 (all first-round handoffs integrated). Record the actual base SHA. Do not share a checkout or branch with another agent. Read docs/specification.txt, docs/implementation.md, docs/parallel-work.md, docs/parallel-results/handoff-integration.md, and docs/rfcs/parallel-contracts.md before editing.

Delivery rules:
- Implement working behavior, with meaningful positive and negative tests. Deliver code, not only an RFC, scaffolding or an adapter proposal.
- Edit only your owned paths. Root Cargo.toml/Cargo.lock, src/main.rs, src/lib.rs, CI and global documentation belong to the coordinator unless explicitly assigned below. Read other lanes' code, but do not change it. Report any necessary cross-boundary patch separately.
- Existing workspace members stay registered: do not restore nested workspaces or lockfiles. A NEW crate may temporarily use its own [workspace] and Cargo.lock for independent testing; the coordinator will register it and regenerate the root lockfile. Match current validated dependency versions. Do not edit the existing sandbox guest workspace/lockfile.
- Do not downgrade dependencies to match illustrative design snippets. Keep public module paths and existing supported behavior, except the explicitly assigned arithmetic migration.
- Keep ownership and failures explicit. Unsupported, ambiguous, overflow, invalid evidence and exhausted budgets must never become success. Preserve recoverable owners on error where the API takes ownership.
- A byte hash is not provenance; a residual effect row is not an authority audit; executable Lean extraction is not a preservation/termination proof; seeded replay is not DPOR. Do not claim kernel/hardware validation from mocks or object emission.
- Preserve the user-approved field restriction k_i ⊑ m_i. Do not modify the Google Doc. Record decisions and remaining gaps in your assigned report.
- Run formatting, relevant tests and strict all-target Clippy. Use real Lean when extraction changes; report unavailable toolchains, kernel permissions or platforms explicitly. For source fixtures outside the arithmetic lane, avoid depending on implicit wrapping arithmetic: that lane is migrating +, - and * to checked effects.
- Commit and push YOUR branch only. Do not merge into main, publish packages or message third parties. Return the base SHA, commit SHA/PR, public API, exact checks/results, integration steps and remaining limitations. If blocked at an ownership boundary, finish independent work and provide a precise coordinator patch/request.
- Other agents are running concurrently. Do not broaden your scope or start overlapping subagents.

Assignment: migrate source integer arithmetic to the normative checked semantics in design §11.2, in one coherent change.

Exclusive ownership:
- crates/vesper-core/src/numeric.rs
- src/check.rs, src/runtime.rs, src/ir.rs, src/ir_check.rs, src/ir_opt.rs, src/optimize.rs, src/native.rs, src/lean.rs, src/lean_ir.rs, src/interface.rs
- Existing root tests/** and examples/**, plus new root arithmetic regression files
- Fixture-only edits to existing #[cfg(test)] source programs in other root src/** files (for example src/io_session.rs); their production APIs/logic remain coordinator-owned
- lean/CheckedArithmetic.lean and docs/parallel-results/checked-arithmetic.md
Do not edit the AST, lexer/parser, package CLI, root manifests or other crates. Ordinary call syntax is sufficient for wrapping intrinsics. Supply any dependency-registration change to the coordinator. Fixture-only permission above does not authorize production changes to src/io_session.rs, src/database.rs, CLI routing or other unassigned modules.

Baseline: u8/u32/u64/i64 arithmetic currently wraps. Potential integer division and dynamic indexing use World.partial_ops and Fail[RuntimeError]. The evaluator, optimizer, native and Lean paths already share scalar kind helpers. src/interface.rs exports builtin contracts under std/core. Read the existing numeric/partial-operation and handler tests before changing behavior.

Implement:
1. A single documented, versioned semantics contract. Ordinary integer +, -, *, their compound assignments and value-level signed negation check overflow and raise Fail[ArithOverflow] unless proved safe. Supply a reserved copy error type ArithOverflow with a documented constructor and stable builtin identity. Do not collapse this with Fail[RuntimeError] or erase typed effect arguments.
2. Explicit same-integer-kind wrapping_add, wrapping_sub and wrapping_mul calls, available in all backends as pure total operations. Reject invalid types, arities and conflicting declarations. Preserve contextual literal typing and the signed-minimum literal spelling.
3. Define the signed division corner coherently: i64::MIN / -1 raises ArithOverflow; i64::MIN % -1 is zero. Division by zero and indexing retain RuntimeError. Floats retain their existing IEEE behavior. Document these boundaries and ensure Lean agrees.
4. Extend checker facts only as far as justified. Constant/range proofs may discharge failures; assignment, inout calls and alias-changing resumptions must invalidate stale facts. A failed compound assignment leaves its target unchanged. Unhandled possible overflow requires the proper effect.
5. Carry potentially failing arithmetic through IR frame checks and state tokens. Optimizations may not discard/reorder observable failures, wrap an overflowing constant, or use reassociation/cancellation rules invalid under checked arithmetic. Keep wrapping-only identities scoped to wrapping operations.
6. Native JIT/objects and executable Lean extraction must implement the same failure/handler/continuation behavior as the evaluator, including non-tail handlers and mutable places. No evaluator fallback.
7. Migrate existing tests/examples deliberately: use explicit wrapping where that was their intent, or declare/handle checked failure. Do not weaken ownership checks or disable old tests just to restore green builds.

Acceptance:
- Boundaries for all four integer kinds: successful endpoints, over/underflow, compound assignment, negation, contextual literals, unsigned high values and signed division corners.
- Correct selection between typed Fail instances, labeled/nested handlers, aborts with linear temporaries/state, and post-resumption fact invalidation.
- Evaluator/JIT/optimized/unoptimized agreement; real Lean compile/#eval for success and handled failure; AArch64 objects and scripts/test-aarch64.sh when tools are available.
- Root interface/SemVer and CLI tests reflect the new builtin/effect contracts. Run the full workspace suite with VESPER_LEAN pointing to the real toolchain, formatting and strict Clippy. Report missing checks honestly.

This is the only compiler-writing lane in this parallel batch. Do not begin trait syntax, region syntax or source-native suspension lowering. Your report must identify any arithmetic behavior still unsupported and the source compatibility impact.
~~~

