# Reference resource observation provider (#317)

Frozen against main `1d63dd0e84fa77e651fc0f9c66cc7df65c8c5dd4` and the
42-section specification snapshot
`8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d`.
The document's modification metadata remains 2026-11-13T04:46:25.948Z.

This implements a reference-evaluator provider prerequisite from #309. It does
not authorize the full resource oracle, native observations, generator, Lean
model, campaign or compiler-refinement claim. External #308 owns native/main
integration and #311 owns capability checking; those files remain untouched.

## Profile and ownership

The caller explicitly selects exact, duplicate-free names of nongeneric empty
nominal-linear structs: not enums, one empty variant with the same constructor
name. Structural wrappers and tuples transfer selected children's identities;
their independent nominal obligations are not part of this selected-type trace.
The approved field restriction `k_i ⊑ m_i` is unchanged.

`Value::Data` gets `resource_identity: Option<ResourceIdentity>`. Its opaque
identity travels inside the actual value through Clone, slots, aggregate
packing/projection, parameter/return transfer and `qualify_value`. Guest fields,
addresses, source-derived expected events and detached shadow identities cannot
replace it. Ordinary unobserved constructors use None; display is unchanged.
Actual observed constructor execution mints. A successfully matched, committed
by-value empty constructor pattern terminally consumes. Binding a name, passing
sink parameters, structural wrappers, borrowed bindings and pattern probes do
not consume. No optimizer switch or pass is disabled for observations.

Use each actual constructor expression/IR origin for mint sites and each atomic
Constructor Pattern.span for consume sites. IR match-arm instruction spans may
refer to the initializer or first arm body and are not consume provenance.
Never fabricate a zero span for host-forged callable constructors: refuse an
observed construction without a genuine operation site.

Full pure pattern matching precedes consumption. Collect terminal atoms in
source-pattern depth-first order; validate all identities, batch duplicates and
the complete remaining event capacity before committing any consume in that
pattern. A failed or unselected probe produces no event.

## Frozen typed API

The nested public module is `runtime::resource_trace`, with no new dependencies
or lib registration. `ResourceTraceConfig::new(world, selected: &[&str], limits)`
validates before retaining copied names. ResourceTraceLimits has these public
usize fields, all checked against hard maxima before controlled allocation:

| Field | Default | Hard maximum |
| --- | ---: | ---: |
| max_selected_types | 8 | 64 |
| max_name_bytes (per name) | 256 | 1024 |
| max_selected_name_bytes | 2048 | 16384 |
| max_resources | 256 | 4096 |
| max_events | 512 | 8192 |
| max_value_nodes | 4096 | 65536 |
| max_value_depth | 64 | 256 |

Zero bounds are valid and may deliberately refuse the first operation.
max_resources counts total minted identities, including consumed tombstones,
not simultaneous live resources. Retain the complete lifetime map until the
next epoch; no ABA reuse is allowed. Limits bound the observation provider,
not all evaluator, source, host-input or process allocations.

ResourceIdentity has private instance/run/serial fields, immutable getters and
Copy/Eq/Debug. A noncloneable internal ResourceTraceEngine owns authority and a
cloneable read-only ResourceTraceObserver can snapshot immutable bounded reports.
The instance nonce is globally checked; run epoch and serial increments are
checked. No address is used as identity and exhaustion never wraps.

Engine operations: new(config), observer(), begin_run(),
mint(ty, constructor, field_count, actual_span), consume_batch(actual carriers),
validate_values(actual entry args), finish(completion) and sticky fail(reason,
optional span). ResourceConsume borrows actual type/constructor and contains the
actual carrier identity and pattern site. Its batch is fully prepaid.
Authoritative operations, including finish, return typed errors; run must
propagate a finalization failure rather than report a successful value with an
unfinished observer. Borrow conflicts cannot panic or silently become success.

`Runtime::observe_resources(config)` returns the read-only observer and refuses
reconfiguration of an already observed Runtime. It revalidates the selected
declarations against its actual World before attaching: a config created for
another World does not authorize same-named affine/nonempty/generic/enum types.
Failed attachment leaves the ordinary Runtime unobserved. Config fields remain
private and validate_world reuses the bounded selected-name profile, not a World
clone or an address as authority. run begins a fresh epoch and
validates actual entry values before executing. Bounded iterative traversal
rejects missing, foreign, previous-run, dead, duplicate and wrong-kind tracked
carriers. External callable/closure/continuation boundary inputs may be explicitly
unsupported; no current-run identity can be fabricated for input admission.

Reports expose selected names, instance/run, ordered events with action, kind
index, identity and actual source span, live residual identities with kind/mint
site, status and first failure. Completion distinguishes Completed, RuntimeError,
FuelExhausted and Abandoned; TraceFailed is sticky. First failures retain bounded
typed data, not arbitrary messages. A completed returned owner remains a visible
live residual; Completed does not assert leak freedom or exhaustion. Failed
execution/fuel/capacity retains only actual recorded prefix and residuals, with
no synthetic cleanup consumes. A previously copied snapshot stays immutable;
an observer reads the new epoch after a subsequent run.

The first provider explicitly refuses traced begin_external and records that
refusal for the surviving observer. Unobserved external sessions are unchanged.
Dropping a running engine marks Abandoned without inventing events. Internal
borrow failures must return an error rather than panic.

## Writers and acceptance

Core writer owns NEW src/runtime/resource_trace.rs and core report. Runtime
writer owns src/runtime.rs, narrow carrier compatibility in src/io_session.rs,
src/layout.rs, tests/source_native_suspension.rs, and the two publicly amended
sim-session value.rs/tests/simulation.rs paths. Native-suspension's existing test
encoder and real JSON encoder reject Some identity instead of erasing it across
an uninstrumented boundary; no new Layout encoder is invented.

Independent acceptance owns NEW tests/runtime_resource_trace.rs, matching new
fixtures and acceptance report. It must execute checked source with optimize
off/on, exact source sites, repeated helper sites, shadowing, both branches,
wrappers/tuples/projections and unselected-type isolation. Mutate actual carrier
routing in initially checked World/AST values to detect duplication/wrong-field
consumption, rather than merely mutating event lists. Exercise invalid profiles,
foreign/prior/missing carriers, returns, zero/exact capacity, atomic batch refusal
and partial fuel/error prefixes. Existing interpreter/I/O/layout/simulation and
source-native suspension regressions must remain valid. Shared Cargo builds
require the coordinator's exclusive lease. All reports distinguish actual
executed evidence from remaining native/full-oracle work.
