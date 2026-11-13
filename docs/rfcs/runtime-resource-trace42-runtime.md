# Reference runtime resource provider (#317)

This report covers the runtime/compatibility lane under the frozen
runtime-resource-trace42 contract. Source was authored against main1d63dd0.
No native provider, resource oracle, Lean resource model, campaign, exhaustion
proof or full §42 completion is claimed.

The runtime opt-in attaches the core engine only after validating its selected
declarations against the actual Runtime World, and rejects reconfiguration.
Opaque identities live in actual Data carriers. Actual constructor execution
mints with its expression/IR span; Clone, slots, wrappers, projection and
qualify_value retain the same identity. Host-forged callable constructors cannot
mint with a fabricated source site.

Owning pattern bindings stage the complete pure match before collecting atomic
terminal patterns. Their actual carriers and exact Pattern.spans are passed to
the core's atomic batch validation/commit. IR match probes and borrowed pattern
bindings stay pure; sink names and structural unpacking transfer identities.
A handler operation clause stages all owned and borrowed parameter matches as
one transaction and commits one batch across its owned patterns, so a later
mismatch or bound failure cannot leave earlier terminal consumes committed.
Selected constructor patterns additionally require the resolved nominal type to
match the actual carrier, preserving legitimate unselected constructor-name
collisions. Bounded collection does not inspect stale internal environments as
final owners.

Every public ordinary run starts a fresh epoch and validates actual entry and
returned values. The core first checks actual carrier identities; a bounded
value-guided runtime walk then checks shapes against the monomorphic entry's
actual parameter types and checked result type. The closed boundary profile
admits scalars/unit, tuples and nongeneric ADTs; unknown/open/callable/generic
boundary schemas explicitly fail UnsupportedValue. Internal generic helpers are
unchanged. The visitor borrows real Type/field references rather than cloning or
substituting types; a schema encountered only at the result can refuse after
execution while retaining the genuine event prefix. Returned live ownership remains a reported residual, not a
fabricated leak verdict. Completion distinguishes runtime and actual fuel-guard
errors; trace failures retain their typed prefix. Traced external execution is
explicitly refused while the surviving observer retains that refusal.

Narrow Data compatibility adds None at ordinary layout/I/O/simulation decoding
boundaries. The production sim-session JSON encoder and the existing private
native-suspension fixture encoder refuse tracked carriers instead of erasing
their identity. No Layout encoder was added. A real parse/check/observe/run
simulation regression obtains a tracked Ticket inside a wrapper and requires
the actual JSON encoder to reject it; its unobserved counterpart remains valid.

Authored checks: rustfmt and git diff --check. The first combined runtime/core --lib compilation/test command is currently
running under the coordinator's exclusive shared-cache lease. Its result, the
new regression and legacy evaluator/I/O/layout/simulation/source-native
suspension checks have not yet been recorded at this checkpoint. These are source claims only until
the coordinator records actual execution evidence.
