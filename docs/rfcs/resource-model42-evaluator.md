# Finite resource evaluator (#322)

This lane owns only new `lean/ResourceModel42/Evaluator.lean` and this report.
The frozen contract is `ddf5d6c`, clarified by `0399d20`, read from the
coordinator's integration worktree. The first evaluator source checkpoint
imports compiled syntax `d51a21b` and checker `b9ce0c0`. Its definition module
compiles with real Lean 4.24; semantic acceptance and downstream proof gates
remain pending. No Cargo execution is claimed by this lane.

## Operational representation

Use the syntax module's decorated runtime expression and closed value types,
with finite tombstone ledger entries and an ordered successful action prefix.
Only mint and consume operations retain logical source labels at runtime.
Values carry unit, bounded naturals, structural pairs or actual dynamically
minted serials; they have no fabricated input operation sites. Model sites are
not yet source-language byte spans or backend observations.

The evaluator owns an opaque invocation wrapper. Its only admission function
checks separate copy values against the checked program's declared input types
and all input value/depth/aggregate caps. It does not infer source facts from
these actual values, accept expected answers or traces, or expose a constructor
that accepts unchecked admission flags. The checked program comes from actual
checker success through its frozen readonly accessors.

Initialization closes inputs using pure nested value lets with input zero
innermost. These ordinary `Full.Head.letV` reductions consume global fuel.
The wrappers count toward whole-expression nodes and depth.
Structural pair values are already values: recognizing a pair of closed value
atoms must erase to the identical Full expression rather than inventing an
extra operational transition.

## One semantic transition

Search only strict let initializer contexts for the next redex. An unused
initializer still executes. Full unpack and if-zero operate on atoms; only the
selected branch continues. Unpack substitutes the right component at index zero
first, then the left component at index zero. Closed value substitution requires
no value shifting and preserves surviving mint/consume operator labels.

Each successful transition corresponds to exactly one `Full.Head`, lifted
through the actual enclosing let context to one `Full.Step`. Internal focus
search, metric traversal and reconstruction are not separate semantic fuel
steps. The complete successful action prefix includes silent actions for the
proof bridge; the public logical resource trace filters those silent actions.

Mint obtains its fresh serial from total allocation history, appends a live
ledger entry and one ordered mint action. Consume checks that actual serial is
live, marks its retained entry dead and appends one ordered consume action.
Packing, unpacking, substitution and ordinary binding never invent terminal
resource actions. Tombstones remain until the run ends and serials never reuse
the live count, lexical index or source occurrence label.

Terminal detection precedes the fuel-zero test on an admitted state. Otherwise,
fuel zero retains the actual current expression, ledger and trace prefix. One
successful transition costs one unit of the same global fuel. Completed accepts
any admitted result kind and returns its actual residual identities; empty
residuals are not an evaluator postcondition imposed to simulate a proof.

## Prepaid bounds and refusal

All model limits retain the contract's default and hard values. Raw source and
inferred type bounds alone do not control substitution: repeated copy-pair
doubling can produce a large runtime tree from small ANF syntax. Metric
arithmetic saturates at the relevant cap plus one before materializing copied
values or a new expression. Both nodes and depth require independent checks.

Preflight the complete prospective root state, including unchanged let bodies,
both branch subtrees and every enclosing let node. Either focus with an explicit
context and count its retained overhead, or pass the remaining node/depth budget
to the initializer planner. Building a full-cap inner result and only then
measuring its reconstructed parent is insufficient.

For unpack, check the intermediate right substitution and the final left
substitution before semantic commitment. A bounded temporary intermediate is
not a successful transition; if the second stage refuses, retain the original
configuration. Prepay logical work traversals, expression/value copies,
ledger/event capacity and reconstruction before their corresponding growth.
Do not construct an oversized candidate and then measure it.

Measured resource refusals are separate from runtime invariant failures and
from language semantics; this fragment has no abort/effects/arithmetic. A
refusal leaves the expression, ledger and successful action prefix unchanged.
No cleanup consumes are appended. A smaller independent dynamic value cap must
be checked against every closed value island stored in the complete term,
including unselected branches. This is conservative memory admission, not
evaluation of those branches. Initial island admission precedes terminal/fuel
checks. A transition checks newly closed islands in its prospective substituted
term before materializing or committing them. No late completion guard or extra
pair transition enforces this cap.

