# Real cached WASI macro execution through the Forge gate

Suggested branch: `wave3/cached-forge-runner`.

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

Assignment: Real cached WASI macro execution through the Forge gate.

Exclusive ownership:
- NEW crates/vesper-forge-runner/**, including its own tests/examples/fixtures.
- docs/parallel-results/cached-forge-runner.md.
Existing vesper-forge, loom-build, sandbox, CAS, trust, compiler and guest fixture workspace are read-only. If a fixture component is needed, keep its source/build instructions under YOUR new crate and do not rewrite the existing guest.

Baseline: loom-build deliberately accepts BuildScript only. Forge now owns scopes/splices, flat-output provenance, lexical-fragment resolution and a trusted-graph authority gate. Real WASI fixture tests already show that guest scopes cannot become caller provenance. Do not merely enable macro StepKinds in the build runner.

Implement:
1. A real macro runner for ProcMacro/DeriveMacro that executes a component under SandboxHost with snapshotted declared inputs, deterministic settings and explicit budgets, then imports the actual wire result into HygieneHost and resolves/checks authority.
2. Separate execution caching from expansion approval. Store only bounded raw UNTRUSTED wire output in CAS. Never persist host scopes, expansion ids, splice handles or AcceptedExpansion as reusable authorization. Every cache hit rehydrates fresh host provenance, validates claims against the CURRENT registry, resolves and reruns CURRENT emits/ceiling/transitive graph checks.
3. Execution identity/key binds exact component, invocation, inputs, observable sandbox limits/settings and actual host implementation identity (including executable bytes or equally complete verified identity). Approval context binds complete canonical registry/graph/policy/ceiling content, not only caller-supplied version labels. An approval-context change may reuse raw execution but must never reuse old approval.
4. Flat ABI output remains entirely macro-originated, even for echoed tokens. Caller-local antiquotation is unsupported without a real host-authenticated splice protocol; do not invent provenance from guest scopes/spans/pre_resolved ids or add a parallel typed guest ABI in this milestone.
5. Validate malformed/oversized raw output before additional per-token allocation or retention; account for Wasmtime's initial canonical-ABI lifting, which already makes bounded host allocations. Do not claim zero initial allocation. Keep private snapshots stable, verify every cache object and reject/quarantine corrupt records through documented policy. Error diagnostics, failed resolution and authority denial must not publish an accepted result. Preserve no-replace/fresh-destination semantics for outputs.
6. Return a useful explicit result (execution miss/hit, raw output identity, fresh resolution/audit, or rejection). Hashes/audits are conditional local policy evidence, not package confinement or registry provenance.

Acceptance:
- Real component miss/hit gives equivalent fresh expansions and policies; old scope ids never survive sessions.
- Pure flat output succeeds; generated dependency/own-package privileged calls and locally handled effects still require macro emits AND caller ceiling.
- Changed component/input/settings invalidate execution; changed registry, graph or policy content under the SAME version label cannot retain old authorization.
- Tests cover forged caller scopes/spans/claims, malformed/oversized output, stale/corrupt cache entries, failed guest/expansion nonpublication and supported deterministic output.
- No network, compiler source macro syntax, Sigstore or full build-graph claims.

~~~
