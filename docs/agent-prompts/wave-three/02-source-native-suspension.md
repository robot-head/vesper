# Checked source to native one-shot suspension

Suggested branch: `wave3/source-native-suspension`.

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

Assignment: Checked source to native one-shot suspension.

Exclusive ownership:
- src/native.rs and NEW src/native_session.rs / native suspension helper modules.
- Optional NEW crates/vesper-native-session/** if a facade above root is preferable; avoid dependency cycles.
- NEW tests/source_native_suspension.rs, tests/native_suspend_abi.rs, examples/native-pulse/**.
- docs/parallel-results/source-native-suspension.md.
Read compiler AST/checker/IR, vesper-fiber-abi, Pulse and the native experiment without changing them. No parser/checker/IR-schema/runtime/Lean writers in this lane. Request coordinator module/dependency wiring separately.

Baseline: hand-written CLIF parks/resumes through vesper-fiber-abi ABI_VERSION=2 on pinned Linux x86_64/AArch64 stacks. NativeRuntime has owned shutdown/drain, driver cancellation, FP isolation and recoverable incomplete shutdown. The ordinary compiler still specializes lexical effects and does not emit source parking. Read docs/rfcs/native-pulse-abi.md and the source-lowering checklist in native-lifecycle.md.

Implement:
1. A public embedding compile/run path for an ACTUALLY checked source entry with declared linear external effects. Use existing FunctionIr/Op::Call capability and effect metadata; validate explicit effect/operation bindings against the checked signatures. No new syntax or IR variants. Keep ordinary jit/build behavior stable.
2. Lower the supported operations to clif::emit_park and typed resume/failure/cancellation branches. At least yield and one owned read operation must work. A first read may use a fixed-size scalar/aggregate buffer rather than pretending a general ByteSlice API exists; state the supported ABI precisely.
3. Preserve live scalar/aggregate locals, source linear obligations and inout updates through multiple parks. Data-frame accesses after a park must derive from ParkResult.data_base, never an old base. Keep physical stacks pinned and do not claim machine-stack relocation.
4. Use NativeRuntime/one-shot evidence owners, not the reference evaluator. Cancellation must execute source-defined cleanup/result handling consistent with the checked signature and may re-park; define abortive cleanup on repeated cancellation. Keep typed arithmetic/runtime failures distinct. Unsupported cancellation signatures fail at compile/binding time, never drop a live source obligation.
5. Original I/O completion, not a cancel acknowledgement, releases DMA/request owners. Submission failure returns owners. Finish through owned shutdown; return IncompleteShutdown and its runtime/owners when a drain budget expires. Local captures only; no work-stealing or inferred Send claim.
6. Preserve ABI v2 layout and public fiber APIs. Platform lane 6 may add macOS direct fibers but pulse/source execution remains Linux in this slice. Put all source-specific binding/layout metadata in backend-private structures. If a frozen shared schema truly blocks progress, finish the independent supported path and send a minimal coordinator request.

Acceptance:
- Source holds and consumes a linear resource exactly once across at least two native parks; aggregate values and inout fields agree with evaluator execution used only as an oracle.
- Typed success/error/cancel paths, cancellation before start/during I/O, cleanup re-park and budgeted incomplete shutdown are exercised with real native machine code.
- A deterministic fixture driver covers cancellation and error ordering; actual io_uring test explicitly reports unavailable kernels. No mock result is presented as kernel evidence.
- Negative tests reject wrong effect bindings, duplicate/stale resume, stale data-base use and unsupported source shapes. AArch64 object generation and QEMU execution of the supported source path are included where available.

~~~
