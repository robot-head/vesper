# Independent resource-oracle acceptance plan (#309)

Historical planning checkpoint for #309. The separately claimed #317 now implements the reference-evaluator provider with its own narrower [frozen API/profile](runtime-resource-trace42-contracts.md) and [actual evidence](runtime-resource-trace42-integration.md). The model/native/generator/campaign proposals below remain future work; they are not implemented behavior or a full-harness source authorization.

This is a read-only feasibility and acceptance plan against main `c226ee98695135ad59e64318213e79f911ad6325`. It owns only this document. No compiler, model, test, dependency or build file was edited; no Cargo build, CLI probe, Lean compilation or replay was executed by this lane. The available compiled CLI is an older `20ee` baseline and is not evidence about this source revision. Proposed APIs and limits below are recommendations for a future published contract, not implemented interfaces.

The immediate blocker is an executed resource-identity observation seam in both real backends. A Lean resource model, generator and trace comparator can be useful foundations, but cannot establish differential resource behavior until actual evaluator and native executions emit their own identity-bearing events. Pending external #307/#308 source belongs to its existing owners and must be reviewed through its actual published API before this blocker is considered resolved.

## Requirement and existing evidence

Specification §42.1 requires a computable multi-local reference checker and fuel evaluator, audited through native Lean and external Lean4Lean, then 100,000+ Crucible ChoiceTape-generated programs with one-to-one static diagnostics and bit-identical dynamic resource mint/consume traces. The approved field premise remains mandatory: for every field, `k_i ⊑ m_i`. An `@@ copy` or `@@ affine` override cannot hide a linear Ticket. Passing a finite test profile does not establish the rest of §42, Rust-to-model refinement, arbitrary source semantics, physical resource conservation, self-hosting or DDC.

The existing arithmetic campaign establishes real arithmetic execution, including the previously recorded 100,000-case run. Its implementation provides a useful isolation pattern but not resource evidence:

| Inspected current implementation | Actual behavior | Acceptance implication |
| --- | --- | --- |
| `src/bin/arithmetic_oracle42/worker.rs::execute_case` | Parses rendered source, independently normalizes its actual AST to the request, checks original source, runs the actual optimizer and rechecks its output | Preserve this source binding and recheck. A DTO accepted by Lean is not evidence that Rust executed that DTO. |
| `execute_mode` | Calls `Runtime::run` and real `native::jit_with_options` for original/optimized AST × IR off/on: four settings per backend, eight outcomes | All eight executions need their own observed traces. A scalar answer from the existing API has no resource ledger. |
| `src/runtime.rs::Value::Data` | Contains type, constructor and fields; the value enum is `Clone`, and slot reads clone representations | No opaque logical resource ID is present. Representation cloning is neither an observed mint nor proof of an ownership copy. |
| `src/native.rs::data` and JIT API | Empty/nonaggregate data can lower to zero; aggregate fields are stored/copied; JIT returns a decoded `Value` | Addresses, zero bits, payloads or source variables cannot identify empty/equal-payload resources. Actual identity propagation needs a defined implementation. |
| `native::object_with_observer` | Observes compiled Cranelift functions | This observes compilation, not executed mint/consume events. |
| `Runtime::begin_external` / `native_session` | Evaluator exposes actual external operations; native session presently supports Yield/no arguments and Read/one inout word buffer with constrained enum results | No existing generic resource mint/consume ABI follows from this. Encoding a custom ledger into Read would be a narrower host-operation experiment, not constructor/destructor evidence. |
| Crucible `Source`, `ChoiceTape`, `fuzz`, `replay`, `shrink` | Real bounded draws, strict replay, coverage-driven corpus and same-failure shrinking exist | A new ownership-aware generator can use them. Existing arithmetic rank/SplitMix generation and scalar CLI feedback do not already discharge the ChoiceTape/resource requirement. |

The current grammar has no resource `mint`/`consume` keywords. Existing language/backend tests demonstrate linear structs, terminal destructuring, tuples, wrapper mode crossing and sink helpers. These are source support examples, not new execution evidence from this lane. The source boundary for tracked operations therefore needs to be frozen explicitly.

## First profile and source binding

Review of the model plan agrees with its proposed finite atomic Ticket profile plus structural wrappers, subject to real hook feasibility. A reasonable initial entry accepts copy inputs and returns a copy result. It can contain many lexical locals, strict initializers, typed integer arithmetic, booleans, selected branches, fixed finite helpers, moves, tuples and finite acyclic records. Borrowing and projected updates should enter only with a demonstrated executable hook/model correspondence; they must not be advertised merely because the general checker accepts their syntax.

