# Distinct private names in kernel replay (#380)

These are fixed handwritten, standalone controls for the proof-audit backend.
`PrivateReplayLeft` and `PrivateReplayRight` each declare
`private theorem ProofAuditPrivateReplay.shared : True` in their own module.
Lean assigns different real private full names to those declarations, although
`privateToUserName` gives the same user spelling. Public roots
`ProofAuditPrivateReplay.left` and `ProofAuditPrivateReplay.right` reference
their own respective private theorem. `PrivateReplayBoth` imports both modules.

Both private declarations must remain distinct and must be kernel-checked.
Normalizing them to one user name during replay loses a valid environment
identity; merely checking the public roots' common proposition `True` would
miss that error. No recursion, unsafe/partial declaration, project axiom,
expected-result input, or generated source template is involved.

The independent regression should compile the actual fixed sources, observe
the distinct full names with equal normalized spelling, and audit BOTH public
roots through the real unchanged-policy CLI. Source drafting alone is not
execution evidence; actual compile/metadata results are recorded below.
No corrupted-metadata fixture is claimed.

## Independent fixed metadata probe

`PrivateReplayMetadata.lean` is a fixed handwritten probe. Run it from a
SEPARATE source directory with `LEAN_PATH` pointing to the three-module artifact
root, using `lean --run`, without producing its `.olean` in that root. It is
not part of the audited fixture import closure.

The probe computes a `VESPER_PRIVATE_REPLAY_METADATA_V1:` JSON record with
`schema_version: 1`, `declaration_count`, `modules`, `private_declarations`, and
`public_roots`. Counts, full/normalized names, safety flags and each public
proof's private dependency come from the actual imported ConstantInfo records
and proof bodies. Its checks require distinct private full names, equal user
spelling, and a different corresponding private dependency for each public
root. It does not supply expected answers to the auditor.

## Actual execution evidence

The backend writer, using his existing Lean compiler lease, compiled the
three fixed source modules from `c3a6c1a` and ran the tracked probe from
`9a4c27e` with real pinned Lean 4.24. The fixture author did not run a second
compiler or use a new lease. All three compiles exited 0 with empty logs. The
probe exited 0 with empty stderr and reported:

- Four actual declarations: two from Left, two from Right, none from Both.
- `_private.PrivateReplayLeft.0.ProofAuditPrivateReplay.shared` and
  `_private.PrivateReplayRight.0.ProofAuditPrivateReplay.shared` as distinct
  full private names, each with user spelling `ProofAuditPrivateReplay.shared`.
- Both private declarations have `unsafe=false` and `partial=false`.
- Public `left` and `right` reference their respective, different private
  declarations.

The backend writer also ran the new Lean audit helper directly against the
actual compiled aggregator and both public roots. It exited 0 in 11.46 seconds
with `kernel_replayed: true`, `checked_constants: 4`, and an empty axiom list
for each root; audit stderr was empty. Logs are under
`/tmp/resource-kernel42-replay-controls-v1`: the three module `.log` files,
`metadata.stdout`, `metadata.stderr`, `audit.stdout`, and `audit.stderr`.
The fixture author inspected those actual records and logs.

This is direct Lean-backend replay evidence. No Rust CLI test, full production
model audit, independent external checker replay, or reproduced old-backend
panic is claimed by this fixture lane. The dedicated Rust integration owner
runs the real CLI regression separately. No Cargo command was run here, no
compiled artifact is tracked, and `git diff --check` passed.
