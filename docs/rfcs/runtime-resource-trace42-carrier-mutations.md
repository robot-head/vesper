# Private actual-carrier fault controls (#317)

This independent lane owns only
`src/runtime/resource_trace_acceptance.rs` and this report. The runtime writer
owns the private test-module registration. No production observation hook,
unchecked World, fabricated identity, AST/IR mutation, event edit or ledger edit
is introduced by these tests.

## Controlled handoff

Every case parses and checks a genuine program with two empty linear `Ticket`
constructors inside a structural `Pair`, plus valid sequential and atomic batch
sink helpers. It validates the actual lowered program before execution, attaches
the ordinary resource observer and begins exactly one engine epoch. The private
`Runtime::run_execution("make", ...)` path performs ordinary configured IR
optimization, invocation and evaluation. With optimization enabled, successful
execution and the actual `optimized` flag establish that ordinary optimization
and its ownership validation completed before the fault. Neither mode disables
or replaces any optimizer pass. The program is validated again after `make`.

The tests inspect the actual returned carrier and require two distinct opaque
identities corresponding to two genuine `Mint` events and the exact constructor
expression source spans. Only then do they change the actual returned value:
clone its actual left carrier into its right field, remove the right identity,
change its right nominal type while retaining its matching constructor name, or
add an unexpected right carrier field. The unchanged compiled sink helper then
executes in the same epoch through `run_execution`.

This is a deliberately private, controlled internal-handoff fault. It does not
claim malformed public entry values are accepted, the source checker permits
duplication, or arbitrary compiler/runtime behavior refines a resource model.
Public `run` would correctly start a fresh epoch; using it for the second half
would test previous-run admission instead of this in-run carrier fault.

## Independent expectations

Four tests cover twenty source-checked handoff executions across optimization
off and on:

- Unchanged sequential and batch values consume the actual two minted identities
  in source-pattern order, with exact atomic constructor `Pattern.span` ranges
  and no residuals.
- A duplicated right carrier passed to sequential consumes commits the first
  real consume, then produces `AlreadyConsumed` at the right pattern. The second
  genuinely minted identity remains live with its original mint site.
- The same duplicate in one nested `Pair` pattern produces `DuplicateIdentity`
  at the right atomic pattern, commits zero consumes and retains both original
  identities.
- Missing identity, wrong nominal type and nonempty selected carriers produce
  `MissingIdentity`, `WrongKind` and `MalformedCarrier`, respectively. Sequential
  execution retains one consume; atomic execution retains zero. Both check the
  exact right failure site, retained identity/kind/mint-site association and
  sticky `TraceFailed` status after finalization.

A snapshot copied before the fault must still contain its original two mint
events and two live identities afterward. Expected source spans are located in
the independent fixed source, not copied from events or substituted instruction
spans.

## Validation status

Source authored and formatted; `git diff --check` passed. No Cargo build or test
has been executed by this lane: the coordinator has not yet granted the shared
build lease. Runtime evidence remains pending and will be recorded only after
the actual combined source compiles and executes. Existing public integration
tests separately distinguish optimized IR rejection of mutated checked Worlds
from these post-validation dynamic carrier controls.
