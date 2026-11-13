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
execution while retaining the genuine event prefix. Returned live ownership
remains a reported residual, not a fabricated leak verdict. Completion distinguishes runtime and actual fuel-guard
errors; trace failures retain their typed prefix. Traced external execution is
explicitly refused while the surviving observer retains that refusal.

Narrow Data compatibility adds None at ordinary layout/I/O/simulation decoding
boundaries. The production sim-session JSON encoder and the existing private
native-suspension fixture encoder refuse tracked carriers instead of erasing
their identity. No Layout encoder was added. A real parse/check/observe/run
simulation regression obtains a tracked Ticket inside a wrapper and requires
the actual JSON encoder to reject it; its unobserved counterpart remains valid.

Actual checks executed by this runtime lane with the coordinator's exclusive
shared-cache Cargo lease (jobs1, incremental off, debug information disabled):

- `cargo test --locked -p vesper --lib`: 91 passed, including all eight new
  tracing-core units and the existing I/O session tests.
- Focused legacy root targets: backends68, effect_optimization5, numeric21 and
  source_native_suspension10: 104 passed.
- `cargo test --locked -p vesper-sim-session`: 65 passed (library18, CLI2,
  models12, simulation16, Warp12, Warp ACK5), including the real tracked JSON
  refusal and its ordinary unobserved counterpart.

No failures or ignored tests occurred. Logs are
`/tmp/runtime-resource42-runtime-{lib,compat,sim}.log`. These executed binaries
were compiled from the first coherent runtime checkpoint9e97bf9 with core
5f91722+d60471d+e8231fe. Followup12ff98b prepays the complete scheduled child
count and next depth before any pattern/typed-boundary traversal stack growth.
Its final combined --lib and independently owned acceptance rerun is handed to
the independent acceptance lane after explicit release of this Cargo lease;
those final-head results are not claimed here yet. Rustfmt and git diff --check
also passed. The existing native-suspension fixture encoder's rejection is a
source-level assertion; the production JSON rejection above is actual runtime
evidence. No native resource observer or complete resource oracle is implied.
