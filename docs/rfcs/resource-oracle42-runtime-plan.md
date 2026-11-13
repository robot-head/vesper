# Resource differential oracle: runtime feasibility and pending seam (#309)

This is a read-only implementation plan under public issue309. The only owned
change is this new document. Source inspected is main
`c226ee98695135ad59e64318213e79f911ad6325`, including merged295/278/287. No Cargo,
build, native compilation, expensive replay or existing binary invocation was
performed in this lane. The existing target/parallel-next executable is the
historical20ee baseline, not evidence about the current main source. The
coordinator's completed external-model/oracle work is a foundation, not resource
trace execution evidence for this proposal.

## Decision

A faithful resource oracle is **not executable through the current generic
evaluator/JIT APIs**. It needs actual dynamic identity/event observation in both
backends. The finite profile below is a feasible next contract once those APIs
exist and pass discriminating tests. Do not implement a source-to-trace function
and label it a backend execution, or pass the Lean answer into either backend.

There is an independent piece ready now: merged295 already makes
vesper-crucible a root Cargo dependency. The future standalone harness can use
its actual Source, ChoiceTape, replay and shrinking APIs without changing Cargo
or copying a similarly named PRNG. Native/compiler paths remain reserved by
external307/308; those issues are **not promised to implement resource hooks**.
The missing resource APIs are prerequisites for a separately claimed follow-up
after their edits merge or explicit ownership coordination, not a request to
broaden their existing scope. This document authorizes no overlapping edits.

## What exists in this source snapshot

| API/path | Actual availability | Limit for resource lockstep |
|---|---|---|
| parser::parse + check::check | Real source parsing, type/ownership/effect checking; checked World and TypeMap | Must remain the authority for actual compiler diagnostics, including the capability checks merged278. |
| Runtime::new/run, optimize/fuel | Actual Place-IR execution, with Expr continuations/fallbacks; optimize toggles its IR optimizer | No public generic mint/consume observer, resource UID or final ownership ledger. This is not a separate pure AST interpreter. |
| Runtime Value::Data | Type/constructor names and ordinary BTreeMap fields; Value is Clone | No hidden object/resource identity. Copying payloads cannot establish identity or consumption. |
| Runtime begin_external + Suspension | Actual linear external-effect requests, one-shot resume, arguments and inout writes | Useful host-effect boundary, but not observation of every resource constructor, pattern or sink transfer. Capability IDs identify effect installations, not arbitrary Ticket resources. |
| native::jit_with_options | Real zero-argument JIT compile/finalize/execute, explicit IR flag, final Value | No dynamic resource trace result or host-observer argument. |
| native::object_with_observer, merged295 | Callback receives each emitted CLIF function after finalization | Compile-time emission observer; instruction counts/CLIF visits are not executed resource events. |
| native::batch::NativeBatch | One owned JIT module, finite scalar/direct-call subset; frees executable memory on Drop | Optimization deliberately off, scalar profile only. Cannot supply the requested four-mode resource matrix. |
| native::session::NativeSession | Real typed one-shot fiber execution and compile_with_options | Only Yield(no args) or Read(one inout word-buffer), fixed enum result conventions. No generic mint→Ticket / sink-close operation hook. |
| src/crucible_commands.rs | Actual Source/ChoiceTape input generation and evaluator/native comparisons | Scalar source targets; native helper uses NativeBatch. Its documented coarse/source-literal feedback is not dynamic native resource coverage. |

Concrete source paths are Runtime IR Op::Data, AST aggregate completion and
Value::Constructor invocation. All construct ordinary Value::Data without UID
minting. bind_pattern clones values; native layouts have no hidden tracing
header, and aggregate code copies bytes. A linear empty struct may have no
usable machine identity. Do not infer identity from Value equality, a guest
`id:u64` field, an address, layout offset, source slot or aggregate zero.

One could reinterpret native-session Read buffers as an RPC to a host ledger,
or write source helpers that append markers to a mutable trace array. Such an
experiment would observe those host/helper calls. It does not establish actual
constructor/destructor ownership events, and can miss a duplicated/lost object
whose marker survives independently. It is not the proposed generic resource
oracle and must not be presented as an existing hook.

## Proposed finite source profile, conditional on hooks

Coordinate this with the model plan, not a second incompatible language:

- At most256 AST occurrences,64 named slots,8 record declarations,4 fields per
  record, structural type depth4, expression depth16 and64 dynamic atomic mints.
  No recursion, loops, arbitrary higher-order calls, user-defined effect
  handlers, regions, arrays, FFI, strings or unbounded allocation in this first
  profile. Fixed helper calls are acyclic and explicitly validated.