The lower-value-cap control after a genuine mint must use pair operands that are
variables and become closed through later copy-value substitution. A closed
literal pair already stored in the initial term is subject to initial admission
and cannot supply that later-failure evidence. Every refusal identifies an
actually exceeded configured bound, including when a zero cap deliberately
refuses the first resource operation. No arbitrary resource-refusal fallback
may hide an executable transition. The exposed guard metadata and helper names
are listed below.

## Proof interface and release gates

Evaluator definitions needed by the proof module remain publicly unfoldable;
the evaluator must not import the downstream proof module. No named helper
theorem is hidden from the complete audit catalog. The planned bridge requires:

- Closed runtime values erase to `Full.IsVal` values, and runtime substitution
  erases to `Full.subst` of that closed value.
- The finite ledger embeds as `Full.RCtx`: live entries map to linear and dead
  or unallocated entries to none. Its retained length supplies the next fresh
  serial; mint and consume agree with Full store push/update behavior.
- Every actual successful step erases to one `Full.Step`, and the entire
  successful prefix erases to `Full.Steps`.
- Completed values erase to `Full.IsVal`.
- Checked reachable execution cannot choose an invariant failure: it produces
  a value, a genuine transition, fuel exhaustion or a specifically measured
  resource refusal. Success soundness alone would allow an always-refusing
  implementation and does not satisfy this gate.
- Progress under a let initializer uses typing for its resource subcontext plus
  inclusion in the actual whole ledger. Its frame may own additional live
  resources, so induction must not assume the initializer alone is typed by
  the entire ledger. Source-site metadata is validated before execution and
  preserved through substitution; erased Full typing alone cannot establish
  additional runtime site or cache checks.
- Preservation/progress classification follows actual checked-program typing
  and the bridge. Copy-result exhaustion/exact-once consequences are conditional
  on the existing Full theorem's hypotheses, not asserted for returned owners.

Only named theorem roots are submitted to the existing theorem-only auditor.
Computable definitions enter dependency closure through the actual soundness
and execution theorems; that association alone is not a semantic property.

These establish a finite logical model only. They do not bind a Vesper source
adapter, native/runtime observation stream, compiler refinement, general source
decidability, full resource campaign or self-hosting claim.

## Current evidence

Read actual `MetatheoryFull.Expr`, `subst`, `IsVal`, ANF typing rules, `EC`,
`Head`, `Step`, `Steps`, finite-store freshness and terminal exhaustion
definitions, together with the historical resource-model plan and complete
frozen contract. The syntax, checker and proof planners independently agree on
the decorated substitution route and right-then-left unpack order.

The actual first module compiled without diagnostics using:

```sh
LEAN_PATH=/tmp/resource-model42-checker-api-v1 /home/agent/.local/share/vesper-lean/bin/lean -o /tmp/resource-model42-evaluator-api-v1/ResourceModel42/Evaluator.olean lean/ResourceModel42/Evaluator.lean
```

Log: `/tmp/resource-model42-evaluator-v1.log`. The first compile identified two
unavailable `Inhabited` instances for unchecked array indexing; actual input
validation now uses a total list shape traversal rather than default indexing.
The second compile passed. `git diff --check` passed. No semantic execution,
axiom audit, transitive replay, checked soundness or no-invariant theorem is
claimed from compiling these definitions.

## First coherent evaluator API

- `LedgerEntry { mintSite, live }`, `Config { expr, ledger }`, `Config.live` and
  `Config.erase` expose the finite tombstone embedding for proofs.
- `ResourceEvent { action, site, id }` and `PlannedAction` record the actual
  dynamic mint serial or consumed serial. `PlannedAction.apply` retains all
  ledger slots, pushes on mint and marks a consumed entry dead.
- Public `locate`, `prepare`, `Replacement.materialize`,
  `Replacement.preflight`, and `PlannedAction.preflight` expose the actual
  focus/planning/checking paths. Substitution preflights both intermediate and
  final whole-context states before either is materialized.
- `step limits eventCount config` is an unfoldable proof helper returning
  terminal, next, measured capacity/resource/event refusal, or invariant error.
  Resource/event refusals retain their actual requested and configured counts;
  capacity tags come from the actual syntax preflight helpers, whose metric
  correspondence remains a proof obligation.
- `validateInvocation checkedProgram values` is the sole constructor for the
  opaque `Invocation`. It measures initial values/closure, checks copy-only
  types, recursive shape and every natural's `[0, 2^64)` range. Its readonly
  fields retain an actual admission-success equality for proofs.
