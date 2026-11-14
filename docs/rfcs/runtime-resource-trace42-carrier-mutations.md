# Private actual-carrier fault controls (#317)

Final coordinator execution passed all five private tests and 24 handoffs on dd364f6 (0 failed/ignored, 91 other library tests deliberately filtered). The production-identical preceding full package run passed 1069 test executions with the earlier four-test private module. Final strict all-target Clippy passed. The chronology below distinguishes those checkpoints; see [integration evidence](runtime-resource-trace42-integration.md).

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
swap its actual left and right carriers, clone its actual left carrier into its
right field, remove the right identity,
change its right nominal type while retaining its matching constructor name, or
add an unexpected right carrier field. The unchanged compiled sink helper then
executes in the same epoch through `run_execution`.

This is a deliberately private, controlled internal-handoff fault. It does not
claim malformed public entry values are accepted, the source checker permits
duplication, or arbitrary compiler/runtime behavior refines a resource model.
Public `run` would correctly start a fresh epoch; using it for the second half
would test previous-run admission instead of this in-run carrier fault.

## Independent expectations

Five tests cover twenty-four source-checked handoff executions across
optimization off and on:

- Unchanged sequential and batch values consume the actual two minted identities
  in source-pattern order, with exact atomic constructor `Pattern.span` ranges
  and no residuals.
- Swapping the actual left and right carriers conserves both resources and
  successfully consumes them. Both sinks must record the second minted identity
  at the unchanged left pattern and the first minted identity at the unchanged
  right pattern. The two mint/two consume counts and empty residual alone could
  not distinguish this permutation from unchanged routing; checking the actual
  identity/site association prevents reconstructing consumption from expected
  constructor or source-field order.
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

Source authored and formatted; `git diff --check` passed. This lane performed no
Cargo execution. The coordinator executed the actual combined implementation at
`9ccb1da5ae8f7720d52ce89c08ccd8648be3f376`; the full log is
`/tmp/runtime-resource42-independent-final.log`. Its library target passed
95 tests in 12.95 seconds and its public `runtime_resource_trace` integration
target passed 17 tests in 0.63 seconds, with zero failures, ignored tests or
filters. The build took 42.57 seconds.

The original four private `runtime::resource_trace_acceptance` tests appear as
successful executed tests in that log, covering the original twenty real handoffs:

- `valid_private_handoffs_consume_the_two_actual_identities_in_both_ir_modes`
- `duplicate_actual_carrier_fails_sequentially_after_one_consume_in_both_ir_modes`
- `duplicate_actual_carrier_refuses_the_entire_batch_in_both_ir_modes`
- `actual_missing_and_wrong_headers_keep_precise_transaction_prefixes_in_both_ir_modes`

This provides actual post-IR carrier-fault evidence in both modes: the valid
program was checked, optimized when configured, executed to mint real carriers,
then faulted at the private value handoff. It is distinct from the public
integration tests' optimized-IR rejection of mutated checked Worlds, which does
not execute those dynamic mutants. Source review by the independent runtime
reviewer found no actionable issue in this lane.

The subsequent focused strict Clippy invocation failed on the existing public
`Execution` enum's size after adding the inline observation engine. The runtime
writer is changing private engine storage rather than allowing the lint. Final
Clippy and regression validation after that storage change remain pending; this
report does not claim those later checks passed.

The fifth test,
`swapped_actual_carriers_reverse_consume_identities_at_unchanged_sites_in_both_ir_modes`,
adds four handoffs after the recorded run. Its source is formatted and diff
checked, but its execution is pending the coordinator's combined-source build
lease. The original four tests' assertions remain unchanged. Full-suite evidence
against the earlier four-test source must not be reported as execution of this
new swap control.
