# Actual sorted subset and erasure foundations (#528)

`lean/Lean4LeanLevel42/SortedSubset.lean` imports only the frozen SortedKeys
module. Every statement concerns the actual unchanged
`Normalize.subset Name.cmp` implementation, actual List.erase, or the existing
strict prefix-first Name.cmp KeySorted predicate. Existing proof and vendor
sources remain read-only.

`subset_sublist` is unconditional apart from actual successful comparison.
Its induction follows the right list: equality consumes one matching entry
on each side, and greater-than skips one right entry. Thus success preserves
left multiplicity even for arbitrary raw lists. `keySorted_erase` uses the
actual erasure sublist to preserve strict order.

`subset_complete` carries strict order on both arguments. The less-than case
cannot contain the left head later in a strictly sorted right list. Equality
removes matching heads; strict left order prevents that head from reappearing
in the left tail. Greater-than safely skips the right head, which strict left
order excludes from the entire left list. `subset_eq_true_iff` combines this
with the existing unchanged subset_sound theorem. No substitute search or
assumed completeness result is introduced.

`erase_subset_iff_subset_cons` states exactly
`p.erase a ⊆ acc ↔ p ⊆ a :: acc`, under only KeySorted p. Forward membership
uses the actual erase membership lemma for a different name. The reverse
uses the genuine no-duplicate consequence of strict order, since erasure
removes only one occurrence. No order assumption on acc is required.

The four numeric witnesses are concrete actual computations:

- `subset_needs_left_sorted`: `[1,0] ⊆ [0,1]`, with the right list strictly
  sorted, but the actual subset test returns false.
- `subset_needs_right_sorted`: `[0] ⊆ [1,0]`, with the left list strictly
  sorted, but the actual early less-than branch returns false.
- `subset_duplicate_left_counterexample`: `[0,0] ⊆ [0]`, while the actual
  test returns false because there is only one matching right occurrence.
- `erase_subset_needs_nodup_counterexample`: `[0,0] ⊆ 0 :: []`, while
  erasing only one zero leaves a list that is not a subset of the empty list.

All five contracted APIs and these four public ASCII witness names were
frozen with the Addable and acceptance lanes before dependency publication.
These facts do not establish feasible completeness, greedy completeness,
fallback unreachability, canonicality, normalized reification correctness,
opaque Level.beq reflection or whole-checker/native equivalence.

## Genuine execution and retained failures

The granted Lean compiler lease used the actual pinned 4.24.0 binary at
`/home/agent/.local/share/vesper-lean/bin/lean`, with `-j1 -M4096` and
`LEAN_NUM_THREADS=1`. Each attempt copied the ENTIRE immutable
`/tmp/lean4lean-addability42-shared-v1` to a fresh private versioned root and
made only that copy writable. All 58 authoritative byte-count/SHA-256 records
were verified before copying. No namespace-prefix roots were layered.
The shared `addability-dependency-identities.json` SHA-256 is
`021fd786e9b6f71a5dc9e62bb9f9c26928965386021d1f5c12bf660b7aa6111b`.

Both failed attempts remain read-only with exact source, compiler.log and
compiler.json command/environment/exit/duration records:

1. `/tmp/lean4lean-addability42-subset-v1`: exit 1 in 0.999s. All five main
   proofs checked; six numeric witness fields could not synthesize
   Decidable while KeySorted remained folded.
2. `/tmp/lean4lean-addability42-subset-v2`: exit 1 in 1.043s. Explicit
   unfolding fixed that issue, but three redundant decide tactics after
   closed simp goals failed with `No goals to be solved`; two singleton
   simp argument lists also produced unused-argument warnings. The final
   witnesses use the existing actual unsorted-key witness or precise
   predicate/refl simplification, retaining one needed ordinary decide.

The final command ran from the fresh complete root
`/tmp/lean4lean-addability42-subset-v3`:

```sh
LEAN_PATH=/tmp/lean4lean-addability42-subset-v3 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/SortedSubset.olean Lean4LeanLevel42/SortedSubset.lean
```

It exited zero without diagnostics in 1.016s. The handwritten
`SubsetAudit.lean` imports the genuine original object and prints axioms for
all nine authored theorem roots; the same pinned command with
`SubsetAudit.lean` as its argument exited zero in 0.761s. Every reported axiom
is within the unchanged standard-three policy: propext, Classical.choice,
Quot.sound. keySorted_erase uses only propext, and the duplicate erasure
witness uses only propext/Quot.sound. No project axioms, admissions, unsafe or
partial definitions, or native decision procedure were introduced. This
original-root check supplements the coordinator's complete packet/alias
projection audit; it does not replace it.

All 58 dependency identities were verified again in both the shared root and
successful private copy after compilation. The final source byte-matches the
owned working-tree file. The sealed read-only final publication contains the
complete copied dependency tree and genuine original artifacts:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| SortedSubset.lean | 5176 | `0daeda536a5b2fa43956a3f8dba4e4c09eb16c43d9cd46de14d97265de4c93df` |
| SortedSubset.olean | 127688 | `d0635d8b5403cbbe931665e12d79c5bed5dcb7bf2cfe5a29a158404c61cf6303` |

The complete 60-record `subset-identities.json` SHA-256 is
`84878fc77116fc2ca409328e9113ceac29bca8183323b25b63a1a09a06afae6b`.
Final compiler and direct axiom logs/JSON records are retained in that root.
Only the two new owned files are committed. No Cargo command, push or PR ran.
Lean compiler lease 1 was explicitly released after publication so the
Addable lane could compile against the genuine dependency. The coordinator
owns the new packet and all eight preceding complete matrices.