- Atomic tracked resource Ticket is linear and has one fixed mint boundary and
  one fixed terminal close boundary. Their real runtime semantics/identity
  carrier must be supplied by the backend owners. Do not pretend an ordinary
  source Ticket{id:literal} already has such a runtime contract.
- Structural tuples and copy/affine-floor records may carry existing atomic
  resources. Their moves, sink identity calls, borrows and full pattern
  decomposition transfer/retain child IDs; they do not mint new atomic IDs.
  Actual effective type mode is derived from fields and nominal floor, not a
  user-supplied local-mode flag. A nominally copy record containing Ticket is
  effectively linear when the real checker says so.
- Initially exclude an independently linear nominal wrapper whose own identity
  is distinct from its children, and exclude empty tracked nominal wrappers.
  Their root-vs-child mint/consume granularity needs a separate explicit policy
  and hooks. Do not silently equate unpacking a pair with closing its children.
- Use copy-only u64/bool inputs and a copy-only result. Wrapping copy arithmetic
  may compute a checksum. A resource-containing result is a later extension
  requiring result-owned residual IDs, not a false blanket leak verdict.
- Normal returns require exhaustion of every minted linear atom. Explicit
  abort/failure cases are admitted only with actual checked frame conditions;
  an already minted linear resource cannot disappear at an abort checkpoint.
  Trace/fuel/backend failures are harness failures, never successful language
  aborts. Keep this distinct from any later affine-drop event policy.

All field restrictions remain unchanged. In particular, src/check.rs emits
E0105 when `mode(field.ty) > field.modality`, implementing k_i⊑m_i. The oracle
must model and retain that restriction; generated declarations cannot relax it
to obtain more cases. Parse/check the exact rendered source, normalize the
actual parsed module back to the DTO and fixed helpers, check the original
World, AST-optimize a real clone with its actual TypeMap, then recheck. No dummy
World, hand-built accepted AST or bypass of new capability diagnostics.

Negative fixtures must be isolated and probed with the current ready binary
after its build. Preserve the actual complete diagnostic-code vector and phase,
including unexpected extras; compare only a published independent model class
for the explicitly isolated shapes. Do not filter diagnostics down to the one
code the model happens to implement. No current-main diagnostic probe is
claimed by this planning lane.

## Explicit pending evaluator/native dependency

The following is a **proposed** seam for coordination with307/308 and a later
Runtime owner, not an API available on main:

```rust
struct TraceLimits { max_events: u32, max_resources: u32 }
struct RawResourceId(/* backend-owned opaque execution identity */);
enum ResourceAction { Mint, Consume }
struct RawResourceEvent {
    action: ResourceAction,
    site: u32,                 // sealed semantic origin, not expected trace index
    kind: u16,
    resource: RawResourceId,
}
enum ExecutionEnd { Returned, Aborted(/* actual typed failure */) }
struct ObservedExecution {
    value: /* actual Value or typed failure */,
    events: Vec<RawResourceEvent>,
    live_resources: /* bounded actual ownership snapshot if exposed */,
    end: ExecutionEnd,
}
// Names are proposals; exact public signatures belong to the backend owners.
Runtime::run_traced(/* actual checked World/profile, args, fuel, limits */);
native::jit_traced_with_options(/* actual World/profile, IR flag, limits */);
```

Required properties before freezing a dependent executable harness:

1. Raw IDs originate at **executed semantic mint operations**. Out-of-band
   metadata follows the actual value through moves, clones, sink calls,
   aggregate copies, branches and decomposition. A clone does not mint a new
   resource. The ID is not reconstructed from the source environment or guest
   scalar payload. Native and evaluator implementations are independently
   instrumented, not one backend's trace relabelled as the other.
   **Out-of-band does not mean detached:** UID metadata must be inseparably
   coupled to the actual value/carrier and follow the real native aggregate
   load/store/projection/copy path. A separate syntax-driven shadow ledger can
   remain correct while the actual carrier aliases the wrong resource; that
   design is insufficient even if its event list is ordered. Mutation tests
   must corrupt the actual carrier/projection/copy and show that the observed
   identity changes or fails, rather than only corrupting the observer's list.
2. Consume events originate only at the agreed actual terminal operation,
   carrying the resource that reached it. They are not emitted just because a
   syntactic sink call exists or a host reference was dropped. Wrappers transfer
   atoms rather than closing/reminting them.