The tracked atom is an opaque linear obligation. A fixed constructor/mint boundary creates it; its fixed terminal unpack/consume boundary ends it. This is different from unpacking a structural container, which transfers its child identities. Ordinary sink entry does not consume: `fn id(sink t: Ticket) -> Ticket { t }` transfers the same atom. Packing, returning that atom between helpers, moving a local and reading copy payload fields must not remint it.

Initially use structural wrappers whose nominal floor does not add a separate linear root obligation, and whose kind becomes linear through their tracked children. General nominal-linear wrappers with only copy fields, including empty linear structs, require an additional root-identity policy. Do not claim that a child-leaf ledger accounts for those roots. Affine implicit drop also needs an observed drop policy; the acceptance harness must not append invented consumes for leftovers. General enums, recursion, closures, continuation capture/resume, handlers, fibers, storage, regions and FFI resource adapters remain explicitly outside this first claim.

The source normalizer must independently traverse the actual parser AST and compare the whole admitted program to its DTO: declaration order, nominal floors and every field override; lexical name resolution including shadowing; types and pass modes; patterns and paths; strict initializer/evaluation order; branches; helper bodies; and fixed operation definitions. A type/kind, residual ownership state, expected diagnostic or expected event stream supplied by the request cannot substitute for independent checking. Invocation values must remain distinct from static facts. Every occurrence gets stable source-bound provenance before optimization; every transformed executed operation must retain that provenance.

## What a backend hook must prove by execution

Before source implementation is frozen, the hook owners should answer these questions with actual API and focused executed tests:

1. How does an opaque identity travel with the actual evaluator value and actual native aggregate representation? An independent syntax-driven shadow ledger is insufficient. A detached sidecar that stays correct when the payload is incorrectly copied can hide the very aliasing/codegen bug under test.
2. Where exactly are Mint and Consume emitted, and how is terminal atomic unpack distinguished from structural child transfer? A source helper called twice at one static definition must create two identities, with distinct call-occurrence provenance where appropriate.
3. How are provenance and observation retained through AST optimization, IR optimization, specialization, aggregate copies and helper inlining? Instrumentation must not be silently deleted/reordered, nor disable the optimizations being compared. Recording events after elimination by consulting the original source is not execution evidence.
4. How do bounded callback failure and isolation work? No panic/unwind across the native boundary; trace/ledger capacity refusal latches an error that dominates any returned value. No silent truncation, automatic consume on cleanup or shared global ledger across cases.
5. Can every execution return its actual terminal owned-resource residual, not just the emitted count? Resource results need explicit returned ownership; the initial copy-result profile instead requires an empty live linear ledger on a completed return.

Raw IDs must be fresh within an execution, including after consumption; reuse/ABA is an invariant failure. IDs must not derive solely from allocation addresses, payloads, source slot IDs or a static operation site. Two empty Tickets and two Tickets with equal copy payloads must remain distinct. A new case or mode starts a fresh namespace; process recycling must not let stale IDs or events contaminate it.

## Trace normalization and hostile comparisons

Each backend and the Lean evaluator independently produces its own bounded ordered successful events, provisionally `Mint(kind, site, raw_id)` and `Consume(kind, site, raw_id)`, plus outcome, copy value and terminal residual ledger. A failed attempted operation retains its actual prefix and structured failure detail. It is not appended as a successful consume.

Validate every stream before comparing it. Retain a raw-ID-to-canonical-ID map for the entire execution, including consumed IDs. Assign canonical ordinals only at observed successful mints, in mint order. Reject duplicate/reused mints, consume-before-mint, unknown IDs, repeated consume and kind changes before any cross-backend equality test. Compare every ordered event tag, resource kind, stable site and canonical ID, followed by the typed outcome and full terminal residual. Freeze a versioned canonical byte encoding so the reported bit-identical comparison has a precise meaning even when backends choose different raw IDs; retain raw IDs as separate evidence. A hash can summarize a successful comparison but cannot replace the first counterexample's actual event data.

These are required hostile tests of the validator/comparison layer, not replacement positive oracle evidence:

| Deliberate fault | Required observation |
| --- | --- |
| Mint A and B with equal payloads; swap their consume order | Different canonical ordered traces despite equal mint/consume counts |
| Mint twice at the same site; consume one ID twice | Duplicate consume rejected; site equality must not collapse the two obligations |
| Consume an unknown ID, or consume before its mint | Invariant refusal with the actual prefix |
| Reuse a consumed raw ID for a later mint | ABA/reused-mint refusal |
| Move/sink/pack/unpack introduces an extra mint or consume | Trace mismatch; no normalization rule erases transfer events |
| Change kind/site while preserving ID and counts | Mismatch/refusal; normalization changes ID spelling only |
| Omit a final event, truncate response or claim done with a live linear residual | Incomplete/mismatch, never a completed pass |
| Return the right scalar but the wrong resource identity/order | Failure independent of scalar equality |
| Send stale case/mode/session identity | Protocol refusal before committing a case |

