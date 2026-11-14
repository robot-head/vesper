# Actual recursive construction completion (#548)

This lane owns only `ConstructionCompletion.lean` and this report. It imports
Completion, MapHelpers, LevelSemantics, and OrderedInsert. It does not import
subsumption, shape, priming, or normalization-invariant proof modules.

Each actual primitive map update preserves VarSupport using actual retained
variable witnesses. AddVar introduces no key, so core support/key-origin
transport preserves MapCompletes. AddConst can introduce its path and therefore
uses that path's independent completion certificate, transported through its
own support result.

For successful actual orderedInsert, the old path certificate is transported
to the actual addNode output. A same-name variable is actually stored at the
inserted key. Appending the inserted name after the old admissible order is
therefore certified: the new key is contained in that name plus the reversed
old order. The actual insertion permutation identifies the resulting order
with the new path. No sorting, nodup, offset-zero, or nonmembership assumption
is needed.

The main paired theorem follows actual normalizeAux branches using levelSize
induction. Sequential branches transport the independent current-path
certificate through the preceding support result, then compose support. The
fresh imax-over-max and nested-imax rewrites retain their actual recursive
calls. Both metavariable branches remain valid inert updates; NoMVars is not
needed for these combinatorial properties. Independent seed certificates and
the empty-map/path completion corollary follow by core certificate transport.

The twelve public theorem names are `map_addConst_varSupport`,
`map_addNode_varSupport`, `map_addVar_varSupport`, `map_addVar_key_origin`,
`map_addVar_completion`, `map_addConst_completion`,
`construction_admissible_snoc`, `addNode_inserted_path_completes`,
`map_addNode_completion`, `normalizeAux_completion`,
`normalizeAux_preserves_completes`, and `normalizeAux_empty_completion`.

The private roots give first priority to the entire immutable
`/tmp/lean4lean-completion42-fold-v2` snapshot: all 50 records of its
`fold-identities.json` were verified, whose SHA-256 is
`c1661137e6c549444335b171a4e7e795beb13217a3ee9d4b93ff33acbeeae249`.
The only missing modules, MapHelpers and LevelSemantics, were copied as genuine
source/object pairs from `/tmp/lean4lean-addability42-shared-v1`. All 58 records
of its manifest were verified (SHA-256
`021fd786e9b6f71a5dc9e62bb9f9c26928965386021d1f5c12bf660b7aa6111b`).
All overlapping module sources/objects matched byte for byte. Thus no newer
Completion or other priority object was overwritten by an older dependency.

Every attempt copied the whole priority root, unsealed only the copy, and
merged those four missing files. Original manifests and explicit object origins
are retained. Each compile used one complete project `LEAN_PATH`. All 58
private input identities were verified before and after compilation; their
`construction-input-identities.json` SHA-256 is
`7f086350e44433acaf23bf18f41a1f97aaf9b4b67bc0ddaeac2c80e16af6449c`.
Completion, MapValue, MapHelpers, LevelSemantics, OrderedInsert, and the authored
source matched the working tree. No dependency object was recompiled here.

The coordinator granted lease 1, temporarily reassigned it to the tree-value
lane while this draft was being written, then explicitly regranted it for
compilation. The successful command ran in
`/tmp/lean4lean-reconstruction42-construction-v2`, with pinned Lean 4.24.0,
one thread, exit status 0, and no diagnostics:

```sh
LEAN_PATH=/tmp/lean4lean-reconstruction42-construction-v2 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/ConstructionCompletion.olean Lean4LeanLevel42/ConstructionCompletion.lean
```

The previous v1 command substituted `v1` in `LEAN_PATH` and as the current
directory. It returned exit status 1: at `45:26`, the lookup case split had
already rewritten an existential lookup to `some m = some m`, requiring
reflexivity; at `66:38`, equality destruction had replaced and cleared the
path binder, so the surviving equal binder was used. These were local proof
elaboration corrections. All theorem statements and the recursive argument
remained unchanged. The failed source, full log, exact command and output-chunk
record remain preserved; no failed object was consumed.

The handwritten audit used this command in the genuine v2 root:

```sh
LEAN_PATH=/tmp/lean4lean-reconstruction42-construction-v2 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 ConstructionCompletionAudit.lean
```

All twelve public-root axiom probes passed the unchanged standard-three set:
`propext`, `Classical.choice`, and `Quot.sound`. Original module ownership was
also inspected for all 37 actual declarations, including generated aliases and
private source-equation helpers. Every declaration is safe, nonpartial, and
nonaxiomatic; no new `_unsafe_rec` exists. The actual original kinds comprise
36 theorems and one safe source-equation splitter definition. A further
`collectAxioms` check passed standard-three for every one of those 37 actual
declarations, without a project-helper omission.

The first audit succeeded with a deprecation warning for `isTheorem`. Its
script/log are retained. The next used `getOriginalConstKind?` without warnings;
the final audit additionally checked all 37 declarations' axioms. Final probe
and inventory exit status is 0 with no diagnostics. This supplements the
coordinator's genuine original/projected root and alias checks.

The publication root is the read-only
`/tmp/lean4lean-reconstruction42-construction-v3`. It contains byte-identical
source, object, final audit and inventory from the genuine v2 compilation,
with explicit `source-object-origins.json`. It corrects only the two historical
audit metadata records to name the actual command filename, while separately
identifying their retained versioned script copies. The original v2 manifest
is retained. No new compiler invocation or proof change occurred in this
publication copy.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `ConstructionCompletion.lean` | 8996 | `e8f0250544a427ff1e3f85ed06fec1770421abcd15a9e4210c70bece43683ee8` |
| `ConstructionCompletion.olean` | 422584 | `89e20c513f3cce2b0142fb1a85c8879e3dafc9f6c7f2c4d950201d3eb2f896ae` |
| `ConstructionCompletionAudit.lean` | 1721 | `763f181bfaa3f4efeba34171be474fde774eded8129d9bd6b8670c901d638513` |
| `audit.log` | 10464 | `9b3da2f1ff001c9e41146855632e6e9c1ecaef5c7ad9a540118b6e111173e584` |
| `original-declarations.json` | 5775 | `720a3d2443e1a83b628e2126d6ebd46873cec6019255083f5af4c07c86b7f5ad` |
| failed v1 source | 8995 | `d0b86c18d00cf1f10efd69d16077817986781faf9ffef1545bda5540b0b94a33` |
| failed v1 `compiler.log` | 337 | `068075752e59760aad70f6d8109e0eabce89c7f3ba08022319cfc2e3f84a53ed` |

The complete 75-file publication `construction-identities.json` has SHA-256
`78bddc83d9d96af22cc8c3bdbf636f6d7295f2131095aed2eebf5bd0736d8120`.
All source/object and tool-record versions are preserved read-only. Lease 1
was explicitly released after the final probes. No Cargo, push, or PR command
ran in this lane. Only this report and the new proof module enter the own
commit. Subsumption composition, map-to-tree denotation, normalized reification,
minimality/canonicality, opaque Level equality and whole-checker/native
equivalence remain outside this lane.