3. Site mapping is sealed to the parsed/checked source and the actual compiled
   artifact. It survives AST/IR optimization, helper specialization and any
   inlining. Site inventory supplies labels, not an expected sequence. Logging
   occurs only when the native/evaluator instruction actually executes.
4. All four optimizer settings really run. Instrumentation may not silently
   disable a pass or append source-derived events after a pass eliminates an
   operation. The owners must explain any trace-specific lowering/barriers and
   demonstrate what is observed. If this is an instrumented semantic profile,
   report that fact; it is not evidence of trace preservation in an unchanged,
   uninstrumented binary which has no resource events at all.
5. Event/resource counters never wrap; all represented buffers are prepaid and
   bounded. Overflow, trace-cap, invariant or callback failure is sticky and
   prevents success. Return the actual retained prefix on an orderly failure.
   Guest execution cannot reset a trace or substitute an expected result.
6. The implementation states the observation trust boundary. Tests must show
   execution provenance and ID propagation, not merely check a sample list
   produced by a callback. A bad observer which invents events is not repaired
   by comparing its output with the oracle.

The native owner may choose a proper host-resource primitive instead of generic
constructor instrumentation. That is acceptable only with a source-bound finite
primitive contract and real evaluator/native implementations. The harness must
wait for that contract; it cannot intercept ordinary helper names and call the
interception an implemented language primitive.

## Trace normalization that preserves counterexamples

Normalize each **actual execution independently**. Never feed Lean's IDs, event
count or expected result to a backend. For a successful Mint, allocate the next
canonical ordinal in observed event order and insert rawID→{ordinal,kind,live}.
Keep that map entry even after consumption. Reject a second Mint of the same raw
ID, including reuse after consumption (ABA). Each Consume must name an existing
live entry of the same kind; reject use-before-mint, unknown ID, double consume
and kind changes before performing trace equality.

Emit canonical events in original order as (action,semanticSite,kind,ordinal).
Do not sort, deduplicate, compare counts alone, assign IDs by source slots, or
rename every Consume to the next expected token. Distinct same-payload/empty
atoms must remain distinct. Reordering independent closes is visible when the
model specifies it; arbitrary numerical freshness alone is quotiented away.

Compare the complete ordered normalized trace, actual result/typed termination
and residual ownership. In the initial copy-result linear profile, normal or
completed-abort exits require no live atomic resources. Mint-minus-consume
residuals derived from actual events must agree with any actual backend exit
ledger; neither can silently drop a resource. If returned resources are later
allowed, the residual must identify their result ownership paths explicitly.

Fuel/trace limit is incomplete, retaining the actual prefix for diagnosis and
never satisfying exhaustion by forgetting the live map. A crash/OS timeout may
lose an unreported in-process prefix: record it as interrupted/unconfirmed,
not an invented empty trace. Malformed raw traces are invariant failures, not
ordinary mismatches and not data that a permissive canonicalizer repairs.

## Eight actual settings and bounded workers

For every valid case use the two independently checked Worlds (AST optimizer
off/on), and for each World run evaluator IR off/on and native IR off/on.
The eight results are executions of one program, not eight synthesized cases.
Runtime's IR flag does not turn it into an AST-only interpreter. Recheck after
AST optimization; any optimizer diagnostic or instrumentation refusal fails
the case rather than silently retrying a smaller language profile.

Proposed initial bounds align with the model:64 minted atoms and128 successful
mint/consume events; model fuel4096/default,65536/HARD; evaluator fuel100000;
raw IPC64 KiB per case/result, source64 KiB, <=256 source/trace sites. Fuel
counters have different units (model CEK transitions versus actual Runtime
steps), so require completion under each budget, not equal step numbers.

Use a standalone resource_oracle42 binary with a private worker mode and a
static ResourceOracle Lean driver. One real pinned Lean session, one outstanding
request at it, no expected trace in worker requests. Worker messages carry only
case/source DTO, actual copy inputs, mode and limits. Results carry each actual
value/termination/trace prefix and mode-specific failure; preserve partial
observations rather than marking unfinished executions completed.

Suggested process defaults are one to four workers, one outstanding case each,
an ordered window<=4*jobs,32 cases per worker/HARD128 including negatives,
request deadline30s/HARD120s, startup300s and shutdown5s. These are proposals
requiring actual measurements before freeze, not execution evidence. Current
ordinary JIT retains executable allocations; recycle workers until an owned
resource-capable JIT handle exists. NativeBatch's scalar/off-only handle cannot
be used as an apparent replacement for enabled modes. Bound stdout/stderr and
kill/reap before joining/cancelling pipe readers; queue pressure is backpressure,
not data loss. A source-shape bound is not an OS execution timeout.