- `evalSteps invocation fuel` refuses fuel above the configured cap before
  starting. `runMachine` is the public unfoldable internal recursion used by
  proofs; the admitted entry takes only the checked invocation wrapper.
- `EvalResult` retains status, unchanged failure configuration, reverse event
  and full-action prefixes. `events`/`actions` expose chronological lists and
  `residual` exposes actual live serials and original mint sites. Completed may
  return an owned handle; no empty-ledger guard exists.

## Evaluator proof checkpoint

The separately authorized `EvaluatorProofs.lean` imports the actual compiled
erasure (`012a4c7`) and ledger (`6ae4905`) checkpoints without a reverse evaluator
dependency. Its first seven public theorems compile with real Lean 4.24:
`locate_plug`, `prepare_sound`, `step_sound`, `terminal_isVal`,
`runMachine_completed_isVal`, `runMachine_prefix`, and `runMachine_steps`.
These prove actual context decomposition, each successful head/step, terminal
value soundness and the actual complete successful prefix through Full.Steps.
The log is `/tmp/resource-model42-evaluator-proofs-v1.log`; the combined private
artifact root is `/tmp/resource-model42-evaluator-proofs-v1`.

The next actual compiled checkpoint extends the module to 21 public theorems.
Log: `/tmp/resource-model42-evaluator-proofs-v2.log`, with no diagnostics. Its
consistent source/olean dependency snapshot is read-only at
`/tmp/resource-model42-evaluator-proofs-v2`; the earlier v1 output path is retired.
The complete current theorem catalog is:

- `locate_plug`, `prepare_sound`, `step_sound`, `terminal_isVal`,
  `runMachine_completed_isVal`, `runMachine_prefix`, `runMachine_steps`;
- `terminal_value_erase`, `completed_value_erase`, `planned_event_projection`,
  `step_event_projection`, `runMachine_event_projection`,
  `runMachine_trace_actions`;
- `typed_atom_value`, `typed_atom_pair`, `typed_atom_nat`, `typed_atom_handle`,
  `locate_prepared`, `step_noInvariant`, `typed_step_classification`,
  `runMachine_noInvariant`.

Returned-value erasure now identifies the actual completed value with the final
term. Event projection relates the actual chronological public events and their
IDs/actions to the successful Full action prefix, rather than assuming a trace.
The generalized typed progress argument uses resource-subcontext inclusion in
the actual ledger, Full canonical-value lemmas, and genuine handle ownership.
The whole-run no-invariant corollary uses actual Full preservation across actual
successful transitions.

The final evaluator proof checkpoint imports the independently compiled metrics
module through `defc692`. It compiles 29 public theorems with no diagnostics
(`/tmp/resource-model42-evaluator-proofs-v4.log`). The fresh read-only complete
source/olean snapshot is `/tmp/resource-model42-evaluator-proofs-v4`.
Its eight additional public theorem roots are `plug_exceeds`,
`replacement_preflight_error`, `action_preflight_error`,
`locate_error_exceeds`, `step_refusal_witness`, `runMachine_refusal_witness`,
`prepare_event_site` and `step_event_site`.

`ReplacementExceeds` independently measures actual incoming closed values,
whole proposed terms and whole intermediate unpack terms. It is not defined
by an algorithm error tag. The actual focus and prepare equalities bind those
values/terms to the actual current execution. Context-depth refusal proves a
real current-term depth excess. Resource/event refusal independently requires
the prepared action to need that space: only mint requests resource space,
and only mint/consume request event space. Requested counts equal actual ledger
history or event count plus one, and the actual configured cap is smaller.
Silent steps cannot justify a resource/event refusal merely by reaching a cap.
`runMachine_refusal_witness` transports this evidence to the actual retained
final configuration and prefix after a resource-limit result.

An independent source reviewer caught the counts-only version of that final
predicate; the action-specific predicate and actual locate/prepare witness
replace it. The corrected statement was re-reviewed with no remaining blocker.
All 29 theorems compile after the strengthening. `step_event_site` separately
identifies each actual event's focused mint/consume operator site and actual
serial, which Full erasure intentionally omits.

Initial checked reachability and copy-only exhaustion consequences are handled
by the separately compiled main proof checkpoint `64e4394`, not assumed by this
lane's raw proof helpers. Final combined acceptance and the complete transitive
audit remain coordinator gates. This lane has not executed Cargo, submitted
audit requests or claimed a full source/backend resource oracle.
