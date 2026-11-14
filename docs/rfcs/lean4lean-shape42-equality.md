# Actual normalized-map equality (#500)

`lean/Lean4LeanLevel42/MapEquality.lean` proves reflection for the actual
transparent comparisons in the pinned `Lean4Lean.Level` implementation.
`varNode_beq_iff_eq` reduces the actual derived comparison to its Name and Nat
fields. `node_beq_iff_eq` reduces the actual explicit comparison to its Nat
constant and complete VarNode list. Public LawfulBEq proof helpers are used as
local instances inside proofs; no global instance is introduced.

`norm_beq_iff_lookup_eq` unfolds the actual two-direction `all`/`get?`
comparison and applies the existing genuine `norm_all_eq_true_iff` bridge.
Option Node comparison reflects structural equality. For absent keys, the
reverse traversal rules out an entry present only on the other side. This
establishes equality of every actual lookup without asserting physical
TreeMap equality or depending on its balanced-tree representation.

`norm_lookup_eq_mapValue` derives denotation equality from lookup equality
using the two directions of the existing actual-map monotonicity theorem.
`norm_beq_mapValue` composes these results. All four main APIs have no MapWF,
key-ordering or variable-ordering premise. Malformed nodes, empty rows and
arbitrary actual maps remain within their statements. The module imports only
`Lean4LeanLevel42.MapAll`, whose proof closure uses MapValue and NameOrder;
it imports neither MapComparison nor CompareFold nor an old aggregator.

The eight public authored theorem names are `varNode_beq_iff_eq`,
`varNode_lawfulBEq`, `node_beq_iff_eq`, `node_lawfulBEq`,
`nodeOption_beq_iff_eq`, `norm_beq_iff_lookup_eq`,
`norm_lookup_eq_mapValue` and `norm_beq_mapValue`.

This lane does not bridge opaque `Lean.Level.beq`, does not establish actual
`isEquiv'` soundness, and makes no completeness, canonicality, reification or
whole-checker/compiler claim. Only the new Lean module and this report are
owned; no existing source or dependency is changed.

Validation used the genuine pinned Lean 4.24.0 binary
`/home/agent/.local/share/vesper-lean/bin/lean`, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, with `-j1 -M4096` and
`LEAN_NUM_THREADS=1`. The preliminary root was a private complete copy of
`/tmp/lean4lean-shape42-shared-v3`; all 48 dependency records were checked for
exact bytes and SHA-256 before copying. The initial handwritten `Probe.lean`
printed the three actual BEq instances, then reported unknown constants for
two guessed `#check` names (`List.beq_eq_true_iff` and
`Option.beq_eq_true_iff`). That failed lookup probe is retained at
`/tmp/lean4lean-shape42-equality-v1/probe.log`. The proof instead uses the
actual existing generic `beq_iff_eq` with locally proved lawful instances.
The first proof-module compilation succeeded in 0.873s and its direct
original-object axiom audit succeeded in 1.357s. Their complete command,
exit and duration records remain in `compile-1.json`/`audit-1.json`, alongside
their logs. No proof compilation failed.

After the coordinator narrowed the shared Shape import closure, root commit
`3872706` was incorporated as `dbfc29a`. Final validation copied the ENTIRE
`/tmp/lean4lean-shape42-shared-v4` into a fresh private versioned root,
unsealed only that copy, and added the unchanged owned source. All 49 records
in the authoritative `shape-dependency-identities.json` were verified before
copying and again after the final compilation. The dependency manifest
SHA-256 is
`3d413232882773a770b9501cb9a7466b83c964fd59c1edf1d5e3f631d6cc7ece`.
Neither shared root nor any existing dependency was modified. No namespace
prefix roots were layered.

From `/tmp/lean4lean-shape42-equality-v2`, the exact successful commands were:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-shape42-equality-v2 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/MapEquality.olean Lean4LeanLevel42/MapEquality.lean
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-shape42-equality-v2 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 EqualityAudit.lean
```

The final compilation exited zero without diagnostics in 0.814s. The direct
original-object `#print axioms` audit of all eight authored roots exited zero
in 1.119s. The first five roots use only `propext` and `Quot.sound`; the last
three use only `propext`, `Classical.choice` and `Quot.sound`. No project axiom,
admission, unsafe/partial definition or native decision procedure was added.
The coordinator owns the complete fixed-packet projection/alias audit and
integrated acceptance.

The final private root is sealed read-only. Its `equality-identities.json`
contains all 49 unchanged dependency records plus the two genuine new
source/object records; its SHA-256 is
`2b18cf82aba9771f9d717d69e468da462954606292ab9b11a16f0314dcb83645`.
The new source is 2915 bytes, SHA-256
`d02cafa6738fad2f5738a2dc7a35a5361bfc796ac9886a8a40d16d3ac84b69a1`;
the genuine original object is 81720 bytes, SHA-256
`70eb8418ac48f67e9e841f527d779e3371b05977f71e116fcf3673168efbd33b`.
`compile-2.json`/`audit-2.json` and their logs retain the final commands,
identity-bound environment, exits and durations. No Cargo command ran.
Lean compiler lease 1 was explicitly released after publication so the next
lane could compile. No push or PR was performed in this lane.
