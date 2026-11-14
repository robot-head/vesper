# Foundation acceptance (#406)

Source is authored; execution is pending coherent proof sources and the
coordinator's build lease. No Cargo, Lean compilation, projection or audit has
been run by this acceptance lane. Formatting and whitespace checks passed.

The new target is `tests/lean4lean_foundations42.rs`. It leaves existing tests,
profiles, exporters, auditors, Cargo and CI untouched. The isolated source branch
imports only the coordinator's aggregate dependency; that dependency is not an
acceptance-lane implementation change.

The target reads the exact vendored List/Level source bytes, verifies selected
provenance and existing source-manifest entries, binds the unchanged kernel
patch and license, and freshly compiles the complete fixed proof import closure.
It permits toolchain Lean/Std/Init imports but rejects missing package imports
and unreferenced proof source files. New named proofs must use the contract's
public ASCII theorem syntax. The catalog is derived from actual fixed sources,
not a golden theorem count; required semantic, counterexample, node-helper and
fresh-name roots prevent a vacuous catalog.

The unchanged kernel snapshot operates on fresh legacy source/object pairs in
a private root, exporting to a disjoint new directory. The harness validates
module paths, trusted-root confinement, canonical/omitted/duplicate/occurrence
partitions and every actual proof-alias mapping. Every source/object, exporter
and projected-object association is checked before and after operations.

Every authored theorem and every actual alias is submitted to the real strict
auditor in batches of at most 128. Returned root union, complete replay count,
standard axiom policy and real artifact hashes must match. The bounded actual
evidence marker is printed immediately after this positive stage, before later
refusal controls, so a later failed assertion cannot erase its diagnostic data.

The remaining matrix changes an actual private copied source, removes/corrupts
an imported Level object, removes/corrupts the standalone projection, and reuses
the existing fixed AliasAxiom triplet. Its canonical-only proof must genuinely
pass while adding the actual alternate-proof alias must reject the exact
forbidden project axiom. No successful synthetic transcript, expected-accept
flag or source-only outcome substitutes for execution.

The first finite budget is 300 seconds for the whole target, with 4-MiB fixed
sources, 24 source modules, 8-MiB bounded stdout/stderr captures and bounded
regular artifact inventories. Capture files live outside artifact roots;
timeout handling reaps the direct child and makes no descendant-cancellation
claim. Runtime and platform validation remain unmeasured here. No scheduling
or default bound is changed based on prediction.

Passing this target will establish the stated logical foundation lemmas and
their source/artifact/axiom associations. It will not complete map/normalizer or
checker correctness, prove callers satisfy the fresh-name invariant, prove
native equivalence, or change the external profile's audit-status fields.
