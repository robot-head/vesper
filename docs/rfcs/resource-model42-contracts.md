# Finite executable resource model (#322)

Frozen model-only scope at main
`30de7fef8be040a890e34f26370ee15b1429642e`. The 42-section specification snapshot is
`8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d`;
Google modification metadata remains 2026-11-13T04:46:25.948Z. #308 retains
native integration, #319 retains observer source, and #320/#321 retain existing
CI/BUILD changes. No existing compiler, runtime, model or Cargo file is edited.

## Admitted calculus and authoritative data

Use the pure ANF subfragment of Vesper.MetatheoryFull: unit, bounded u64-like
naturals, de Bruijn variables, strict lexical let, binary structural pairs and
full unpack, numeric-zero branches, one atomic linear ticket kind, mint and
terminal consume. Computation occurs in let initializers/bodies; pair, unpack,
ifz and consume operands are atoms. There are no raw source handle literals.
Source atoms are unit/nat/variable; computed pairs are source expressions, not
atoms. Runtime atoms additionally admit closed pair/handle values after
substitution. Pair-as-value normalization erases identically; it is not an
invented operational step. Unpack binds left at index 1 and right at index 0;
reduction substitutes right at zero, then left at zero.

Model types include unit, nat, ticket and modal binary pairs. Pair floors may
be copy or affine; nominal-linear aggregate-root identities are deferred.
The approved component premise `k_i ⊑ m_i` remains mandatory. Binder modes are
inferred type kinds, never caller-supplied overrides. One ticket kind maps
exactly to Full.hdl; no claim about multiple nominal kinds is made by erasure.
Static literal inference rejects naturals outside `[0, 2^64)`; actual invocation
values are separately checked and never become static facts.

Source sites are bounded, unique logical occurrence labels, distinct from
static variable indices and dynamic resource identities. Event operations keep
their original labels through substitution. Every source site is strictly
less than the configured raw-occurrence cap; labels are unique across the
whole raw source AST. An expression wrapping one atom has no additional site.
Runtime substitution may repeat nonoperation origins without revalidating
source-label uniqueness. These model labels are not yet a
binding to parsed Vesper byte spans or an observed backend stream. No expected
answer, expected trace, acceptance flag, inferred type or ownership mask is
accepted as authoritative request input.

Initial declared inputs are copy-only and validated separately from invocation
values. Checked root results may have any admitted kind: returning an owned
ticket is legal and retains its live residual. The later differential harness
may restrict entries to copy results. There is no runtime posthoc empty-ledger
guard masquerading as an exhaustion proof.

General arithmetic, four integer kinds, booleans, field projections/loans,
enums, generics, arbitrary functions, effects, closures, continuations,
physical resources and source adaptation remain outside this first fragment.
Full already includes the merged safe literal-arithmetic rule from #314; the
older planning statement that all arithmetic requires abort is historical.

## Computable checking and proof interface

Syntax owns RawProgram, source/runtime syntax, type metrics, source validation
and an opaque ValidatedProgram with readonly data and validation evidence.
Checker owns an opaque CheckedProgram created only by actual inference success.
Its readonly accessors expose the validated program, inferred result and a
genuine inference-success equality for the proof module. Evaluator owns any
opaque validated Invocation. Cross-module constructors are not accidentally
public smart constructors that accept unchecked success booleans.

The checker computes finite local types, bounded expanded type metrics and
usage masks. Copy reuse is allowed; sequential noncopy demands are disjoint.
Let/unpack enforce use of every owned linear binder. Both arms are checked,
their result types agree and their linear demands agree. This first checker
also requires equal affine demand: affine-only disagreement is an explicit
profile refusal, not a semantic ownership error. Check for any linear-demand
disagreement before classifying affine-only differences, regardless of binding
order. Input values never affect it.
Semantic errors, profile/admission refusals, invocation errors, runtime
invariants, fuel and measured resource limits have distinct categories.
Only a first structured semantic error is promised; full ordered Rust
diagnostic correspondence remains separate work.

The soundness theorem uses a strong ContextFits relation: every demanded
binding is present at its actual type kind; unused linear entries are absent;
unused copy/affine extras may remain. This accounts for unused affine binders
in Full let rules without an unsound minimal-mask assumption. The required
accepted-checking theorem constructs actual Full.HasType evidence, with empty
resource context and row, for any irrelevant effect signature. No caller
typing premise substitutes for the checker's own result.

## Bounded operational execution

Use decorated substitution and a finite tombstone ledger. Each successful
transition must erase to one actual Full.Head/Full.Step, including strict let
contexts; this is not a CEK stuttering claim. Runtime values have no synthetic
input source sites. Copy invocation values close the source using pure nested
lets, with input 0 innermost; their actual silent transitions count as fuel.

Resource identity is a fresh dynamic serial from total mint history, not a
slot, occurrence label or live-count reuse. Consume requires the actual live
identity, marks its retained entry dead, and records one ordered event. Moves,
packing/unpacking and ordinary binding produce no terminal event. Public trace
filters silent steps, while the proof relates the complete successful prefix
to Full.Steps. Completed returns the actual value and residual; fuel, invariant
or capacity failure retains the actual prior prefix without cleanup events.

