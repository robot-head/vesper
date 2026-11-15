# Archived Lean4Lean source provenance

The external backend is now [Logos](../logos42/README.md). The `lean4lean42`
binary and this directory's `setup.py` are compatibility entry points for Logos;
setup now requires the same `--cargo` argument as the canonical tool.

The patches and manifests below retain the source provenance used by the
`Lean4LeanLevel42` mathematical helper proofs. They do not describe the active
checker, and Lean4Lean receipts cannot be used with Logos.


The earlier implementation review remains in `docs/rfcs/lean4lean42-proof-plan.md`.
The seven helper proof packets continue to audit their archived source models;
they do not establish correctness of the Logos implementation.
