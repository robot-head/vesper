# Normalizer semantics and list foundations (#403)

The two fixed source modules compile on pinned Lean 4.24. They contain **14**
public authored theorems: nine in Semantics and five in ListProofs. This is
native Lean compilation evidence, not a completed transitive axiom audit or a
proof of the entire normalizer/checker. The coordinator's complete artifact
projection, replay and root/alias audit remains a separate gate.

The public parent/child claims and pushed contract preceded edits. Scope is
only `lean/Lean4LeanLevel42/{Semantics,ListProofs}.lean` and this report. No
vendored helper, sealed checker, kernel, auditor, runtime, Cargo or CI source
was changed.

## Definitions and proved statements

The namespace is `Vesper.Lean4LeanLevel42`, importing the actual pinned
`Lean4Lean.Level`, without the old Theory/Verify modules or their assumptions.
Semantics defines the frozen valuation, enabled-key test, maximum variable
value, conditional node value and variable/condition-membership invariant.

Its nine theorems establish enabled-key membership and subset transfer,
empty/cons/append variable values, member value bounds, enabled/disabled node
values and positivity of a well-formed enabled node's variables.

ListProofs establishes:

- `name_cmp_eq`: equality reflection for actual `Lean.Name.cmp`, by structural
  recursion on names and standard Nat/String comparison facts.
- `subset_sound`: actual `Normalize.subset Name.cmp` returning true implies
  ordinary list membership inclusion. It needs no sorting or uniqueness.
- `subsumeVars_union`: pruning preserves the maximum union with the original
  dominator list, for all valuations and arbitrary variable lists.
- `offset_fold_bound`: a checked fold bound for an arbitrary initial offset.
- `offset_bound`: for a nonempty variable list whose valuations are positive,
  the actual production maximum-offset fold plus one is bounded by its value.

The proofs refer to the real helper definitions. They introduce no expected
acceptance flags, sortedness assumption, desired-result premise, project axiom,
admitted proof, unsafe/partial definition or native_decide escape. All named
theorems are public ASCII `theorem` commands. The explicit Name.cmp parameter
avoids accidentally using the different global Ord Name instance.

## Actual compilation and immutable dependencies

Actual compiler:
`/home/agent/.local/share/vesper-lean/bin/lean`, pinned commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`.
Each invocation used `LEAN_NUM_THREADS=1`, `-j1 -M4096`, and only the private
dependency root plus this toolchain's `lib/lean` on LEAN_PATH. No Cargo ran.

The selected List/Level bytes were checked against the source-provenance
manifest's lengths and SHA256 values and independently associated by BLAKE3:

| Source | Bytes | BLAKE3 |
| --- | ---: | --- |
| Lean4Lean/List.lean | 176 | `508045af9f1a1590749506718fa43245c035a5d37fd06e7ae97ec9722a4e1027` |
| Lean4Lean/Level.lean | 13,744 | `4b3f0b40448d255f3d7d48e03f2251610b73993d13beb6fdd372f0d511538139` |

Fresh List, Level and Semantics compilation passed with empty stdout/stderr.
The complete Semantics dependency was published early at source checkpoint
`fcd9425` in immutable `/tmp/lean4lean-foundations42-semantics-v1`.

ListProofs' final compilation passed with empty stdout/stderr. Its three
earlier unsuccessful development invocations found ordinary Lean name,
elaboration and termination-tactic issues: an unavailable congruence helper,
implicit list-case rewrites, ambiguous fold binder types, and a redundant
termination tactic. These were fixed in the proof source; no success was
reported for those attempts and no unfinished stub was published.

Final source checkpoint is `791eaf1`. The immutable dependency root
`/tmp/lean4lean-foundations42-semantics-v2` contains the unchanged v1 dependencies
plus ListProofs. All six v1 source/object associations were rechecked equal
before publishing v2. Both roots carry an `identities.json` table.

| File | Bytes | BLAKE3 |
| --- | ---: | --- |
| Semantics.lean | 2,608 | `524650eab8518da85c93ee1f43789871071b0d8a8dcc83c35906ad95dfa348ad` |
| Semantics.olean | 95,264 | `f87105b1f18a674f52941a89e89400e1cdbae9ec20a9f6803fd560a323359f6c` |
| ListProofs.lean | 4,828 | `441ad7a22f7c3b9807de5534fbe53e08e0fda20e1abc2fab8905663aa145ac72` |
| ListProofs.olean | 2,727,256 | `3ddcf682b7f9bd1728426ad96a7c94f67ac374880dec4a14634349b9b02faafd` |

The sibling subsumption lane received v1 and then v2 with exact source/object
hashes. Published dependency roots were never edited afterward. These hashes
associate observed bytes; they do not prove native binary derivation.

Map normalization, full level comparison/canonicity, declaration admission,
the checker's implementation proof library and native equivalence remain
outside this packet. Existing external profile audit fields are unchanged.
