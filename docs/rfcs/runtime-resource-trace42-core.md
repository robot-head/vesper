# Reference resource observation core (#317)

This coherent source checkpoint implements the core side of frozen contract
`f8e4abb` and its actual-World revalidation/fallible-finalization amendments.
Only new `src/runtime/resource_trace.rs` and this report are owned by this lane.
Runtime registration, actual Value carrier integration and independent source
acceptance are separate writers' work.

The source contains validated private configuration, the exact default/hard
limits, opaque process-local instance/run/serial identities, a noncloneable
engine and a cloneable read-only observer. Configuration copies selected names
only after bounds/duplicates/exact empty linear declaration checks, and exposes
actual-World revalidation for Runtime attachment. Total-mint history includes
consumed tombstones until a checked new epoch. Capacity failures cannot create
phantom successful events or identities.

Mint and terminal batch-consume update the actual event ledger only when called
by executed Runtime operations. Batch shape/identity/duplicate/capacity checks
finish before any release commits. The core walks actual Value carriers
iteratively at owning entry/result boundaries, rejecting identity loss, foreign
or stale epochs, dead/wrong-kind or duplicate carriers and unsupported callable
boundaries. It does not reconstruct observations from syntax or expected data.

Snapshots expose immutable selected names, ordered events and sites, residual
live identities, status and bounded typed first failure. TraceFailed is sticky.
Completed retains returned-owner residuals and is not an exhaustion assertion.
Ordinary error, fuel exhaustion and abandonment are distinct completions, with
no invented cleanup events. `finish` returns Result so an internal borrow
failure cannot become apparent successful execution. Engine destruction uses a
separate alive Cell before attempting the ledger borrow; snapshots can therefore
report Abandoned after a running engine is dropped even if that borrow was
temporarily unavailable. Internal borrow conflicts return typed errors rather
than RefCell panics.

Eight private tests are authored for zero/exact capacities, total-mint
history, atomic consume batches, first-error retention, actual cloned carrier
identity, epochs/foreign/missing/dead/wrong carriers, traversal admission,
configuration revalidation and checked nonce/borrow/drop edges. They are not
yet executed at this checkpoint.

## Executed evidence at checkpoint

```sh
rustfmt --edition 2024 src/runtime/resource_trace.rs
rustfmt --check --edition 2024 src/runtime/resource_trace.rs
git diff --check
```

Formatting and whitespace checks pass. No Cargo/rustc check, unit test or runtime
execution has been run by this lane: the new module depends on the separately
owned Runtime Data field/module declaration, and the coordinator requires a
combined-source build lease first. This is concrete source for integration,
not a success receipt or a placeholder implementation. Actual combined build
and test outcomes must follow before acceptance.

No native provider, complete resource oracle, physical resource accounting,
large differential campaign, whole-process bound, compiler-refinement theorem
or §42 completion is claimed. Controlled bulk vectors/string copies use
fallible reservations; fixed-size Rc control blocks use Rust's normal allocator.
Limits bound provider-owned data and each snapshot, not an unlimited number of
snapshots retained by a caller or the pre-existing evaluator/host value graph.
