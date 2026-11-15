# Actual map-any lookup reflection (#529)

`MapAny.lean` imports only `Lean4LeanLevel42.MapValue`. It unfolds the actual
short-circuit `TreeMap.any` traversal, uses the actual
`Std.TreeMap.forIn_eq_forIn_toList` bridge, and then uses actual stored lookup
membership with comparator `List.compareLex Name.cmp`. It does not add a
replacement search implementation.

The six public roots are `list_forIn_any_eq`, `norm_any_eq_toList_any`,
`norm_any_eq_true_iff`, `norm_any_witness`, `norm_any_of_lookup`, and
`norm_any_congr_lookup`. The central statement is
`s.any f = true ↔ ∃ p n, s.get? p = some n ∧ f p n = true`.
The witness and reverse lemmas have implicit `s` and `f`; the reverse lemma
also has implicit `p` and `n`. Lookup congruence takes explicit `s t f` and
`h : ∀ p, s.get? p = t.get? p`. There are no sorting, NodeWF, map-shape,
physical-map equality, or supplied search-result premises.

## Genuine local validation

The compiler was `/home/agent/.local/share/vesper-lean/bin/lean`, version
4.24.0, commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, executable SHA256
`8fadc3ed92cc9decb94c96ee9c0876e91e03a3a5e69138f32114f02789e2766c`.
No Cargo command ran. Compiler lease 2 was explicitly released after these
compiles and probes, before this report/commit.

All 58 entries in
`/tmp/lean4lean-addability42-shared-v1/addability-dependency-identities.json`
were checked for both length and SHA256. Its SHA256 is
`021fd786e9b6f71a5dc9e62bb9f9c26928965386021d1f5c12bf660b7aa6111b`.
The 24 recorded project/vendor source paths matched this checkout. Complete
fresh copies were made at `/tmp/lean4lean-addability42-any-v1` and `-v2`;
only those copies were made writable. Both copies are now read-only. The
dependency identities were rechecked against both the original input and
the final copy after compilation; the final own source matches the checkout.

Commands, with `R=/tmp/lean4lean-addability42-any-v1` initially:

```sh
# cwd $R; empty diagnostics in /tmp/lean4lean-addability42-any-v1.log
LEAN_PATH=$R LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/MapAny.olean Lean4LeanLevel42/MapAny.lean

# Fixed handwritten six-root #print axioms file; exit 0.
LEAN_PATH=$R LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  /tmp/lean4lean-addability42-any-axioms-v1.lean
```

The first asynchronous compile's completion reply was truncated by the tool
context boundary. A later poll reported an already-closed process ID. Its
original genuine object and empty diagnostic log remain preserved. An
explicit confirmation compile ran successfully from the v1 working directory
with `LEAN_PATH` set to v2 (log `-v2.log`), producing the same object hash.
The final confirmation used both the v2 working directory and complete v2
import root, and exited 0:

```sh
# cwd /tmp/lean4lean-addability42-any-v2
LEAN_PATH=/tmp/lean4lean-addability42-any-v2 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/MapAny.olean Lean4LeanLevel42/MapAny.lean
LEAN_PATH=/tmp/lean4lean-addability42-any-v2 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  /tmp/lean4lean-addability42-any-axioms-v1.lean
```

There were no Lean elaboration failures or source revisions. The final
compile transcript `/tmp/lean4lean-addability42-any-v2-compile.log` is empty,
SHA256 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.
Both earlier compile logs have the same empty-file hash.

The fixed probe source SHA256 is
`50dc59334a504861fc483457568e78784a20a77c1bc21680d68434302d3fb6de`.
Both probe transcripts have SHA256
`7e514be73a887de65f8101c36df5eea83af4cf97ef7e0391dc20935d5ecea880`;
the final one is `/tmp/lean4lean-addability42-any-v2-axioms.log`.
`list_forIn_any_eq` reports `[propext, Quot.sound]`; each of the five
`norm_any_*` roots reports `[propext, Classical.choice, Quot.sound]`.
These are local dependency probes, not a substitute for the integration
packet's genuine validator matrices.

## Frozen artifacts and limits

Final complete root: `/tmp/lean4lean-addability42-any-v2`.
Its `any-identities.json` records all 61 pre-manifest files, including the
inherited dependency manifest and every inherited source/object/companion.
Manifest SHA256:
`c3ac4079d95729d3ee853854bf8b2e4e8a50fc4e9eca740828bc2ac0fc2f5681`.
Own source SHA256:
`31ada5aa48e2b0a130f44472f18c8050bdc6c7a5c7903897fff24b288bee8c79`.
Own genuine object SHA256:
`54b2a3613b5f93e53747a45825a94b2224002057bd26195823452d4d09e6aea8`.
The v1 and v2 source/object hashes agree. `git diff --check` passed.

This lane establishes actual any/lookup reflection and congruence. It makes
no canonicality, feasible-completeness, normalized-reification, opaque
Level.beq reflection, whole-checker/native equivalence, or source-campaign
claim. No parent/helper/vendor/runtime/auditor/CI files were changed.