Validator-only fixtures must be labeled synthetic. Positive evidence must additionally show genuine executions with multiple simultaneous resources, nested transfers and distinct consume orders; a fake peer must never produce an accepted mathematical answer or successful complete report.

## Static diagnostics and evaluation boundaries

For the admitted semantic negative matrix, compare complete ordered diagnostic vectors, phase and stable source blame—not merely the presence of a desired code. Rust checker recovery can add cascades. Isolate fixtures and execute the actual current checker before freezing their exact vectors; source inspection alone does not justify singleton expectations. The Lean checker must compute its answer independently rather than receive Rust's vector as an expected result.

The inspected checker associates field relaxation with E0105; type/name errors with E0200/E0201; linear loss with E0302; use-after-move with E0303; borrowed ownership transfer with E0305; moving a field out of a linear aggregate with E0306; returning branch disagreement with E0309; and live-linear abort frames with E0402. Additional borrow/pass/shape errors need their own real isolated fixtures. Keep parser rejection, semantic rejection, malformed DTO, unsupported feature, resource bound and harness/process failure as separate phases.

The negative matrix should cover unused/wildcard/shadowed linear owners; duplicated moves; sink identity followed by stale use; borrowing then moving; whole-root versus projected overlap; disjoint sibling paths; illegal field overrides; wrong tuple/record patterns; branch consume in one arm only; live linear frames across potential abort; and source-binding corruption. Reject unsupported empty nominal-linear-root cases explicitly until their identity policy exists.

Positive cases should include zero, one and many simultaneous resources; strict unused initializers that mint and consume internally; same-site repeated helper calls; multi-hop moves; nested structural packing/unpacking; valid full destructuring; selected branch order; and safe/overflow arithmetic at lifetime boundaries. The current relational full model's arithmetic rule always includes an abort label, whereas Rust can discharge checked arithmetic effects using independent range facts. Either repair that correspondence or restrict the profile explicitly; runtime input values must not be mistaken for static range facts.

Overflow before any mint can have an empty trace. Overflow after legitimate terminal consumes must retain the earlier trace. A completed language abort cannot excuse live linear obligations; actual checker rejection may prevent such execution. Fuel exhaustion, a native crash, timeout, invariant failure or capacity failure is an incomplete harness result with an honest prefix, never a language overflow or successful exhaustion proof.

## Fuel and finite process requirements

The model proposal uses default 4,096/hard 65,536 transitions, terminal-check first, one unit per nonterminal transition, with no per-local reset or invented cleanup. Its other proposed caps are 256 syntax occurrences, 64 slots, 8 records, 4 fields each, depth 4, 64 mints and 128 successful events. These remain proposed until the contract fixes expanded value size, helper expansion and all wire/metadata/work bounds. Existing arithmetic response/depth caps cannot simply carry a whole resource trace.

Acceptance should test zero and exact boundary fuel on tiny model computations, success at a sufficient budget, and one-less exhaustion with the exact committed prefix. Different CEK/IR/native machines do not consume identical transition counts: compare completed semantics, not numerical fuel usage. Current native JIT has no guest semantic fuel API; an OS watchdog is process isolation, not evidence of matching fuel accounting. Never count timeout/fuel/capacity-invalid cases toward a completed corpus.

Preflight frame lengths before parsing, and table counts, expanded type/value sizes and name/path depths before semantic materialization; the bounded parser itself needs finite token/nesting/retention limits. Frames use exact reads and reject oversized/truncated/trailing/unknown data. The persistent Lean driver receives finite data only, never expected traces or generated Lean source. Every successful session has confirmed sequence/case identities and counters, then an exact BYE/EOF handshake. Malformed/reordered/oversized output, wrong tool, missing model/receipt, wrong manifest hashes, crash, stderr flood, stall, queue pressure and cancellation need portable kill/reap/join tests using the production transport. Synthetic peers remain negative/transport-only.

Use recycled Rust workers to bound deliberately retained JIT modules, with an explicit maximum cases per worker and bounded concurrency. This is not an RSS/whole-process memory theorem. A killed worker can have unknown partial activity: report only confirmed counters, plus interrupted/unknown activity, rather than inventing exact completed calls. The trace prefix is evidence only for operations actually observed and committed; buffers lost on process death cannot be reported as complete.

## ChoiceTape corpus, coverage and replay

The generator must use actual Crucible `Source` draws to choose source structure, lexical ownership transfers, branches, declarations and invocation data. Persist the realized `ChoiceTape`, generator/profile version, seed and original case association. Strict replay must regenerate the identical DTO/source and pass the independent AST normalizer before rerunning real backends. Merely storing a seed beside a pre-ranked arithmetic corpus is not ChoiceTape generation.

