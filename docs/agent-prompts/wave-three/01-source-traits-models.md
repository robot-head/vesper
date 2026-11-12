# Source trait/model syntax and static evidence elaboration

Suggested branch: `wave3/source-traits-models`.

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

Assignment: Source trait/model syntax and static evidence elaboration.

Exclusive ownership:
- crates/vesper-core/src/ast.rs: additive frontend declaration metadata only; existing ExprKind/Type shapes remain unchanged.
- crates/vesper-syntax/**, src/check.rs, src/interface.rs, new src/pact_frontend.rs.
- crates/vesper-pact-bridge/** and an optional NEW root-free Pact binding/lowering crate if needed.
- NEW tests/source_traits.rs, tests/source_models.rs and examples/traits/**.
- docs/parallel-results/source-traits-models.md.
No src/native.rs, runtime/IR/optimizer/Lean changes, existing root test rewrites, or vesper-pact solver changes.

Baseline: vesper-pact already solves canonical/named goals and coherence; vesper-pact-bridge derives checked source types/mode schemes and consumes revision-bound evidence. It currently depends on ROOT vesper. Adding root -> bridge would create a Cargo cycle: resolve this by using Pact directly or extracting a root-free binding/lowering core, keeping CheckedPackage above the compiler. Preserve the existing bridge API where feasible.

Implement:
1. Real source declarations for traits with associated types/method signatures, canonical impls, named models, and lexical model selection consistent with §16. Parse bodies and useful diagnostics/spans, not opaque raw strings. Frontend-only metadata may be additive, but normalize projection/model syntax before ordinary backend core types/calls are produced.
2. Lower declarations into the existing Pact goal/coherence boundary. Stable identities use package-qualified declaration paths, not spans or allocation order. Recheck source mode bounds, associated types, row/effect arguments and model brands after substitution. Keep canonical orphan/overlap rejection; allow otherwise-valid named models without falsely making them canonical.
3. A coherent STATIC dispatch slice must execute. Resolve concrete trait/model method calls to existing checked function calls, including associated-type normalization and lexical model choice/shadowing. Typecheck method bodies/signatures, passing modes and effects; never choose a method by printed name alone. Keep generated implementation bodies private using explicit metadata; do not accidentally export them as public source functions. No interpreter-specific dynamic dispatch shortcut.
4. Keep accepted backend inputs in the current core schema. Unsupported generic dictionary dispatch, unresolved projections or model flows must fail before a World is accepted for execution. Do not encode unresolved trait operations as fake ordinary types/calls. Do not add backend node variants or alter row/ownership semantics.
5. Consume proof/evidence only under the current coherent revision. Source or binding changes invalidate it. Trait/model contracts not representable by current .vhir must export Unsupported metadata, not disappear or receive a false SemVer compatibility certificate. Supply any richer interface-schema proposal to the coordinator separately.
6. Document the exact accepted grammar and supported/static fragment. Keep original method/function and generic ADT syntax working.

Acceptance:
- Source programs declare a trait, associated type, canonical impl and two named models; concrete calls and local model shadowing select the intended bodies.
- Existing evaluator and JIT execute the elaborated ordinary calls with matching results; supported cases also emit AArch64 objects. Lean is read-only and must continue accepting supported core output.
- Negative tests cover overlap/orphans, malformed/bad method signatures, mode/effect mismatch, wrong model brand, ambiguous/no-solution/overflow obligations, escaping unresolved projections and stale evidence.
- Existing bridge mode/coherence/certificate tests and focused compiler ownership/arithmetic tests still pass.

~~~