Terminal detection precedes the fuel-zero test on an admitted state. Each
successful Full transition costs exactly one global fuel unit. No per-subtree
fuel resets or eager execution of unselected branches is allowed. Prepay every
transition's ledger/event space and full proposed expression/value growth,
including both branches, enclosing let contexts and intermediate/final unpack
substitutions, before constructing copies or committing any change. Refusal
must leave the current term, ledger and event prefix unchanged.

Admission measures all closed value islands already stored in a term,
including unselected branches. This conservative memory check does not execute
those branches. Initial admission precedes terminal/fuel checks; a transition
prepays newly closed value islands before substitution constructs or commits
them. A pair of already closed atoms is already a Full value, so no invented
pair transition or late completion guard is used to enforce a value cap. A
dynamic value-limit test after a mint must instead use variable operands that
become a closed pair through a later actual substitution. Input-closing let
wrappers count toward whole-expression nodes and depth.

Starting limits (default / hard maximum) are raw occurrences 256/1024, lexical
context 64/256, raw depth 64/128, expanded type and per-value nodes 255/1023,
type/value depth 16/32, aggregate initial copy-value nodes 4096/16384, initial
inputs 16/64, whole runtime expression nodes 65536/1048576, runtime depth
256/512, total resources including tombstones 64/256, and events 128/512.
Default fuel is 4096; hard fuel is 65536. Saturating/prepaid metrics and bounded
work traversal must reject expansion before allocation, including compact
copy-pair doubling. A lower independent dynamic value cap can refuse after an
earlier genuine mint, preserving that live prefix. These are logical model
limits, not claims about all Lean/compiler/process allocation or throughput.
Syntax publishes exact names and coherent definitions in its first source
checkpoint before dependent writers start; changes need coordinator review.
Zero caps retain their ordinary numeric meaning: a zero raw-occurrence cap
admits no expression, zero inputs admits an input-free program, and zero
resource/event space can admit a program whose first operation later refuses.
Zero fuel still completes an already admitted value. Every measured refusal
must identify an actually exceeded configured bound; merely avoiding invariant
failure is not permission to refuse an otherwise executable step.

## Required checked properties and acceptance

Proofs must establish source/runtime erasure, closed-value IsVal and
substitution commutation, finite-context checking soundness, validated input
closure, ledger push/update/freshness, successful-step soundness, completed
value soundness and Full.Steps prefix correspondence. Checked reachable states
must not produce runtime invariant failure: only values, actual transitions or
explicit measured resource refusals are allowed. Mere success-soundness would
permit an always-refusing evaluator and is insufficient. Derive preservation,
progress/refusal classification, and completed-copy exhaustion/exact-once
properties from the existing Full theorems and actual step correspondence.
No empty-ledger runtime guard may replace those proofs. This remains a
restricted model, not proof of source/Rust compiler refinement or full
decidability/completeness for the complete calculus.

All new named theorems are public and audited; the exact catalog follows the
implemented source, with no omitted private helper theorem. Each actual audit
request has at most 128 roots; partition a larger catalog explicitly and check
the complete union. The only allowed transitive axioms are the standard
`Classical.choice`, `Quot.sound`, `propext` (an allowlist, not required axioms).
The existing auditor accepts theorem roots only. Computable definitions enter
its real dependency closure through the corresponding soundness and execution
theorems; definition names must not be submitted as theorem roots. Dependency
association alone is not evidence of a semantic property.
No sorry/admit/project axioms, unsafe/partial definitions or native-decide escape
is permitted in the model. Fixed handwritten semantic tests and privacy
controls are allowed. Any generated Lean, including a genuine policy-negative
control, uses vesper-lean-syntax AST and one render, never text templates.

Independent acceptance compiles fresh fixed modules with real Lean 4.24,
checks all meaningful positive/negative semantic assertions and actual
transitive kernel replay/axiom policy. Missing/wrong Lean, missing/corrupt
artifacts or a forbidden policy axiom cannot silently skip into success.
Measure the new Cargo target before proposing any existing BUILD/CI timeout
change; external #320/#321 ownership is respected. No full-harness generator,
external Lean4Lean model-profile coverage, native observation, diagnostic
agreement, 100000-program resource campaign or self-hosting is claimed.

## Exclusive writers

- Syntax: NEW lean/ResourceModel42/Syntax.lean and syntax report.
- Checker: NEW lean/ResourceModel42/Checker.lean and checker report.
- Evaluator: NEW lean/ResourceModel42/Evaluator.lean and evaluator report.
- Ledger proof assistance: NEW lean/ResourceModel42/LedgerProofs.lean and
  ledger report; imports Evaluator, never the reverse.
- Proofs: NEW lean/ResourceModel42/Proofs.lean and proof report.
- Independent acceptance: NEW lean/ResourceModel42/Acceptance.lean,
  tests/resource_model42.rs and acceptance report.
- Coordinator: NEW lean/ResourceModel42.lean, this contract and final report.

Only Syntax starts source implementation before its first coherent API
checkpoint. Remaining writers import that checkpoint before coding. Lean
artifacts go to private temporary roots, with immutable dependency checkpoints
and explicit leases; Cargo uses the established exclusive shared build lease.
