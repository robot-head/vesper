# Resource oracle planning resolutions (#309)

This records the reviewed model, runtime and independent acceptance plans at
main `c226ee98695135ad59e64318213e79f911ad6325`. It is not a frozen source
contract and authorizes no resource-oracle source writer. All three planning
lanes performed source review only, without current-baseline execution.

The exact specification snapshot is
`8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d`.
The document's modification metadata remains 2026-11-13T04:46:25.948Z.

## Findings and ownership

The evaluator and ordinary JIT lack dynamic resource identity/event hooks.
`Value::Data` is cloneable and idless; native aggregate lowering can erase empty
nominal data. Compile-time CLIF observers cannot supply executed events, and
native-session Yield/Read signatures do not provide a generic mint/sink ABI.
External #307/#308 are not promised to supply that functionality. A separate
public claim must follow their merges or explicit source ownership coordination.
No native/runtime/main/lib edit is authorized by these plans.

Actual Crucible Source/ChoiceTape/replay/shrink APIs are available through the
root dependency added by #295. Existing arithmetic generation is not already a
resource ChoiceTape campaign. The fixed two-model external-checker profile
also does not cover a future ResourceOracle module; that needs its own reviewed
profile/root/pins extension, preserving strict coverage and receipt association.

The metatheory is relational rather than a computable checker/evaluator. Its
always-abort arithmetic rule and nominal-root identity semantics cannot simply
be inherited as full source refinement. The independent bounded literal bridge
is separately implemented under #313/PR #314; symbolic facts, four kinds and
executable finite-context correspondence remain further work.

## Decisions for the next freeze

The first proposed profile uses atomic linear Tickets plus structural wrappers,
copy inputs/results and bounded acyclic helpers. Wrapping/structural unpacking
transfers child identities; it does not close/remint them. Independent nominal
linear container-root obligations, physical resources, arbitrary handlers and
continuations require explicit later semantics. The exact copy-kind, borrow and
projected-update surface remains conditional on real fixture/hook evidence;
the broader model proposal is not silently an implemented runtime profile.

Preserve linear exhaustion and the field restriction `k_i ⊑ m_i`. Every backend
must obtain opaque identity at an actual semantic mint, carry it inseparably
with the actual value through aggregate load/store/projection/copy, and report
its actual terminal consume. Guest fields/addresses and detached syntax/shadow
ledgers are insufficient. Real-carrier mutation tests must expose aliasing and
wrong-field copying, rather than only changing an observer's event list.

Freeze source-bound provenance before optimization and preserve it through all
actual AST/IR optimizer settings. Instrumentation must not silently disable
passes or synthesize events after their operation is eliminated. Any
instrumented-only semantic profile must be reported as such, with no ordinary
untraced compiler-refinement claim. No backend receives an expected trace.

Canonicalization changes identity spelling only: new ordinal at each observed
fresh mint, one lifetime map retained after consume, no ABA reuse, and rejection
of unknown/double/wrong-kind consumes before ordered trace comparison. Compare
actions, kinds, sites, canonical IDs, typed outcome and terminal residuals.
Counts or digest equality alone are insufficient.

Proposed limits are not measured capacities. The source freeze must bound
**expanded helper executions**, not merely distinct helper-definition nodes:
repeated calls can execute one mint site many times even without recursion.
Compute checked expansion summaries or pre-expand call occurrences before
admission, including all branches, type/value trees and provenance tables.
Prepay the complete wire envelope and decoded scratch/retained metadata; a
source cap equal to a frame cap leaves no room for other request fields.

Fuel is terminal-first and global across reference transitions. Exhaustion,
crash, timeout or capacity failure retains only actual confirmed prefix data;
it is never cleanup/exhaustion success. Static diagnostics compare complete
ordered vectors from isolated current-source fixtures and independently
computed model results. Runtime inputs never become static facts.

Actual ChoiceTape generation/replay must preserve normalized source and typed
inputs. Distinct programs, invocations, attempts, invalids, negatives, shrinks
and fully committed eight-mode cases need separate counters. A budget of
100000 fuzz executions does not establish 100000 completed valid programs.
The full resource campaign remains unexecuted.

Source authorization requires published actual observer APIs and execution
proof, exact finite semantics/diagnostic vectors/encoding, independent boundary
acceptance and child claims. These documents supply a concrete dependency map;
they do not supply positive resource-trace, full-model or bootstrap evidence.
