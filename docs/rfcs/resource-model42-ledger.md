# Finite ledger embedding proofs (#322)

Owned source checkpoint `6ae4905` adds only
`lean/ResourceModel42/LedgerProofs.lean`. It imports the actual Evaluator;
Evaluator does not import it. Dependencies are Syntax `d51a21b`, Checker
`8fc7fff`, and Evaluator `26ec38f`, over unchanged Full at baseline `30de7fe`.
The local dependency copies `8a0f914` and `96a6802` are not ledger-owned work.
The latter preserves the evaluator report supplied with its dependency commit.

The proofs use the actual `Config.live` lookup, `Config.erase`, Array.push,
Array.modify, and `PlannedAction.apply`. They establish that the finite ledger
embeds the existing Full resource store:

- Every embedded store is fresh above the ledger's total length.
- Mint appends a live entry at exactly the old length, matching Full's store
  update and increasing its next serial by one.
- Consume's actual modify-to-dead operation matches setting that resource to
  none and preserves array length. The store equality also holds for an
  out-of-range index; operational consume admission remains Evaluator's job.
- Push preserves old lookups; consume preserves mint-site metadata. Live IDs
  are below total allocation history, and action history length never shrinks.
- Full `DiscardR` implies every retained entry is dead, hence an actual
  EvalResult has an empty residual list under that premise.

These are store/array embedding lemmas. They do not assume an expected trace,
assert an unproved evaluator invariant, or insert a terminal cleanup. Actual
step correspondence, reachable-state progress, event correspondence, and
conditional copy-result exhaustion remain the operational proof lanes' work.

## Complete public theorem catalog

All 20 manual theorems are public, under
`Vesper.ResourceModel42.LedgerProofs`:

1. `erase_live`
2. `erase_next`
3. `live_linear_iff`
4. `live_lookup`
5. `live_fresh`
6. `erase_fresh`
7. `live_push`
8. `live_modify_dead`
9. `ledger_size_mint`
10. `ledger_size_consume`
11. `live_consumed`
12. `push_lookup_fresh`
13. `push_lookup_history`
14. `consume_mintSite_history`
15. `live_id_lt`
16. `mint_id_not_old`
17. `action_history_size_monotone`
18. `action_live`
19. `discard_entries_dead`
20. `discard_residual_nil`

There are no private helper theorems, sorry/admit/project axioms, unsafe or
partial definitions, or native-decision shortcuts in this module.

## Actual execution and limits

With the coordinator's explicit one-compiler lease, the source writer ran the
real pinned `/home/agent/.local/share/vesper-lean/bin/lean`, using
`LEAN_NUM_THREADS=1`, against the immutable
`/tmp/resource-model42-evaluator-api-v1` dependency chain. The final compile
exited 0 with an empty diagnostic log:
`/tmp/resource-model42-ledger-build-v1/compile.log`.
Initial failures concerned an unannotated quantified entry type and a Boolean
case goal; those were corrected as genuine Lean proofs. Unused simp arguments
were removed. `git diff --check` passed.

The complete immutable, read-only dependency checkpoint is
`/tmp/resource-model42-ledger-api-6ae4905`, containing Full, Syntax, Checker,
Evaluator and LedgerProofs sources/oleans. No compiled artifact is tracked or
stored in the checkout. The compiler lease was released immediately after
the coherent checkpoint; no Cargo command was run.

Lean compilation kernel-checks these proof terms, but no separate transitive
axiom-policy audit or independent external replay is claimed by this report.
Independent acceptance must include all 20 roots in the assembled catalog and
partition requests when the complete project's union exceeds 128 roots. These
proofs concern the finite model, not a Rust/compiler refinement or a resource
differential campaign.
