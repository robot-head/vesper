# Actual construction shape (#501)

This lane owns only `lean/Lean4LeanLevel42/ConstructionShape.lean` and this
report under the frozen #499–#503 contract. Root dependency correction
`3872706` was imported as `5b3f324`; it changes only shared Shape and the
contract. No existing algorithm, helper proof, vendor source, runtime,
checker/auditor/profile, Cargo or CI source changed in this lane.

The module imports only the root-owned `Lean4LeanLevel42.Shape`. The final
shared Shape imports SortedKeys, NormalizeValue and MapSubsumption, avoiding
unnecessary comparison modules in the new packet's logical closure. Complete
artifact copies still retain every fixed parent object; unused files do not
become new imports or acceptance roots.

## Public results

Fourteen public ASCII theorems establish structural properties of actual
`NormLevel` operations:

- `map_keysSorted_empty` and `map_varsSorted_empty` provide the genuine empty
  map bases.
- `map_addConst_keysSorted` and `map_addNode_keysSorted` require the actual
  supplied key's KeySorted predicate and initial MapKeysSorted.
- `map_addVar_keysSorted` needs only initial MapKeysSorted. The actual modify
  cannot create a key; presence of its updated key supplies the needed order.
- `map_addConst_varsSorted`, `map_addNode_varsSorted` and
  `map_addVar_varsSorted` need only initial MapVarsSorted. Constant insertion
  creates an empty variable list, node insertion creates a singleton, and
  actual `VarNode.addVar` preserves strict order by variable name.
- `normalizeAux_varsSorted (l) (p) (k) (acc) (hs)` uses the frozen actual
  construction-property theorem with those genuine update-preservation facts.
- `normalizeAux_keysSorted (l) (p) (k) (acc) (hp) (hs)` carries the sorted
  current path through the actual `levelSize` recursion. Successful
  orderedInsert uses `orderedInsert_sorted`; all other paths remain unchanged.
  The proof covers both fresh imax terms created by max-right/imax-right
  rewrites, sequential accumulator updates, member/offset cases and both
  metavariable cases.
- `normalizeAux_empty_varsSorted` and `normalizeAux_empty_keysSorted` are
  empty-map construction corollaries for every actual Level.
- `normalizeAux_needs_sorted_path` proves a real necessity witness: starting
  from the empty map, actual normalization of zero at numeric path `[1,0]`
  with offset two stores that unsorted key. The witness uses actual alter and
  lookup facts, rather than assuming the stored result.
- `map_addVar_varsSorted_merge_witness` exercises actual map modification:
  `[0+2,1+7]` at key `[0,1]`, inserting `0+5`, produces actual lookup
  `[0+5,1+7]` and preserves the map's strict variable-name order.

No NoMVars, MapWF, priming, valuation or semantic-correctness premise appears.
Variable order needs no current-path order premise. Every Level, including
metavariables, is admitted. The results do not claim normalization repairs an
arbitrary initially unsorted map, or that shape alone establishes canonicality,
comparison completeness, reification or whole-checker/native equivalence.

## Dependency association and genuine compiler execution

The initial lease was explicitly returned before root rebuilt the minimal
shared module. No compiler ran against shared v3. Root then regranted Lean
lease 2 for the final complete shared snapshot:

```text
/tmp/lean4lean-shape42-shared-v4
shape-dependency-identities.json SHA-256
3d413232882773a770b9501cb9a7466b83c964fd59c1edf1d5e3f631d6cc7ece
```

All 49 recorded dependency byte counts and SHA-256 hashes were verified. They
include 23 source/object pairs and three inherited metadata files. Every one
of the 23 source files byte-matched the current frozen worktree or vendor
source. Each attempt copied the entire shared root into a new versioned
private root and made only that copy writable before adding the owned module.
The pinned namespace prefix was therefore supplied by one complete own root;
no mixed LEAN_PATH roots or parent-object recompilation were used.

All failed attempts are retained, not overwritten:

1. `/tmp/lean4lean-shape42-construction-v1`: the first command was mistakenly
   invoked with cwd `/tmp`, so relative `Lean4LeanLevel42/ConstructionShape.lean`
   did not exist there. Lean exited 1 with the actual file-not-found diagnostic;
   it did not check the proof. Its exact command/cwd/result is in `attempt.json`.
2. `/tmp/lean4lean-shape42-construction-v2`: the real proof compile reported
   unsolved missing-key cases at lines 51 and 102, because unrestricted simp
   changed get? notation before using its equality. Controlled `simp only`
   with actual Option.map_none fixes those cases. The unsorted-path witness's
   raw lookup rfl also failed; it now derives the lookup from actual alter.
   Actual log: `/tmp/lean4lean-shape42-construction-v2.log`.
3. `/tmp/lean4lean-shape42-construction-v3`: all substantive proofs passed, but
   an explicit rfl after a closing rewrite in that witness reported `No goals
   to be solved` at line 211. Removing only that redundant tactic fixed it.
   Actual log: `/tmp/lean4lean-shape42-construction-v3.log`.

Each attempt used the same command shape, with its own root substituted below;
v1 alone used the mistaken cwd already stated. The final command ran from
`/tmp/lean4lean-shape42-construction-v4`:

```sh
LEAN_PATH=/tmp/lean4lean-shape42-construction-v4 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/ConstructionShape.olean Lean4LeanLevel42/ConstructionShape.lean
```

This genuine pinned Lean 4.24 compilation exited 0 with no diagnostics.
`/tmp/lean4lean-shape42-construction-v4.log` is empty. No Cargo invocation ran.

After the successful compile, all 49 dependency identities were checked again
against both the untouched shared root and final private copy. The source
matched the owned worktree byte-for-byte. Final artifacts are:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| ConstructionShape.lean | 8541 | 83fcd889226d19b18cb9150ea8798c7c19d4919a16f57b4018b1b5a6a3bcfe3b |
| ConstructionShape.olean | 280408 | 5258fb98150f366be68bd14920ea003717f48521fcb0e89b3df8eaafb943b3e8 |

The final full 52-record manifest is
`/tmp/lean4lean-shape42-construction-v4/construction-identities.json`, SHA-256
`c0773d137ad0a4d0d0422aeeca374a4634af4d831662046429d627174bb60178`.
This includes every copied dependency/metadata file and both new artifacts;
its own hash is recorded separately here. All four attempt roots are now
read-only. Whitespace validation passed. Compiler lease 2 was explicitly
released immediately after final identity verification. Root aggregate,
projection/audit, all-alias replay and complete matrix execution remain the
coordinator's separate integration gates; none is claimed from this compile.
