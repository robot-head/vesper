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
execution evidence; actual compile/metadata results will be recorded below
once a compiler lease is granted. No corrupted-metadata fixture is claimed.
