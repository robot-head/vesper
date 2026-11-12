# Actual source-checker causes (#88)

The source checker now adds ordered related labels/help to existing ownership,
alias and abort-frame diagnostics. Primary codes, messages, spans and the existing
primary deduplication key remain unchanged. This delivery changes only
`src/check.rs`, `tests/diagnostic_causes.rs` and this report; core diagnostic model
commit `4ec73e3f030371758f99d7d6030afcd0efa6de25` is a separate dependency.

Supported actual causes:

- E0302 unconsumed linear bindings and destructive shadowing identify the actual
  binding declaration. Unrelated unused results/wildcards do not acquire an
  invented previous declaration or consumption.
- E0303 moved-place reuse identifies its actual binding and earlier overlapping
  ownership-taking source events. These are not advertised as a complete history
  or a guessed last use.
- E0301 overlapping actual arguments identify the earlier argument expression;
  active argument-loan conflicts identify the loan's actual originating argument
  and the accessed declaration. Overlapping formal projections within the same
  actual argument instead say that this argument's projection overlaps, avoiding
  a false claim that another argument was evaluated earlier.
- E0305 borrowed non-copy moves, E0306 linear aggregate field moves and E0307
  shared-parameter mutation identify the actual parameter/binding declaration.
- E0309 branch-dependent consumption identifies the declaration and actual
  ownership-taking events from the relevant branch states, excluding common incoming history from branch-specific labels.
- E0402 abortable checkpoints identify actual live linear declarations, active
  inout parameter declarations, originating inout argument expressions, or linear
  handler-state initializer expressions, according to the existing producer.

Help explains existing ownership, alias and resumption invariants. It does not
invent callable abort/commit methods, unchecked rewrites, proof authority or
automatic fixes. Existing E-codes are used as implemented; they are not renumbered
to resemble specification examples.

Diagnostic-only evidence is checker-private. It describes compiler ownership
transfer events, not proof of successful execution of a whole source program. `Binding` stores at most four actual
admissible transfer witnesses, with at most 64 path components and 1024 aggregate path bytes per
stored event. Evidence is retained only when the place's ownership preconditions
hold: no prior overlapping move, valid projections/field, no conflicting active
loan, owned sink access, and no forbidden linear aggregate field move. Existing
error-recovery moved paths remain unchanged, but rejected attempts cannot become
successful-transfer witnesses. Oversized/full histories stay absent. New/shadowing bindings start
with empty histories. Branch propagation requires the same binding declaration
span and does not participate in moved-state comparisons. Loans retain their
actual source argument span, and state guards retain their actual initializer
span. Loan captions describe the argument's requested access, so rejected
shared/inout or explicit-pass requests do not falsely certify a successful loan.
State captions say `linear handler state initializer`, identifying the actual
expression even when ownership checking rejected its initialization.
Nested blocks preserve matching binding evidence through the existing Env.
The existing checker does not reinitialize an already moved place by assignment:
such a write stays rejected and retains the original cause, without inventing a
new initialized state. New bindings/shadowing reset the evidence. No new public AST/type/World/IR/interface/native ABI schema is introduced.

Each emitted diagnostic receives at most eight related labels and one fixed,
short help string. A cap can omit genuine evidence, never replace it with an
invented source location. Existing source/AST and diagnostic-count profiles still
bound overall checker work; this lane adds per-binding/per-diagnostic bounds,
not a hard global RSS/time guarantee. The separate frontend/renderer lanes
validate all spans and aggregate metadata before publishing editor failures.

Ownership transfer, projections, call evaluation order, abort-frame predicates,
branch merging, literal facts, inferred results/effects, IR lowering and inference
convergence remain unchanged. A static comparison confirmed identical argument
lists for all 109 existing `self.error` calls, ignoring formatting whitespace.
That comparison supplements the regression tests; it is not a formal semantics
proof. Diagnostic metadata is outside the convergence state and the unchanged
primary deduplication tuple.

Validation used private `/tmp/vesper-target-tooling41-doctest`, one build job,
dev/test debug disabled and incremental compilation disabled:

- `cargo check -p vesper --lib --offline`: passed.
- `cargo test -p vesper --test diagnostic_causes --offline`: 17 passed.
- `cargo test -p vesper --test language --test syntax_and_ir --offline`:
  107 existing language tests and four syntax/IR tests passed.
- `cargo clippy -p vesper --lib --test diagnostic_causes --offline -- -D warnings`:
  passed.
- Owned Rust files pass rustfmt; `git diff --check` passes.

Tests assert exact primary messages/spans and actual related spans for exhausted
and shadowed bindings, moved field paths and disjoint valid moves, branch joins,
borrowed parameters, argument aliases/loans, live linear/inout abort frames and
handler-state initializers. Unicode/CRLF sources retain real byte offsets. The
shadowing regression excludes outer declarations/consumptions from an inner
binding's causes. Caps and unchanged primary deduplication are checked. Invalid borrowed/loan/
linear-field/repeated attempts do not become transfer witnesses; separate valid
field transfers exercise the actual history cap. Nested-block and rejected
reinitialization cases preserve real origins without changing recovery. Rejected
borrowed/moved state initializers and invalid shared/inout/explicit-pass argument
requests retain neutral source-fact captions, not successful-initialization or
successful-loan claims. A prebranch consumption followed by a rejected branch
re-use retains the unchanged primary E0309 but does not mislabel common incoming
history as a new branch-specific transfer. Existing
language tests also exercise control flow, capability inference and source/PCG
acceptance. Tests use parser/checker and the existing interpreter; no JIT, native
source execution, WASM, macro, proof or ambient URI read is performed.

Missing actual event evidence remains absent. PoisonedRecovery, per-function PCG,
proof lenses, automatic substructural edits and latency targets remain outside
this implementation. No new Lean theorem or axiom is introduced.
