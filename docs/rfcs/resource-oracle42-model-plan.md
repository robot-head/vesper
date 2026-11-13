# Executable resource reference model: feasibility plan (#309)

Historical planning checkpoint for #309. The separately claimed #317 now implements the reference-evaluator provider with its own narrower [frozen API/profile](runtime-resource-trace42-contracts.md) and [actual evidence](runtime-resource-trace42-integration.md). The model/native/generator/campaign proposals below remain future work; they are not implemented behavior or a full-harness source authorization.

This is a source-review plan at repository baseline `c226ee98695135ad59e64318213e79f911ad6325`, not an implementation or execution report. No Lean/Rust source, tests, Cargo files or compiler hooks were changed, and no build, model replay or CLI probe was run. The compiled CLI is older than this baseline and was not used as current evidence. Only this new document is owned by this lane. The specification snapshot SHA256 remains `8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d`, with Google modification metadata `2026-11-13T04:46:25.948Z`.

The useful next step is a computable multi-local resource judgment and fuel evaluator with an ordered dynamic ledger. A real backend observation seam must be established before claiming differential resource traces. The first bounded logical-token profile can provide a foundation for the full reference model; it does not complete the reference semantics requested in §42.1/42.2, the compiler-refinement proof, physical-resource accounting, self-hosting or DDC.

## What exists, and what cannot be assumed

The source reviewed was the exact §42.1/42.2, the surface grammar and modal rules in §§3–6, Appendix A, and these current files:

| Existing component | Relevant implemented surface | Missing bridge |
| --- | --- | --- |
| `lean/MetatheoryAppendix.lean` and snapshot Appendix A | Mode lattice, field well-formedness, projection/frame judgments, one-local relational `HasType`/`EvalDet`, conservation theorem | Not a computable multi-local checker/evaluator. Separate minted/consumed lists or equal list lengths do not establish equality of interleaved event streams. |
| `lean/MetatheoryFull.lean` | Multi-local de Bruijn calculus; function-valued contexts; modal pair fields; ANF let/unpack/branch; field-tree get/swap/swap2; one-shot handlers; relational small steps with ordered `Act.mint`, `consume`, `capture`, `resume`; live-resource store; progress, preservation and exact-once exhaustion | `HasType`, `Head`, `Step` and `Steps` are propositions, not executable decision procedures. Function-valued contexts/stores require a finite representation and a proved correspondence. Source AST and runtime events are not bound to this model. |
| `lean/ArithmeticOracle.lean` | Executable validated array-local checker and evaluator, strict unused initializers, independent arithmetic facts, fuel and bounded framed IO | Private wrappers and DTOs describe only copy integers. No owned places, nominal resource IDs, borrow loans, resource events, branches, records or modal diagnostics. Its 512-byte response cap is insufficient for resource traces. |
| `crates/vesper-core/src/ast.rs`, `crates/vesper-syntax/src/parser.rs` | Actual mode/pass/field/record/pattern/block/if/match/call/effect syntax; field modality defaults to linear | No source-language `mint` or `consume` keyword. A model primitive needs a fixed, source-bound lowering or real operation hook. |
| `src/check.rs` | Kind inference, declaration field restriction, path moves/loans, scope exhaustion, branch merge, abort-frame checking, integer facts | No public independently executable ownership judgment or resource-identity trace. `World.moves` and successful static analysis describe ownership transfers, not observed terminal releases. |
| `src/runtime.rs` | Explicit CEK/IR evaluator; ordinary values and scoped external effects | `Value::Data` has no resource identity; `Slot::get`, environment/IR movement and pattern binding clone representations. A clone is not necessarily a resource copy, and a move is not terminal consumption. |
| `src/native.rs`, `src/native_session.rs` | Real JIT/aggregate lowering, compile-time Cranelift function observer; native linear-effect session lowering | Compile-time `EmittedFunction` observation is not a dynamic operation trace. Ordinary JIT entries must handle effects. Session bindings support only Yield/no arguments/enum result and Read/one inout word buffer/enum result, not arbitrary mint(site) or consume(site,id). |

