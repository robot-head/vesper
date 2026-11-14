# Actual reference resource observation (#317)

The reference evaluator now has opt-in observations for explicitly selected
nongeneric empty nominal-linear structs. It assigns opaque identity at actual
constructor execution, carries that identity inside the actual Value::Data
through transfers and Clone, and records terminal owning constructor-pattern
destruction. This is the reference-provider prerequisite identified by #309,
not a native resource provider or the complete §42 resource oracle.

The baseline is main `1d63dd0e84fa77e651fc0f9c66cc7df65c8c5dd4`. The current
42-section document snapshot and actual metadata are recorded in
[the frozen contracts](runtime-resource-trace42-contracts.md). Current public
entry points are Runtime::observe_resources and the typed, read-only observer
snapshot API in runtime::resource_trace. Default and hard limits, boundary
restrictions, error ordering and exact report fields are part of that contract.

## Actual behavior and review

Identities have checked instance, run and serial association. Complete lifetime
history remains bounded by total mints, including consumed identities, until
the next run. No address or guest field authorizes identity, and no trace is
reconstructed from expected source behavior. Completed returned ownership
remains a visible residual; completion does not assert exhaustion. Fuel/runtime
errors and capacity refusals retain their actual prefix and residuals without
invented cleanup consumes. The approved field restriction is unchanged.

Independent source review found and resolved four integration hazards: attaching
a configuration to a different World requires actual declaration revalidation;
pattern probes and borrowed bindings cannot consume; an operation clause binds
all its parameters as one terminal-consume transaction; and nominal pattern and
typed boundary checks prevent an untracked Other header from masquerading as a
selected Ticket. Exact Pattern spans supply consume provenance, including match
arms whose IR instruction spans refer to a different expression. Traversal
prepayment accounts for visited nodes, pending work and all children before
growing buffers. Read-only snapshots survive errors and abandoned execution.

The observation engine is privately boxed to avoid enlarging the existing
Execution enum beyond the strict Clippy limit. No public Execution boxing or
lint suppression was introduced. Ordinary decoding creates no observer identity;
the actual simulation JSON encoder refuses tracked carriers instead of erasing
them. The existing native-suspension test encoder has the same explicit refusal;
it is a private fixture encoder, not a new production native observation API.

## Validation record

Separate authors implemented the core, evaluator integration, public acceptance
and private carrier-fault controls. A separate reviewer inspected the final
source and test seams without claiming to have executed them.

Before the private storage fix, the final independent command passed all 95
library tests and 17 public acceptance tests, with zero failed, ignored or
filtered cases. The library includes eight core tests and four private tests
executing twenty valid-IR handoffs across both runtime optimizer settings. Those
controls run actual checked constructors, mutate only real returned Data fields
inside one epoch after genuine compilation/optimization, then execute an
unchanged compiled consuming helper. Sequential aliasing, atomic duplicates and
missing/wrong/malformed carriers are detected with exact prefixes, identities,
residuals and source sites. No identity, ledger, program or event list is forged.

The initial public acceptance attempt passed 13 cases and failed four assertions
that expected observer execution after deliberately corrupting a checked World.
Actual diagnostic replay showed that the optimized pipeline correctly rejects
those malformed Worlds with E0410 before execution. Tests now require that exact
static rejection with zero events and residuals; it is not counted as observer
detection. Unoptimized actual-carrier assertions remain intact, and the private
post-compilation controls separately exercise dynamic detection with optimization
enabled. The public suite also runs a small genuine AST optimizer × IR optimizer
matrix, bounded fuel sweeps, whole-clause failure/capacity transactions, epoch and
cross-World refusal cases. These are bounded tests, not a 100000-program campaign.

The earlier integration source also passed 104 focused backend, numeric, effect
optimization and source-native suspension regressions, plus all 65 simulation
package tests, including the genuine nested tracked-value JSON refusal. The
first strict Clippy attempt found only the Execution size regression described
above. The final boxed production source then passed `cargo test -p vesper
--locked`: 1069 test executions across 90 result records, zero failures, ignored
tests or filters. This includes both genuine external-model replay and the real
arithmetic oracle matrix, plus all six proof-audit CLI tests.

A subsequent test-only swap control conserves both actual resources while
reversing their physical field routing. Its two consume events must carry the
opposite mint identities at the unchanged left/right pattern sites; counts alone
would miss this difference. All five private tests, covering twenty-four real
handoffs, passed on that source (zero failed/ignored, 91 unrelated library tests
deliberately filtered). The earlier full package result is not relabeled as an
unfiltered run of that newly added test.

`cargo clippy -p vesper -p vesper-sim-session --locked --all-targets -- -D warnings`
passed in 37.62 seconds on the final source. Workspace formatting, actual CLI
format checking of all six new Vesper fixtures, Bazel metadata synchronization
and whitespace checks also passed. Logs are
`/tmp/runtime-resource42-root-full.log`,
`/tmp/runtime-resource42-final-carriers.log`, and
`/tmp/runtime-resource42-final-clippy.log` in the execution workspace. These local
Linux results do not claim hosted macOS/Windows execution.

The executed and coordinator worktrees have byte-identical contents for all
fifteen changed Rust/fixture files. Their actual SHA-256 associations, source
heads and separate validation checkpoints are recorded in
runtime-resource-trace42-source-manifest.json. This is an
input association, not proof of binary derivation or a hostile-host attestation.

## Remaining boundaries

Public observed entries are monomorphic and support scalar/unit/tuple/nongeneric
ADT boundary values. Generic/callable/open boundary schemas and traced external
sessions explicitly refuse; internal generic helpers are unchanged. Logical
limits apply to the observation provider, not all evaluator/source/process
allocations. Structural wrappers' independent nominal obligations are not
selected automatically. No native identity layout/observer, Lean ResourceOracle,
complete static diagnostic correspondence, generator/shrinker campaign,
compiler refinement, self-hosting or full design completion is claimed here.
