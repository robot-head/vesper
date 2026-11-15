# Actual addability witnesses and monotonicity (#530)

This lane owns only `lean/Lean4LeanLevel42/Addable.lean` and this report. It
imports the new SortedSubset and MapAny modules and the existing MapEquality
proof. The unchanged actual `NormLevel.addable` remains the sole tested
algorithm. No parent, vendor, runtime, native, kernel, profile, auditor,
extractor, Cargo, or workflow source is changed.

`addable_iff_witness` is unconditional: actual successful addability is exactly
an actual lookup with a stored variable of the requested name and arbitrary
offset, together with a successful actual subset test on the erased key. The
proof reflects the actual map `any`, variable-list `any`, Name equality, and
Boolean conjunction. The offset is never restricted to zero.

`addable_iff_subset` replaces that Boolean test with `p ⊆ a :: acc` when every
actual stored row key and the accumulator are strictly sorted. It uses actual
subset completeness, sortedness after erase, and the proved sorted-key erase
equivalence. It introduces no duplicate map-shape predicate and requires no
MapWF, sorted variable list, nonempty-row property, or NoMVars hypothesis.

`addable_mono_sorted_acc` needs sorted old and new accumulators, inclusion, and
actual old addability. Successful old matching supplies an erased-key sublist
of the old accumulator, so no map-key-sorting premise is needed.
`addable_mono_keys` instead requires sorted actual stored keys and the new
accumulator; the old accumulator may be arbitrary. `addable_orderedInsert`
uses the first form through actual ordered insertion/getD with only sorted
old accumulator and actual old addability.

Lookup congruence applies the new actual map-any congruence. Boolean-map
congruence reuses existing actual map-BEq reflection. Neither congruence
requires sorting, WF, or physical tree equality. No opaque Level-BEq bridge or
new axiom is introduced.

The numerical witnesses show success becoming failure when the target loses
sorting, success in an unsorted source key failing against a sorted larger
accumulator, and actual addability supplied solely by an offset-seven stored
variable with no zero-offset stored witness anywhere in the map. They execute
the actual addability test through its traversal bridge and actual source
subset equations.

The public `addable_eq_toList_any` helper is an exact traversal identity for
the actual source expression with its captured prefix-first Name comparator
written explicitly. It supplies no replacement implementation or success flag.
All eleven public authored theorem roots are included in the direct audit and
will also enter the coordinator's unchanged projection/alias audit.

The coordinator explicitly granted compiler lease 2 after MapAny released it.
Before every attempt, the full genuine dependency manifests were checked:

| Original root | Records | Manifest SHA-256 |
| --- | ---: | --- |
| `/tmp/lean4lean-addability42-shared-v1` | 58 | `021fd786e9b6f71a5dc9e62bb9f9c26928965386021d1f5c12bf660b7aa6111b` |
| `/tmp/lean4lean-addability42-subset-v3` | 60 | `84878fc77116fc2ca409328e9113ceac29bca8183323b25b63a1a09a06afae6b` |
| `/tmp/lean4lean-addability42-any-v2` | 61 | `c3ac4079d95729d3ee853854bf8b2e4e8a50fc4e9eca740828bc2ac0fc2f5681` |

Every byte count and SHA-256 entry was verified. All 58 shared records matched
both peer roots byte for byte. The peer source files matched their isolated
working trees, and existing MapEquality, SortedKeys, and MapValue sources
matched this lane's working tree. Each attempt copied the entire shared root
to a fresh private version, unsealed only that copy, then copied the genuine
peer source/object pairs and original complete peer manifests. One complete
private `LEAN_PATH` root prevented namespace shadowing.

The resulting 65-record `addable-input-identities.json` binds all private
dependency inputs, including both original peer manifests, with SHA-256
`733ec736f6600718a7246450df3d189123f6cffbf643e19c6203df39995da9db`.
All 65 records were verified after copying and again after the successful
compile and audit. The authored source matched the working tree byte for
byte. No dependency source or object was changed or recompiled in this lane.

The successful command ran in
`/tmp/lean4lean-addability42-addable-v3`, using pinned Lean 4.24.0, one thread,
and the granted lease. It returned exit status 0 with no diagnostics:

```sh
LEAN_PATH=/tmp/lean4lean-addability42-addable-v3 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/Addable.olean Lean4LeanLevel42/Addable.lean
```

The previous two attempts used the same command with `v1` and `v2` respectively
substituted in `LEAN_PATH` and as the current directory. Both returned exit
status 1, and both immutable source versions and full diagnostics remain:

- v1 used the propositional-equality theorem `Bool.and_eq_true` as an iff;
  `Bool.and_eq_true_iff` is the correct core theorem. Its witness proofs also
  needed explicit disjunction cases and the captured comparator exposed
  through the exact traversal identity. An unused `hi` simplification in the
  already-rewritten some/getD branch was removed.
- v2 left only the two negative numerical witnesses' closed Nat.compare
  expressions (`123:2` and `153:2`). Ordinary kernel-checked `decide` closes
  those finite arithmetic goals in v3. No native decision tactic is used.

| Retained failed artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| v1 `Addable.lean` | 7521 | `531a439dc53846aa087d1a4f7c6b2f19e9ca9e7c52575fd68bb3c111266a0d76` |
| v1 `compiler.log` | 4435 | `e93198df15cf58e2f238dc38f3793d794cda813c150701a421fbec28a431806c` |
| v2 `Addable.lean` | 8181 | `d414ebd59d1ea11ccbacdcbba1732dd50d8083c7ee2501047568915d497503aa` |
| v2 `compiler.log` | 801 | `b8c1ba3bae0c4f2bb15066cd88cd550a6b152b66bda1b00f758a528c5f9569d3` |

Each version also retains `compiler-result.json`, binding the exact command,
working directory, compiler exit status, tool-output chunk identity, and full
output. No failed object was consumed.

The independently handwritten `AddableAudit.lean` imports the genuine module
and prints axioms for all eleven public roots. This command returned exit
status 0; every root uses only the unchanged standard-three set `propext`,
`Classical.choice`, and `Quot.sound`:

```sh
LEAN_PATH=/tmp/lean4lean-addability42-addable-v3 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 AddableAudit.lean
```

| Final artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `Addable.lean` | 8203 | `4ac922a3092c9d20e35224e6e267d8e1057e170549cd12ac233e54b9be83dbc4` |
| `Addable.olean` | 244648 | `b9c7c1434cbd34d6b1e815a2741bd36cca99d3ff3eede6e08d186f521724534b` |
| `AddableAudit.lean` | 700 | `6a7a17753459e9151766b3b949446d5ebae151ff0d37d855e31a6ff5f0bcaf20` |
| `audit.log` | 1184 | `6662206f623b7797c27d88881b6fb079f93ad3b7604d1b2bd022551ef2007897` |

The final root's complete 72-file `addable-identities.json` has SHA-256
`2cf258ec5339550341991695dd2273439436b068cb39ed13afa705c2f39ceb81`.
All three versioned roots are read-only. Compiler lease 2 was explicitly
released after the successful compile, eleven-root probe, and post-build
identity checks. No Cargo, push, or PR command ran here. The own commit changes
only this report and the new module. Projected artifact identities and the
genuine acceptance matrices remain the coordinator's integration work.

These results prepare later greedy-completeness proofs. They make no claim
about feasible completeness, fallback unreachability, canonicality, normalized
reification, opaque Level equality, or whole-checker/native equivalence.