A 100,000-execution fuzzer budget does not imply 100,000 completed valid programs. Count attempts, invalid draws, semantic negatives, duplicates, accepted distinct programs, completed eight-mode comparisons, failures and shrink executions separately. Define a program digest from normalized parsed semantics, excluding case numbers/comments. Distinguish distinct programs from distinct invocations of one program; changing only out-of-band inputs does not silently become a new synthesized program. Program uniqueness and committed completion must both hold for the reported 100,000+ campaign.

Require coverage across resource count and live-set peaks, same-site dynamic mints, move/borrow/transfer depth, wrapper shape/depth, consume permutations, distinct/equal payloads, branch outcomes, nominal mode/field constraints, and arithmetic/abort positions. A finite seeded boundary matrix should guarantee cap edges and hard cases independently of chance. Per-mode confirmed counters must demonstrate actual evaluator and JIT calls for every accepted case. Syntax counters and coarse scalar outcomes can be reported, but execution-guided native coverage requires actual executed feedback; the existing CLI's source-level operand stand-ins are not that evidence.

On first discrepancy, save the original source/DTO, realized tape, typed inputs, all completed per-mode outcomes, raw and canonical traces, residuals, exact failure category, profile/tool/model/source identities and bounded diagnostics before shrinking. Preserve the original even if shrinking fails. The shrink predicate must strictly replay a valid source-bound program and retain the meaningful mismatch class and affected backend/mode; accepting any later error can reduce a resource bug to an unrelated parse/fuel failure. Ownership-invalid edits are invalid candidates, not successful counterexamples.

Crucible's current shrinker requires a smaller `(tape length, sum)` key and reproduces a failure kind. A resource harness should encode the required failure category precisely and recheck it after every actual execution. Bound shrink attempts, keep them outside campaign totals, report zero honestly when disabled, and claim no global minimality. Session wire sequence stays separate from the original case ID during shrink/replay; use a fresh identified replay session after an oracle failure rather than silently restarting a dead session as the same run.

## Evidence gates and sibling review

1. Publish the exact profile, source-bound primitive policy, identity/provenance hook APIs and finite error/fuel contract after reviewing external #307/#308. No positive resource claim precedes actual hooks in both backends.
2. Implement the independent finite checker/evaluator and source normalizer; pin and kernel-audit the actual new model. Extend the external Lean4Lean replay profile/required roots to include this new model: the existing both-model replay does not automatically audit `ResourceOracle`.
3. Execute isolated actual checker fixtures and trace boundary/validator mutants. Demonstrate that deliberate missing/reordered/aliased actual hook events fail comparison, without giving either backend an expected trace. Recheck all optimized programs.
4. Run a bounded CI matrix through real pinned Lean and all eight actual Rust execution settings, with no skip on missing/wrong tools, plus portable hostile-process and isolation tests. Record actual target wall time before selecting CI timeouts/concurrency.
5. Execute the separately budgeted release 100,000+ distinct-program campaign and publish exact machine-readable results, tool/source/model associations, coverage, counters and counterexample/replay instructions. Hosted CI results and large-run evidence remain pending until executed; plan documents cannot supply them.

The model plan reviewed at `/workspace/vesper-worktrees/resource-oracle42-model-plan/docs/rfcs/resource-oracle42-model-plan.md` (commit `285aefc`) correctly treats the runtime seam as pending and distinguishes atomic terminal unpack from structural child transfer. Its finite multi-local checker, ordered ledger, transition fuel and diagnostic-order requirements are compatible with this acceptance plan. Its proposed wider borrow/update/function surface still needs actual source/runtime feasibility evidence before inclusion. Existing metatheory is relational and has arithmetic/nominal-root differences, so a computable model needs an explicit correspondence rather than an inherited full-source theorem claim.

The runtime plan reviewed at `/workspace/vesper-worktrees/resource-oracle42-runtime-plan/docs/rfcs/resource-oracle42-runtime-plan.md` (commit `13a6220`) independently confirms the missing generic observation hook and agrees on full-lifetime canonical maps, no raw-ID reuse, actual value/metadata propagation, source provenance through optimization and honest incomplete prefixes. A host Read-envelope ledger is correctly rejected as a substitute for constructor/destructor tracing. Its distinction between an instrumented semantic profile and unchanged uninstrumented binary behavior is necessary. Suggested wire/source/draw/worker/deadline bounds need measurements before freeze; they are not capacity evidence. Distinct invocation counts must remain separate from normalized program uniqueness. These are planning agreements; no runtime source implementation or positive execution was performed in this lane.

This document is the only changed artifact. It recommends future publicly claimed source/test work and does not authorize edits to another agent's hooks, core/native implementation, Lean model, Cargo files or CI.
