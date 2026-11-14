# Actual subsumption shape (#502)

This lane owns only `lean/Lean4LeanLevel42/SubsumptionShape.lean` and this
report. It imports the frozen shared `Shape` module and its genuine parent
closure. No existing proof, source helper, vendor, runtime, auditor, profile,
extractor, Cargo, or workflow source is changed.

The actual `subsumeVars` result is a sublist of the left input, without any
premise on either input. That frozen fact and the actual node/minimizer
sublist facts transport strict variable-name sorting through `subsumeVars`,
`Node.subsumeBy`, `Node.subsume`, and `NormLevel.minimize`. Neither the right
node nor the minimizer's fixed map requires sorting or WF for these results.

The outer-fold sorting proofs use the actual `minimizeEntry` source-expression
abbreviation. Key sorting requires a sorted visited key and a key-sorted
accumulator. Variable sorting requires sorted original variables and a
variable-sorted accumulator. Their generalized list folds allow duplicate or
empty original rows; they need no distinctness or original-row lookup
assumption. Actual source-map lookups supply the row sorting hypotheses when
specializing to the original `s.toList`.

The no-empty-row proof is unconditional for every raw map. Its generic fold
invariant says each current stored row is nonempty or its key occurs among the
remaining fixed source rows. A visit leaves the current key nonempty whenever
it remains stored, because the actual source expression erases precisely when
the minimized node is empty. Every other lookup is unchanged. Thus empty rows
can only remain at keys scheduled for a future visit; after all source rows
have been visited, every stored row is nonempty. The initial invariant follows
directly from actual lookup/list membership. This argument needs no WF,
sorting, NoMVars, source-key distinctness, or original-current-node equality.

The shared v4 contract/source revision was cherry-picked from root commit
`3872706` as local commit `f48fcda`, before the owned change. The predicates did
not change. Shared `Shape` now imports only `SortedKeys`, `NormalizeValue`, and
`MapSubsumption`; this module imports only `Shape`, without importing map
comparison, its fold/all modules, or the previous aggregate.

The coordinator explicitly granted compiler lease 1 after the equality lane
released it. Both attempts copied the entire immutable
`/tmp/lean4lean-shape42-shared-v4` directory into a fresh versioned private
root, changed permissions only on that copy, and used one complete `LEAN_PATH`
root. All 49 byte-count/SHA-256 input records were verified before and after
each copy and after successful compilation. The input manifest
`shape-dependency-identities.json` SHA-256 is
`3d413232882773a770b9501cb9a7466b83c964fd59c1edf1d5e3f631d6cc7ece`.
The shared Shape/SortedKeys source files and the final authored source matched
their working-tree files byte for byte. No dependency source or object was
edited or recompiled in this lane.

The first genuine attempt in
`/tmp/lean4lean-shape42-subsumption-v1` returned exit status 1 with exactly:

```text
Lean4LeanLevel42/SubsumptionShape.lean:11:5: error(lean.unknownIdentifier): Unknown identifier `Normalize.subsumeVars`
Lean4LeanLevel42/SubsumptionShape.lean:15:16: error(lean.unknownIdentifier): Unknown identifier `Normalize.subsumeVars`
```

Adding `Lean.Level` to the namespace-opening command resolved both names. No
theorem statement or proof argument changed. The failed source and exact
`compiler.log` remain in the read-only v1 directory; no failed object was used.
The failed source is 5,908 bytes with SHA-256
`f07e5f9b6bae1902a04f3234db19a01bb37fd2dfc56f232f40cf60e25abe9a53`;
its 239-byte log has SHA-256
`6f55b566358aa64fd36e5e6c0dc9b82ad5c105defcc96222f96cf28cea66e8c2`.
The v1 command was the command below with `v1` substituted in `LEAN_PATH` and
as its current directory.

The following genuine command ran in the fresh
`/tmp/lean4lean-shape42-subsumption-v2` directory with pinned Lean 4.24, one
thread, and the granted lease. It returned exit status 0 with no diagnostics:

```sh
LEAN_PATH=/tmp/lean4lean-shape42-subsumption-v2 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/SubsumptionShape.olean Lean4LeanLevel42/SubsumptionShape.lean
```

An independently handwritten `Audit.lean` imports the genuine module and runs
`#print axioms` on every one of its 14 public theorem roots. This command also
returned exit status 0:

```sh
LEAN_PATH=/tmp/lean4lean-shape42-subsumption-v2 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 Audit.lean
```

Every reported axiom belongs to the unchanged standard-three policy:
`propext`, `Classical.choice`, and `Quot.sound`. The subsumeVars and node
pruning theorems use only `propext` and `Quot.sound`. This direct root audit supplements
the coordinator's genuine projection/alias acceptance; it does not replace it.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `SubsumptionShape.lean` | 5919 | `4495154ae222d0e511ebb51be729bcd863fda1dc84411ee96e274bd2084321fa` |
| `SubsumptionShape.olean` | 112640 | `cfaf196ad952ebd084d5db296c1d38c9705dbe77a886b9e70b4ecf523dc078fc` |
| `Audit.lean` | 916 | `36425ae72793aa34020a35fdf2331be5d1ee02d5cbf53e38d7db1d6ad59ee28a` |
| `audit.log` | 1460 | `6206a01f4d13ad81cd4599c6b18149a5ec0c0bdb78c89319047e45323d5b7ac1` |

The final root's 51-record dependency-and-module `subsumption-identities.json`
has SHA-256
`f3ac58185eff57117ff63d6097ebd4b202cd8bf3028ca1eb6bd58a3f7acfcfa1`.
Both successful and failed roots are read-only. Compiler lease 1 was explicitly
released after the successful compile, root audit, and post-build identity
checks. No Cargo, push, or PR command ran in this lane. All authored helper
names are public ASCII; there are no project axioms or unchecked proof escapes.
The own commit contains only the new module and this report.
