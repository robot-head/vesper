# Actual complete paths and inside-out reconstruction chains (#549)

`lean/Lean4LeanLevel42/LexChainCompletion.lean` imports only Feasible and
ReifyCongruence. It uses the actual unchanged candidate predicate, find?,
lexChain recursion, and unconditional actual chain permutation theorem.
Completion is the final folded Admissible certificate from the preceding
wave, not the earlier recursive definition or an algorithm-success flag.

`admissible_append_iff` makes accumulator orientation explicit: after a
prefix xs, the prior accumulator is `xs.reverse ++ acc`. From an independent
Completes witness, `completes_last_edge` takes its actual last occurrence.
The preceding order permutes the original path with that occurrence erased,
remains independently admissible from the empty accumulator, and covers the
last name once its preceding names have accumulated. This helper itself needs
no sorting assumption.

For strictly sorted stored keys and path, actual addability reflection and
actual feasible completeness turn that last occurrence into a real candidate.
`lexChain_candidate_exists` proves a member satisfies the actual Boolean
predicate. `lexChain_find_some` connects it to the actual find? result;
`lexChain_no_nonempty_fallback` excludes the failed-find branch for nonempty
complete paths. It does not assert that a successful chain is unequal to the
input path. `lexChain_nonempty_step` states the precise actual cons equation
with positive fuel, leaving the zero-fuel behavior explicit.

`lexChain_reverse_admissible` assumes exactly sorted stored keys, KeySorted
path, independent Completes from the empty accumulator, and fuel at least the
path length. The actual selected predicate supplies a remainder completion
through actual feasible soundness. Erasure preserves sortedness and reduces
length. The recursively admissible reverse chain forms the prefix; actual
chain permutation transports coverage to that prefix before appending the
selected name. The result is admissibility of the REVERSE inside-out chain.
The path-length specialization is `lexChain_length_reverse_admissible`.
Neither result needs MapWF, NoMVars, a semantic-correctness premise or a
supplied candidate result.

The closed `lexChain_reverse_order_witness` uses numeric names a=0 and b=1,
with a+7 stored at [a] and b+0 at [a,b]. Actual feasible succeeds, actual
lexChain returns [b,a], its reverse is [a,b], and an independent Completes
certificate follows from the prior actual search soundness theorem. This
makes the inside-out orientation observable with genuine source computations.

All nine public ASCII theorem names are admissible_append_iff,
completes_last_edge, lexChain_candidate_exists, lexChain_find_some,
lexChain_no_nonempty_fallback, lexChain_nonempty_step,
lexChain_reverse_admissible, lexChain_length_reverse_admissible and
lexChain_reverse_order_witness. No recursive definition or replacement search
algorithm is added. Admissibility is not a minimality/canonicality theorem;
map-to-tree denotation, normalized reification correctness, opaque native
Level.beq reflection and whole-checker/native equivalence remain separate.

## Genuine dependencies, failed attempt and original-object probes

The source was based on frozen commit
`ed18ce23b06d3ffe0fe8a1a3eb1f605e043945ac`. Each attempt copied the ENTIRE
immutable `/tmp/lean4lean-completion42-fold-v2` to a new private versioned root,
then made only that copy writable. Its authoritative fold-identities.json
has 50 records and SHA-256
`c1661137e6c549444335b171a4e7e795beb13217a3ee9d4b93ff33acbeeae249`.
All recorded byte counts and hashes were verified. Completion source was
explicitly required to have the FINAL folded-definition SHA-256
`f89ca02776f406b633af9411aae7d0875e4067d927f0629c5d8f81025a678505`;
the rebuilt Feasible original object SHA-256 is
`5e577ea07ddbdcb8bea12beda740c2a7e07781e4084dc1b9e54e1e0f38083820`.
The earlier recursive-core artifacts were not substituted. Completion,
Feasible and ReifyCongruence source bytes matched this worktree. A single
complete project LEAN_PATH prevented namespace shadowing. No existing
source/object or shared root was mutated.

The actual pinned executable is
`/home/agent/.local/share/vesper-lean/bin/lean`, Lean 4.24.0 commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`. All runs used `-j1 -M4096`,
`LEAN_NUM_THREADS=1`, and the sole private root as LEAN_PATH. The compile
command, run from each attempt root, was:

```sh
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/LexChainCompletion.olean Lean4LeanLevel42/LexChainCompletion.lean
```

The first attempt, `/tmp/lean4lean-reconstruction42-chain-v1`, exited 1 in
5.254s. It used the reserved word `prefix` as a local identifier, producing
parser errors. Other diagnostics identified misuse of the Boolean equality
lemma as an iff, and a find?-some branch whose existential equality had
already rewritten to reflexivity. The failed exact source and
chain-compiler.log/chain-compiler.json remain read-only. Corrections use
`front`, Bool.and_eq_true_iff, and rfl in that branch. Static library inspection
also fixed append-singleton argument order and made double-reverse context
normalization explicit before the next attempt. No theorem premise changed.

For coordinated lease fairness, compiler lease 2 was explicitly returned
after that attempt while another lane completed its finite compile/probe.
Only static proof/report work continued until the coordinator explicitly
regranted the lease. The final fresh v2 compilation then exited zero without
diagnostics in 1.177s.

The handwritten ChainAudit.lean imports the genuine original module, prints
axioms for the authored roots, and uses actual environment module ownership
to enumerate EVERY original declaration, checking its safety. The first
successful probe took 2.602s. It found fourteen original declarations: nine
authored theorems and these five actual generated declarations:

- Admissible.eq_1 (theorem)
- admissible_append_iff.match_1_1 (safe definition)
- admissible_append_iff.match_1_3 (safe definition)
- lexChain_reverse_admissible._proof_1_1 (theorem)
- lexChain_reverse_admissible._proof_1_3 (theorem)

The full original-object probe was extended to print axioms for all five as
well. The same pinned command with ChainAudit.lean as argument exited zero in
2.736s. All fourteen actual declarations use only the standard-three policy:
propext, Classical.choice, Quot.sound; the two proof aliases need only
propext/Quot.sound. No unsafe/partial definition, project axiom or additional
recursive definition was found. No project-helper omission or policy change
is requested. No admission or native decision procedure is used. This direct
original probe supplements the coordinator's unchanged complete projection
and alternate-alias audit, which remains an independent integration gate.

The sealed final publication is
`/tmp/lean4lean-reconstruction42-chain-v2`. All 53 private input records were
rechecked after compilation. It retains exact source/object identities,
compiler records, both successful probe logs and the full audit source.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| LexChainCompletion.lean | 7345 | `52d95a4878989bb10c00444865e75ab7234b9ddf7ebd9d370c48631774a81833` |
| LexChainCompletion.olean | 164344 | `8a8a15dd799a484ce7e0eb53cecb23179ee1894ca2ed2bb62a44ebd9c6674d7d` |

The complete 62-file chain-identities.json SHA-256 is
`7b33285a521ffc8d39086e3cff1f5e4d21c892cebc9df289873851d3ab126343`.
Both attempt roots are read-only. The regranted Lean lease 2 was explicitly
released after final publication. No Cargo, push or PR ran here. The own commit
contains only the two new files; existing algorithms, proof helpers,
runner/BUILD registration, source caps, policies and external ownership remain
untouched. Minimality/canonicality and normalized reification correctness are
not claimed.
