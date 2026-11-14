# Actual comparison and sorted-key acceptance (#416)

The Order entry point and static packet are authored. This lane ran direct
rustfmt and static source checks only. It did not execute Cargo, Lean, the
three genuine acceptance matrices, strict Clippy or Bazel. The coordinator
owns those integration checks; this report does not claim they passed.

`tests/lean4lean_order42.rs` calls the shared runner through the closed
`Packet::Order` choice. Its fixed specification selects `Lean4LeanOrder42`,
projects `Lean4LeanOrder42Kernel`, uses label `lean4lean_order42`, scope
`audited_named_order_proofs` and marker `LEAN4LEAN_ORDER42_EVIDENCE`.
There is no runtime module selector, supplied expected answer or alternate
acceptance policy.

The static closure contains exactly nine modules, in contracted dependency
order: `Lean4Lean.List`, `Lean4Lean.Level`, `Lean4LeanLevel42.Semantics`,
`Lean4LeanLevel42.ListProofs`, `Lean4LeanLevel42.OrderedInsert`,
`Lean4LeanLevel42.VarInsert`, `Lean4LeanLevel42.NameOrder`,
`Lean4LeanLevel42.SortedKeys` and the import-only `Lean4LeanOrder42` aggregate.
The existing import-closure check still requires every selected source to
enter the replayed artifact closure.

The Order required catalog guards all eleven named comparator APIs, including
both named class bridges and the three explicit path-comparison APIs, and all
seven sorted-key/variable APIs. It also guards actual numeric, string and
recursive-prefix Name/path comparison witnesses; actual numeric sorted
insertion and duplicate-detection witnesses; numeric VarNode offset merge;
and the explicit unsorted-key witness. The named class bridges are public
theorem roots registered as instances. Required names are a presence guard, not the
audit inventory: the unchanged source-command extraction discovers every
public authored theorem throughout the selected actual helper sources, then
the unchanged root union adds every actual alternate-proof alias. The audit
runs over that complete union, including newly authored computational
witnesses, without a golden theorem or alias count.

Static comparison with base `3f6a8c6` confirms both original required arrays
and both original Packet specifications are byte-identical. Everything from
`const POLICY` through the end of the shared runner is also byte-identical:
provenance, source extraction and binding, original/projection hash
association, occurrence/partition validation, complete root/alias union,
strict audit validation and every refusal control remain intact.

The fixed standard-three axiom policy, 128-root batches, 300-second per-matrix
budget, bounded regular-file capture and artifact/source limits therefore
apply unchanged. The real missing/corrupt original and projected object
controls, changed-source and provenance checks, canonical-clean versus
forbidden-alternate-proof control, tool identity and before/after hashes
remain part of each genuine run.

These proofs concern the actual Name comparator and sorted list
representation invariants. They do not establish map-wide construction,
minimization or normalization; caller path priming; implementation audit
status; compiler refinement; native equivalence; or whole-checker
verification.