The last point is a concrete prerequisite, rather than evidence that a generic resource effect already works. `Runtime::begin_external` is a genuine evaluator operation boundary, but its native counterpart currently has narrower signatures. Pending external native-hook work (#307/#308) owns its source and must be assessed by its actual published API; this planning lane neither edits it nor assumes it supplies this seam.

Two semantic mismatches must be addressed before claiming reuse of existing proofs:

1. `MetatheoryFull.HasType.arith` always requires the abort label. Rust's checked add/subtract/multiply independently discharge that effect when constant facts prove the operation in range. Thus a mechanical executable version of the relational rule can reject safe arithmetic while a linear local is live. A bridge needs a range-proved non-aborting arithmetic rule, or a deliberately narrower first fragment. Actual runtime input values must never become static facts.
2. The full calculus records atomic handle/continuation allocation, but constructing a pair with nominal floor linear does not mint a separate pair-root identity. The source permits nominal linear structs with only copy fields, whose terminal unpack discharges their nominal obligation. The policy for dynamically identifying those nominal roots must be defined; counting only embedded handle leaves is not automatically a full-source linear-obligation semantics. Its arithmetic type is also a u64-like natural, rather than the four distinct signed/unsigned kinds; arbitrary record fields require an explicit encoding or extension of its binary pairs. Existing theorems therefore cannot simply be asserted as a theorem about the proposed richer executable AST.

## Proposed finite foundation, with a route to the full model

Use a new static module and namespace, provisionally `ResourceOracle` / `Vesper.ResourceOracle`, importing existing arithmetic meanings and the shared modal definitions where practical. Do not change the meaning of the arithmetic oracle protocol or pretend its private arithmetic-only checked wrapper admits owned resources.

The first executable profile should be source-representable, closed over resource inputs, and return a copy result. It can admit multiple copy inputs and many sequential locals, rather than retaining the arithmetic oracle's three-local ceiling. Separate semantic negatives from structural malformed requests and unsupported language features.

| Construct | Foundation semantics and source binding |
| --- | --- |
| Unit, bool, u8/u32/u64/i64 | Exact typed values. Reuse `RewriteWitness` checked/wrapping operations; literals fit their actual contextual kind. Inputs carry types and invocation values separately. |
| Lexical typed `let`, blocks, sequence, names | Strict initializer execution once, including unused locals. Each lexical binder has a unique static slot; source name resolution is independently reconstructed, including shadowing. A slot ID never doubles as a runtime resource ID. |
| Fixed atomic linear Ticket | Logical resource carrier with opaque runtime identity, created and terminated only by fixed source-bound boundary operations. No arbitrary construction, projection of its identity field, or unpack outside that boundary in the foundation profile. This restriction is necessary, not a claim that the general source language forbids such code. |
| Tuples and finite acyclic records | Structural containers over the admitted types, with explicit declared field order and validated overrides. Initially restrict user record nominal floors to copy/affine: a wrapper becomes linear through its tracked fields. Packing/unpacking transfers contained IDs without reminting/releasing them. Nominal-linear-root identity is a later explicit extension. |
| Full tuple/record unpack | Moves the aggregate, introduces every field with its actual component kind, checks wildcard losses, and preserves each contained identity. Atomic Ticket terminal unpack inside the fixed terminal operation is different: it ends the tracked atomic obligation. |
| `if` with bool input/condition | Check both arms independently from the post-condition context. Compare ownership residuals on returning paths. Evaluate only the selected arm. No eager evaluation of flat node records. |
| Owned move, fixed sink identity/helper calls | Transfer the same value and identities. Sink entry is not terminal consumption: `fn id(sink t: Ticket) -> Ticket { t }` must mint and consume nothing. |
| Shared/exclusive projected calls and copy-field update | Fixed finite signatures with `in`/`inout`/`sink`, valid field paths, left-to-right argument checking, live loans and prefix-overlap checks. Mutating copy leaves is allowed; assigning over an owned linear field cannot discard its old obligation. Finite helper bodies/signatures must be fixed and source-bound, not trusted from Rust World. |
| Checked arithmetic and Fail | Independent static facts determine potential overflow. Actual overflow/explicit abort has a distinct semantic outcome, and is permitted only after the frame obligations are safe. Admit only the fixed Fail boundary until full lexical effects/handlers are executable. |

This is a proposal for contract review, not a claim that all rows can be shipped before the boundary seam exists. In particular, source-bound Ticket operations and the Fail boundary must first receive actual evaluator/native feasibility probes. If a row cannot be executed faithfully, report that prerequisite and withhold that row's differential claim rather than supplying a syntax-derived trace.

General enums/exhaustiveness, generics and recursive ADTs, arbitrary user functions/recursion, closures, full effect rows/capability identities, stateful handlers, continuation capture/resume, cancellation, region/storage/FFI resources, fibers and concurrency remain explicit later extensions. The foundation must retain an extension path to these constructs, not rename this fragment the full oracle.

## Computable representation and checker API

Recommended API shape (proposed declarations, not implemented source):

```text
validate : RawProgram -> Except RequestError ValidatedProgram
checkExpr : StaticConfig -> DeclTable -> Context -> ExprRef
         -> Except CheckFailure CheckResult
checkProgram : ValidatedProgram -> Except CheckFailure CheckedProgram
step : MachineState -> Except RuntimeInvariant StepResult
evalSteps : CheckedProgram -> CopyInputs -> Nat -> EvalResult
```

Here `CheckFailure` contains a bounded ordered diagnostic vector, with structured category and blame per diagnostic; it is not necessarily a singleton first error. Validated and checked constructors must be private. The executable checker must derive the type, kind, effect/fact information and resulting ownership context; the request must not supply accepted flags, inferred kinds, residuals, a split proof, expected diagnostic, expected value or expected trace.

Use bounded arrays for declarations, finite contexts, slots, values, resource ledger and machine frames. Do not turn the function-valued `Ctx`/`RCtx` into an unbounded search for a context split. The algorithm follows the source's deterministic evaluation/ownership order: copy reads retain availability; owning reads of affine/linear values move the relevant path; borrowed parameters never transfer their non-copy contents. Keep type and pass mode distinct: a plain integer binding does not acquire a linear exhaustion obligation merely because it was received through sink.

Each binding records its lexical slot, type, pass/projection bounds, moved paths and causal sites; checking expressions returns the updated context and optional integer fact. Both branches start from the same residual context; returning linear states must agree exactly, while affine moved paths conservatively union as in the current checker. Never-returning arms do not manufacture a returning ownership obligation. Scope exit must exhaust every surviving owned linear binding, including shadowed bindings; return through a sink value transfers its obligation rather than releasing it. The externally compared entry returns copy, so final outstanding owned linear resources cannot be exported silently.

Call arguments are checked in source order, retaining earlier argument loans while checking later expressions. A whole-root path overlaps every descendant; sibling paths are disjoint only after a divergent component. Shared/shared overlap is permitted. Exclusive or ownership-taking overlaps are rejected; passing a borrowed parameter as sink cannot create ownership. This includes nested argument evaluation, not just a final pairwise list of argument paths. Abortable effects inspect live owned linear frames, active inout parameter frames and exclusive argument loans at the actual checkpoint. Proven-safe arithmetic must not spuriously create such a checkpoint.

For every declared field, independently derive `k_i` under its type environment and require `k_i <= m_i` before admitting the declaration. Then compute `m_decl join join_i(k_i meet m_i)`. Unannotated fields default to linear. A Ticket field annotated copy/affine is invalid; a copy wrapper around an unannotated Ticket is linear. Unpacking binds at actual component kinds, not at a relaxed enclosing nominal floor. Reuse the existing lattice meaning, not a second incompatible ordering.

A source AST binding record should include stable occurrence IDs, lexical slots, field indices and spans. Compare normalized parsed source back to the entire request and fixed boundary definitions before checking/executing any backend. Include pass modes, projections, all field overrides, branch ordering, function/effect declarations and result/parameter types. An optimizer output is not the authority for the initial program. Any later generated Lean goes through `vesper-lean-syntax` nodes and one final render; this plan introduces no generated Lean text.

## Dynamic resource and trace semantics

A resource ID names an obligation, not a slot, object address, integer payload, source variable or transient backend allocation. The runtime planner independently confirmed that no generic observer exists in this baseline. The recommended real hook must propagate an out-of-band identity through actual evaluator/native moves and representation clones; inferring identity from a guest id field or address does not establish that propagation. Model values retain identities through moves, helper calls, containers and branches. Copy values contain no tracked obligations. A copied runtime representation of an owned value is not automatically an additional obligation or an additional consume.

For the foundation atomic profile, specify successful ordered events:

```text
Mint(kind, occurrenceSite, rawId)
Consume(kind, occurrenceSite, rawId)
```

A dynamic mint selects a genuinely fresh identity, creates a live ledger entry and appends one Mint. A terminal consume first requires that exact ID/kind to be live, marks it dead and appends one Consume. A repeated/unknown/foreign-kind consume is an invariant failure, not another successful release. Preserve its bounded failed-attempt detail and trace prefix in failure reporting. Do not silently drop a duplicate event to make counts agree.

Moves, ordinary sink entry/return, structural packing/unpacking, and copy-leaf reads append no terminal event. The fixed atomic terminal operation discharges its atomic obligation exactly once. A future nominal-linear aggregate extension must explicitly specify its own root mint and terminal-unpack events separately from child transfers, including empty records and nested nominal roots. A future continuation extension should retain distinct Capture(mode,id) and Resume(id) events matching `MetatheoryFull.Act`; affine continuation abandonment must not be mislabeled a linear terminal consume.

Normalize only identity spelling. Independently for each observed stream, assign canonical ID 0,1,... to each new raw ID in observed mint order, then resolve every consume through that same bijection. Reject duplicate raw-ID minting/reuse within an execution, consume-before-mint, repeated consume, unknown IDs and wrong-kind use before comparison. Do not assign fresh canonical IDs on each consume or collapse aliases to first/last occurrence. Compare event sequence length and every ordered tag/kind/site/canonical-ID, plus outcome/value and the complete terminal residual resource set. Do not sort events, compare only totals, match resources by value, or compare only a trace digest.

This catches double consumption, aliasing, lost resources, consumes of the wrong freshly minted resource, swapped terminal operations and changed branch/evaluation order. Sites must identify actual source-bound primitive occurrences, not inferred expected event positions. Freeze a source-bound occurrence table before optimization and carry that provenance through the instrumented operation; never backfill a site by consulting the expected trace. A helper boundary must distinguish call occurrences rather than labeling every call with only its shared helper definition site. Resource ID normalization cannot erase a mismatching site. Closed normal returns and language aborts require no outstanding linear ledger entries; fuel/timeout/invariant failure is not a terminal exhaustion success. Future open-input resources require explicit initial ledger identities and result-resource transfer accounting rather than invented mints.

## Fuel, failure and finite admission

Fuel counts reference machine transitions, not compiler work, Rust CEK steps, CPU instructions, allocated bytes or a claimed backend trace. Check for a terminal configuration first; otherwise, if fuel is zero, return FuelExhausted with the actual partial trace/state. A successful transition consumes exactly one unit; its state/resource event updates are committed together. Fuel is global across every initializer, selected branch, unpack, helper frame and final expression. No reset per local or subtree, and no automatic cleanup/consume appended on exhaustion.

Proposed starting caps for review are 256 syntax occurrence nodes, 64 lexical slots including inputs/pattern fields, 8 acyclic record declarations, 4 fields/record, type depth 4, 64 expanded value leaves, 64 mint occurrences and 128 successful resource events. Default reference fuel 4,096 and hard 65,536 are suggested, not yet frozen. Preflight counts all branch syntax and potential mint occurrences before evaluation; with no recursion/loops, executed mints cannot exceed that occurrence bound. Check expanded type/value size with checked arithmetic before materializing a record tree. A bounded node graph represents syntax occurrences, never shared executable subterms evaluated once by accident; repeated local reads use already evaluated values.

For any framed server, send finite data only: typed inputs, flat declaration/pattern/node tables, copy invocation values and the fuel bound. There is no expected-answer/trace field and no generated Lean request. Length, lexical nesting/scalar/name limits and table counts must be checked before parser/semantic allocations. Derive fresh finite response and nesting caps from this schema; the arithmetic server's depth-four and 512-byte response limits cannot be reused unmodified. Return the whole bounded ordered trace and residual ledger, not merely a digest. One healthy Lean process serves many requests.

Use an explicit finite-control CEK/work stack with left-to-right evaluation and bounded frame/value storage. Do not rely on unchecked host recursion on untrusted depth or a potentially expanding substitution evaluator. A later proof can relate finite states to the relational model's substitution semantics.

Outcomes must distinguish Value, LanguageAbort/Overflow, FuelExhausted, RuntimeInvariant and HarnessFailure. Neither fuel exhaustion nor a process deadline/crash is an arithmetic overflow or a comparison pass. An invariant failure from checked code is a bug to retain, never an accepted language diagnostic. Transcript/event/metadata caps need separate preallocation admission and explicit errors; no failed or truncated trace contributes a successful corpus count. No physical-resource or whole-process resource bound is inferred from these reference limits.

## Static diagnostics and acceptance requirements

These code associations are established by the reviewed current checker branches, not by executing new fixtures:

| Independent condition | Actual checker code |
| --- | --- |
| Field override relaxes component ownership | E0105 |
| Unknown binding / field / incompatible type | E0201 / E0202 / E0200 |
| Invalid inout/place requirement / overlap / projection-bound violation | E0300 / E0301 / E0304 |
| Unused linear result, wildcard loss, live-linear shadowing or unconsumed scope binding | E0302 |
| Use after move / non-copy move through borrowed parameter / move field from linear aggregate | E0303 / E0305 / E0306 |
| Mutate shared parameter / explicit pass mismatch | E0307 / E0308 |
| Returning branch ownership disagreement | E0309 |
| Missing effect / abortable checkpoint crosses live linear or inout frame | E0401 / E0402 |
| Pattern field shape / non-irrefutable let | E0211 / E0210 |

A computable semantic error should carry a stable category, occurrence/slot/path blame and stage. The current Rust checker accumulates diagnostics and performs recovery; isolated negatives can still have cascades. Before freezing exact one-to-one agreement, execute each proposed source fixture with the real current checker, record the complete ordered code vector, and make the independent Lean checker compute the corresponding vector/order for this profile. Do not manufacture an expected vector by copying Rust output into the oracle or claim full diagnostic equivalence by matching only any one code. If a first implementation deliberately accepts only isolated negatives, label that finite contract explicitly. Malformed wire data, unsupported language, fuel, process failures and semantic rejections remain separate categories.

Required new tests must include two or more simultaneously live resources, move chains, identity-returning sink helpers, structural pack/unpack, nested fields, disjoint/overlapping borrow arguments, same-resource aliases, swapped consumes, duplicate/unknown/reused IDs, live-linear shadows, both-branch consumption and one-arm loss, nonreturning arms, unused initializers, safe/overflow arithmetic before and after terminal consume, Fail frame restrictions, invalid field overrides and empty nominal linear types as an explicit deferred-policy guard. Shrinking must preserve actual source binding and re-execute both sides; it must not replace an observed trace with a syntax walk.

The execution harness must run the actual Lean checker/evaluator and actual Rust checker, then actual evaluator and actual Cranelift code in each claimed optimization setting. A source helper implementation that logs while it actually executes can be legitimate narrowly instrumented evidence; a generator that constructs the expected trace from syntax cannot. The observed backend path must never receive the Lean expected answer or trace. Physical files, regions, allocations and cancellation require separate real adapters and cannot be established by logical Ticket events.

## Prerequisites and completion milestones

1. Freeze the atomic-versus-nominal obligation policy, exact finite profile, full diagnostic ordering and fuel/error contract. Keep the preserved linear exhaustion and `k_i <= m_i` premise mandatory. Coordinate this model seam with the runtime planner and independent acceptance planner.
2. Establish the smallest genuine backend operation seam. Either add reviewed evaluator/native hooks with preserved IDs and ordered events, or demonstrate a fixed transparent Vesper helper library whose executed state writes supply the full trace in both engines. The helper option must prove its current Fail/inout/frame and optimization support with actual prototypes; it is not assumed feasible here. Native compile-time observers, static moves and the existing restricted session API are insufficient substitutes.
3. Implement the bounded computable checker/evaluator, immutable checked wrapper and complete trace ledger, with independent semantic tests and pinned kernel compilation/audit. Relate its finite contexts/store and machine steps to existing metatheory where meanings coincide; explicitly repair the arithmetic/floor gaps above. A passing model compile is not a refinement theorem.
4. Bind complete parsed source to requests, implement actual dynamic backend collection, validate/normalize IDs without erasing failures, then run finite negative/positive boundary tests before any large corpus. Reuse process isolation and bounded framing concepts from the arithmetic harness, not its insufficient trace payload cap. Lean needs one persistent process, not a source compilation per test case.
5. Run and report the genuine 100,000+ synthesized-program lockstep for the exact admitted profile, including ordered resource traces and full diagnostics. Publish actual seeds, counts, failure/minimization evidence, source/tool/model associations and exact external-checker coverage. Existing arithmetic corpus success is not resource corpus evidence.
6. Extend to nominal linear identities, enums/functions/closures/handlers/continuations and physical resource adapters, then full source semantics and checker/compiler refinement. Completion requires the specification's full reference model and bootstrap obligations; foundations and finite-profile test runs remain explicitly partial until those gaps close.

These milestones are recommendations requiring future published contracts/ownership claims before source writers begin. This document does not authorize changes to external agents' native hooks, metatheory, compiler/core or Cargo paths.