Reuse the proven transport design by an explicitly claimed shared refactor or
new isolated harness module; existing arithmetic/external replay process modules
are private. Do not import their files as an undocumented public API or edit
their live claims during this planning phase.

## Crucible reuse and the100000+ obligation

Current Cargo already provides vesper_crucible. Source::random/with_prefix,
Source::replay, bounded draws, Source::realized, ChoiceTape and public shrink/
replay are genuine available primitives. Generation may maintain a static
typing context to choose valid moves/borrows and balanced branches; it may not
use that context as the alleged observed runtime trace. The actual model checker
and source checker still independently accept/reject every generated case.

Bound each generator to<=2048 realized draws (HARD8192), with actual grammar
bounds making that explicit; do not rely only on Crucible's much larger global
MAX_TAPE_LEN. Save the original seed, case ID, **realized clamped tape**, source
and first mismatch. Strict replay must regenerate the same DTO/source and run
the unchanged complete property again. Shrinking preserves the mismatch class,
runs real Lean and all required backend settings, and keeps separate counts.
Never call a dead oracle session a successful shrink; infrastructure failures
are not resource counterexamples.

The public fuzz(config,property) API automatically shrinks found failures and
its execs include invalid runs. Thus max_execs=100000 is not proof of100000
completed valid lockstep programs. The first harness can use Source/ChoiceTape
for deterministic generation/replay and explicit healthy-session shrinking;
describe that honestly as ChoiceTape generation rather than claiming native
coverage-guided search. For an actual guided Crucible campaign, freeze bounded
feedback labels/corpus storage and stop/shrink behavior for infrastructure errors
before adopting fuzz directly. Source-literal or fabricated CLIF feedback is
not executed native resource coverage.

Report attempted/generated, invalid, static-negative, distinct valid, committed
valid, Lean answers and each of eight completed-mode counts separately. Achieve
at least100000 **actually completed valid** programs for the full claim, plus
isolated negatives. A full pilot/prefix/tape count cannot be extrapolated.
Store only bounded fingerprints for uniqueness; if distinct-source claims are
made, preserve actual differing parsed executable literals or structure, not
comments or case IDs. Embedded invocation literals in a checked main wrapper
change the closed source program; changing only out-of-band runtime inputs to
one unchanged parsed program is invocation uniqueness, not100000 synthesized
programs. Report these separately. No claim of distinct resource traces follows
from distinct programs. Before source freeze, fix a versioned canonical byte
schema for ordered actions/sites/kinds/IDs, typed outcome and residual ownership;
hashes summarize those exact compared bytes, not an unspecified equivalence.

## Ownership, prerequisites and actual acceptance

Current approved edits are this plan only. Before implementation, root should
freeze the model+runtime+acceptance plans together and publish child ownership:

1. After307/308 merge or explicit ownership coordination, separately claim and
   implement the dynamic observer/primitive/API dependency and Runtime work;
   their present scopes do not imply those hooks. No competing native/main/lib
   edits are authorized by this plan.
2. Model lane owns a new fixed ResourceOracle model/driver, with the existing
   metatheory bridge and nominal-root identity gap stated honestly. External
   metatheory258/289 remain reserved; do not copy/edit their unfinished proofs.
3. Corpus/renderer lane owns a bounded DTO/ChoiceTape generator and actual
   source-normalization check; a separate worker/harness lane owns isolated
   execution and raw trace validation. A third independent acceptance lane
   tests the real hooks, canonicalizer, provenance and failures.
4. Current external-checker foundation allowlists exactly two models. A new
   ResourceOracle is **not** externally checked by those passes. Adding it
   requires a separately published fixed-target/profile/pins/root-inventory
   extension after the current reserved slice; no arbitrary-module fallback.

Acceptance must include actual same-payload distinct atoms, sink identity,
borrow stability, tuple/record split+repack, independent branch order, exactly
one close on each path, abort after clean-up, and failures with a live resource.
Mutation tests must make each real backend omit/duplicate/reorder an actual
operation or corrupt propagated identity and observe refusal/mismatch. Test
raw-ID reuse after consume, unknown/double consume and prefix exhaustion in
the real canonicalizer. Verify both requested optimization flags execute,
source association is exact and all compiler diagnostics remain intact.

Until the backend dependencies and these tests pass, the correct result is
"runtime observation seam pending," not resource lockstep completion. No
current-main resource execution,100000-program resource run, checker refinement
proof or full §42 completion is claimed here.
