# Actual flat tree-entry structure (#551)

Implemented only `Lean4LeanLevel42.TreeStructure` and this report, from
`ed18ce23b06d3ffe0fe8a1a3eb1f605e043945ac` after reading the frozen
reconstruction contract. The sole import is SortedKeys, which already supplies
NameOrder. No extra module is added to the frozen combined 24-module closure.
Existing compiler/helper sources are unchanged.

The single new definition is nonrecursive and generic:

```lean
treeEntriesLookup n xs := (xs.find? (fun row => row.1 == n)).map Prod.snd
```

It observes the actual first matching child entry. There is no replacement
updater, recursive TreeWF predicate, supplied search-result flag, or denotation
claim. All update theorems concern unchanged `Lean.Level.Normalize.modifyAt`.

## Seven actual public roots

- `treeEntriesLookup_cons`: first-entry lookup is `if m=n then some v else
  treeEntriesLookup n xs`.
- `treeEntriesLookup_none_of_not_mem`: absence from the actual key projection
  implies lookup none, without sorting or distinctness.
- `modifyAt_keys`: for arbitrary lists and generic Inhabited values, the actual
  updated key projection equals actual orderedInsert Name.cmp on the old keys,
  using `.getD oldKeys` on none. No sorting premise.
- `modifyAt_lookup_other`: if m differs from n, updating n preserves the first
  lookup of m, even for raw unsorted or duplicate lists.
- `modifyAt_keysSorted`: strictly sorted old key projection implies strictly
  sorted updated key projection, through actual ordered insertion.
- `modifyAt_lookup_target`: under strict sorted keys, target lookup equals
  `some (f ((treeEntriesLookup n xs).getD default))`. The same Inhabited instance
  supplies the actual updater's default. Both absent/present target cases are
  covered; no successful insertion or freshness premise is assumed.
- `modifyAt_unsorted_target_witness`: with numeric z=0, o=1, xs=[(o,1),(z,2)]
  and f=Nat.succ, old lookup z is some2, actual modifyAt returns
  [(z,1),(o,1),(z,2)], and new first lookup is some1 rather than some3.
  Keys are explicitly not sorted. Thus an unconditional target-lookup theorem
  is false, despite that original unsorted list having distinct names.

The proof follows each actual comparator branch. In the target lookup lt
case, strict order proves the old target absent from the entire list; the eq
case updates its actual first value; the gt case recurses through an unchanged
head. No whole recursive tree structure or NormLevel.toTree result is claimed.

## Genuine original objects and probes

Final complete private root:
`/tmp/lean4lean-reconstruction42-tree-structure-v3`.

Each fresh attempt copied the entire immutable parent
`/tmp/lean4lean-completion42-fold-v2` and unsealed only its private copy. All
50 current records from the parent's fold-identities manifest were verified
before each attempt and after the final probes, including the corrected
Completion source SHA256
`f89ca02776f406b633af9411aae7d0875e4067d927f0629c5d8f81025a678505`.
Every corresponding project/upstream source matched this checkout. The older
completion integration manifest remains historical provenance; the current
50-record fold manifest is the input authority. No dependency object was
recompiled or changed.

Pinned Lean 4.24.0 commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b` was used under granted compiler
lease 2, with one thread, `-M4096`, and 180-second invocation deadlines.
The final exact commands, from the private root, were:

```text
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/TreeStructure.olean Lean4LeanLevel42/TreeStructure.lean
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 TreeStructureAudit.lean
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 TreeStructureInventory.lean
```

Environment: `PATH=/usr/bin:/bin`, `LANG=C.UTF-8`, `LEAN_NUM_THREADS=1`,
`LEAN_PATH=/tmp/lean4lean-reconstruction42-tree-structure-v3:/home/agent/.local/share/vesper-lean/lib/lean`.

Final compilation exited zero in 0.905 seconds with empty diagnostics.
The genuine original-object seven-root axiom probe exited zero in 0.655
seconds. Permitted axiom sets were:

| Roots | Actual axiom set |
| --- | --- |
| modifyAt_keys | propext |
| lookup cons/absence and unsorted witness | propext, Quot.sound |
| other lookup, sorted keys, target lookup | propext, Classical.choice, Quot.sound |

The actual imported-module inventory exited zero in 3.999 seconds: 15
module declarations, zero unsafe and zero partial declarations. All safe
lazily generated modifyAt equations/splitter helpers are retained; no new
implementation-helper omission is needed. The inventory tool's deprecated
isTheorem diagnostic flag produced one warning; safety checks use actual
isUnsafe/isPartial metadata and are unaffected. The source and all seven
actual theorem roots are explicitly recorded. No axiom, proof admission,
unsafe/partial definition or native_decide was added to the proof source.

## Exact frozen identities and retained attempts

| Artifact | Bytes | SHA256 | BLAKE3 |
| --- | ---: | --- | --- |
| TreeStructure.lean | 5427 | 45041a0d4c5eabcb78ca01d415fabd19efd1811eb71ebc43ed6c6267860dd53a | 2818d0220c73a769f615a5cd46003b8c28635fedefb3b7d838863bf5060411c4 |
| TreeStructure.olean | 197784 | d1c19b0f0e554cb5981a7c1b312b7db89d62824eae417972d0ce01826afea1fa | 5f68d5fed74a0439a505dfe6e57c780ebe762bcc6b78a4437db07d413a437716 |
| tree-structure-identities.json | 11815 | 0fdf59df0827ae57e2db8179662d88b2d1d5697182f6ab320a7cbead4660ec83 | 9ffbe8a600e83c85744cf885255608b65fdf4f9705c7167f20f6a976928ee49a |
| tree-structure-manifest.json | 18771 | 2c9fe68d979b872f1e3eb7248b441a52531dab900300c2e205328970b773b9e3 | 973f4f66ea68ffcaca13ee3c4d577e2078126e08c761db4b492b139eb716b7ff |

The identities file has 52 source/object records: the unchanged 50 plus this
module's original source and object. The full manifest additionally records
compiler/toolchain, all probe inputs/outputs, exact command outcomes, public
root axiom sets and historical attempts. All private roots are read-only.

V1 genuinely failed because the open namespaces omitted Lean.Level, leaving
Normalize names unresolved; its full source/log/result are retained. V2
compiled all seven roots and passed their audit, with two unused simp-argument
warnings. V3 removed those arguments and recompiled cleanly. No theorem
premise or intended conclusion changed. Fresh roots prevented consuming a
failed object. Build/probe commands and all inputs are preserved in final
history alongside the original v1/v2 roots.

`git diff --check` passed. No Cargo, foreign file edit, push or PR action was
performed. Compiler lease 2 is explicitly released and no compiler process
from this lane remains. Parent-owned full kernel packet, eleven matrices and
Bazel validation remain separate; none is claimed passed by this report.
