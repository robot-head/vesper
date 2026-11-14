# Actual feasibility and independent completion certificates (#540)

`lean/Lean4LeanLevel42/Feasible.lean` imports only Completion and Addable.
It proves facts about the actual unchanged `NormLevel.addable`,
`NormLevel.feasible.go`, `NormLevel.feasible` and orderedInsert/getD/erase
operations. The independent EdgeCovered/Admissible/Completes definitions and
core accumulator monotonicity proofs remain root-owned. Root core commit
`51410adaae202f20f8c1b16d6708c3df4f086a07` was incorporated separately as
`3458308`; this lane's own commit contains only the new module and this report.

`addable_edgeCovered` needs only actual successful addability. A stored
operational witness supplies arbitrary offset k and actual successful subset
on the erased key. Every original key name is either the erased name or is
still in that erase, so it belongs to `a :: acc` even for duplicate/raw keys.
`addable_iff_edgeCovered` adds strictly sorted stored keys and accumulator to
obtain the reverse implication from the existing actual addability theorem.
No WF, variable sorting, offset-zero or semantic premise is introduced.

The exchange proof does not assume remaining-name distinctness or freshness:
`admissible_erase` moves a name into the accumulator and erases one occurrence
from an admissible order. Earlier obligations retain coverage by accumulator
monotonicity. At the removed occurrence, the old tail already has that name
available. Cons membership commutes for subsequent obligations.
`completes_erase` transports the independent permutation through actual erase,
retaining duplicate multiplicity.

`feasible_insert_getD_mem` proves that actual ordered insertion/getD has the
same name membership as `a :: acc` for arbitrary input. A none result really
implies that a was already present; a some result uses the genuine insertion
membership theorem. Its sorting helper requires only sorted input.

`feasible_go_sound` proves success implies Completes without any sorting or
fuel-length premise. Every selected name is really in the remaining list and
actually addable. The recursively certified erase is reconstructed by actual
cons/erase permutation, and accumulator membership transports its admissible
tail. Zero fuel or failed search can succeed only on an empty remaining list.

`feasible_go_complete` assumes exactly sorted stored keys, sorted accumulator,
`rem.length ≤ fuel`, and the independent Completes certificate. A nonempty
certificate provides a covered first name, so actual find? cannot return none.
Every actual selected occurrence can be exchanged to the front; actual erase
reduces length by one and orderedInsert/getD preserves sorted accumulator
coverage. Remaining lists may contain duplicates or already accumulated names.
`feasible_sound`, `feasible_complete` and `feasible_iff_completes` specialize
to the real implementation's fuel `rem.length`.

The closed `feasible_duplicate_present_witness` stores a+7 at key [a], starts
with accumulator [a], and asks to complete [a,a]. Actual orderedInsert returns
none, actual go with fuel one fails, and actual feasible with fuel two succeeds
and supplies Completes. It demonstrates that no hidden nodup, `a ∉ acc`,
successful-insertion or offset-zero restriction is needed.

There are thirteen public ASCII theorem roots: addable_edgeCovered,
addable_iff_edgeCovered, feasible_insert_getD_mem, feasible_insert_getD_sorted,
admissible_erase, completes_erase, completes_nonempty_edge, feasible_go_sound,
feasible_go_complete, feasible_sound, feasible_complete,
feasible_iff_completes and feasible_duplicate_present_witness. Their names and
main signatures were frozen with the coordinator and acceptance lane.

## Genuine compilation and original-object audit

All compiler execution waited for the coordinator's explicit lease. Each
attempt copied the entire immutable `/tmp/lean4lean-completion42-core-v3` into
a fresh private versioned root and unsealed only that copy. All 63 records in
its manifest were verified; manifest SHA-256:
`2b069f56969ece810e457ae393c32ab0de2c09fd12fd909bced5fdc0136436c7`.
The complete genuine Addable parent manifest's 72 records were also verified:
`/tmp/lean4lean-addability42-addable-v3/addable-identities.json`, SHA-256
`2cf258ec5339550341991695dd2273439436b068cb39ed13afa705c2f39ceb81`.
Only the needed genuine SortedSubset, MapAny and Addable source/object pairs
were added, with the complete original parent manifest retained. Their source
bytes matched this worktree. One complete private LEAN_PATH supplied every
actual import; there was no namespace-prefix layering or dependency mutation.
The final private input manifest has 72 records, all checked after compilation.

The pinned executable was `/home/agent/.local/share/vesper-lean/bin/lean`,
Lean 4.24.0 commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, with
`LEAN_NUM_THREADS=1`, `-j1 -M4096` and LEAN_PATH set to the sole private root.
From each attempt root, the command was:

```sh
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/Feasible.olean Lean4LeanLevel42/Feasible.lean
```

The failed v1 attempt exited 1 in 1.036s. It reported a reversed nil-Perm
orientation, an ambiguously inferred find?_some predicate, an unavailable
by_contra tactic, and a deprecated length lemma warning. The none-branch
parse failure also left subsequent branches unprocessed. The corrections use
the symmetric nil equality, an explicit actual predicate, ordinary by_cases,
and length_eq_zero_iff. No mathematical premise or claimed result changed.
The failed source/log/command/environment/exit record remains read-only at
`/tmp/lean4lean-completion42-feasible-v1`.

The v2 proof compiled cleanly in 0.927s. The final v3 source additionally
clarifies the witness comment and includes its real insufficient-fuel result.
It compiled cleanly in 0.943s. Both successful attempt roots are retained.
The handwritten FeasibleAudit.lean imported the final genuine original object
and ran `#print axioms` on all thirteen roots. The same pinned command with
FeasibleAudit.lean exited zero in 2.183s; every root uses only propext,
Classical.choice and Quot.sound. No project axiom, admission, unsafe/partial
escape or native decision procedure was added. The coordinator separately
owns the full fixed-packet projection and alternate-alias audit.

The sealed final artifact root is
`/tmp/lean4lean-completion42-feasible-v3`. It retains all copied inputs, exact
compiler/audit logs and JSON execution records, plus genuine final artifacts:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| Feasible.lean | 7838 | `c1eb2fc81c26b38d99f7708c010fdee4e3bc9b3e0598846b72255b0db68ed708` |
| Feasible.olean | 194208 | `d61b59e8005ee7edb65741a515ea7e901f7571a66d42cabd6a91cad1b949dab7` |

The complete 79-file `feasible-identities.json` SHA-256 is
`9a741d1dfd7fadf578b476be4b1d78b70c35fb90cee6a3bc9dfe7ffba95b3157`.
All attempt roots are read-only. The granted Lean lease was explicitly
released after successful publication. No Cargo, push or PR ran in this lane.
Existing sources, shared runner, BUILD, production algorithms and proof policy
remain untouched. NormalizeAux completion preservation, lexChain
admissibility/minimality, tree denotation, normalized reification correctness,
canonicality and whole-checker/native equivalence remain separate.
