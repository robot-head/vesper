# Actual reconstruction congruence (#542)

The final packet replay and subsequent predicate-fold source/object identities
are recorded in [the integration report](lean4lean-completion42-integration.md).
The individual compilation identities below retain their original scope.

This lane owns only `Lean4LeanLevel42/ReifyCongruence.lean` and this report,
from base `3e0a98b`, after reading the frozen completion contract. The module
imports Addable and MapEquality. It adds no definitions or replacement
search/reconstruction algorithm.

The eleven public theorem roots are:

- `feasible_go_lookup_congr (s t fuel acc rem h)`;
- `feasible_lookup_congr (s t acc rem h)`;
- `lexChain_lookup_congr (s t fuel p h)`;
- `lexChain_perm (s fuel p)`;
- `norm_toList_lookup_congr (s t h)`;
- `norm_toTree_lookup_congr (s t h)`;
- `norm_reify_lookup_congr (s t h)`;
- `feasible_beq_congr (s t acc rem h)`;
- `lexChain_beq_congr (s t fuel p h)`;
- `norm_toTree_beq_congr (s t h)`;
- `norm_reify_beq_congr (s t h)`.

Lookup congruence takes `h : ∀ p, s.get? p = t.get? p`. Boolean-map congruence
takes `h : (s == t) = true` for the actual NormLevel instance. The conclusions
are equality of the actual feasible results, chain lists, reconstruction
trees, or reified Levels. They do not assume sortedness, WF, coverage,
completion, NoMVars, or the desired algorithm result.

The feasible/chain proofs follow the actual fuel recursion and `find?`
predicates, using existing actual addability congruence. Chain permutation
is unconditional for every fuel and raw list, including duplicates and the
actual fallback. In its successful branch, the selected element really
belongs to the input by `List.mem_of_find?_eq_some`; the recursive permutation
composes with `List.perm_cons_erase`. Fallback and zero fuel retain the list.

Ordered enumeration equality uses actual
`Std.TreeMap.Equiv.of_forall_constGet?_eq` followed by
`Std.TreeMap.Equiv.toList_eq`, explicitly instantiated with
`List.compareLex Name.cmp`. Reconstruction uses the actual
`Std.TreeMap.foldl_eq_foldl_toList`, equality of those ordered enumerations,
and equality of the actual reconstruction step, including lexChain,
subsumeVars, and Tree.modify. No equality of physical map trees is assumed or
derived. Reified Level equality follows by applying actual Tree.reify to the
proved reconstruction equality. No opaque native Level.beq bridge is used.

## Genuine compiler and dependency evidence

Pinned executable `/home/agent/.local/share/vesper-lean/bin/lean` reports
4.24.0, commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, executable SHA256
`8fadc3ed92cc9decb94c96ee9c0876e91e03a3a5e69138f32114f02789e2766c`.
Each compile/probe used the granted second Lean lease, one complete private
LEAN_PATH, `LEAN_NUM_THREADS=1`, `-j1 -M4096`. No Cargo command ran. The second
Lean lease was explicitly released after final checks, before this commit.

The complete shared root `/tmp/lean4lean-addability42-shared-v1` was copied,
and only the private copies were unsealed. Its 58-record
`addability-dependency-identities.json` SHA256 is
`021fd786e9b6f71a5dc9e62bb9f9c26928965386021d1f5c12bf660b7aa6111b`.
All records were verified by length and SHA256, and all corresponding
project/vendor source bytes matched this worktree.

The genuine parent integration manifest
`/tmp/lean4lean-addability42-integration-v1/integration-identities.json`
has 69 records and SHA256
`f50399362cf5277cfb9151b3fc530d00fc920c482d61f85f9e67261726017b7a`.
Every record was checked. Only the needed Addable, SortedSubset, and MapAny
source/object pairs were added to the complete shared copy; their source
bytes matched this worktree. The original integration manifest is retained.
No dependency object was recompiled or changed.

Each private version has the same 66-record
`congruence-input-identities.json`, SHA256
`d7097872e80500b48ed54750514ec1347afc24722cc60beaa106a652cc255906`.
All private inputs and both original manifests were verified again after the
successful compile and audit. The final own source matches the checkout.

The exact compile command below ran separately with N=1, N=2, and N=3, from
the corresponding complete private directory. N=1 and N=2 exited 1; N=3
exited 0 with empty diagnostics. Each retained `compiler-result.json` records
the command, directory, environment, and exit status.

```sh
# cwd /tmp/lean4lean-completion42-congruence-vN
LEAN_PATH=/tmp/lean4lean-completion42-congruence-vN LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/ReifyCongruence.olean Lean4LeanLevel42/ReifyCongruence.lean
```

V1 needed an explicit comparator on ordered enumeration equality and Tree
type annotations on fold-step lambdas. V2 then needed `congrArg` for the
final fold function equality instead of rewriting the elaborated lambda.
These were elaboration fixes; no theorem premise or conclusion changed.
Failed objects were not consumed. The immutable failed source/log identities
are:

| Attempt | Source SHA256 | Compiler log SHA256 |
| --- | --- | --- |
| v1 | `f25e5a8efb369a8e65b773cd9de68925754d16cdcc62e2deb36af1a324854ca9` | `d412b22234fa1182dd01a673e8c59831dbade58b346c65013a0764f6ef442e8d` |
| v2 | `5ce643d88141fc6ff7cd06245d70199bf5778808539213a918076a486854655d` | `1a7062db2f3a770963414add2f7b107e41fb1dba9172beaf8a16cd4d2109389b` |

The fixed handwritten `CongruenceAudit.lean` imports the actual genuine
module and prints axioms for all eleven public roots. From v3, this command
exited 0, and every root reported exactly
`[propext, Classical.choice, Quot.sound]`:

```sh
LEAN_PATH=/tmp/lean4lean-completion42-congruence-v3 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 CongruenceAudit.lean
```

## Frozen artifact identities and scope

All three private roots are read-only. Final complete root:
`/tmp/lean4lean-completion42-congruence-v3`.
Its `congruence-identities.json` records all 75 pre-manifest files, including
all sources/objects/companions, original manifests, command results, toolchain
identity, and probe input/output. Manifest SHA256:
`06e8eb3272ed9318edf5352eb871177d9e64c649f9943f51fe8a0e1ac696cee8`.

| Final artifact | Bytes | SHA256 |
| --- | ---: | --- |
| ReifyCongruence.lean | 4500 | `def0295aa432920f1c4b40729af6939a8d024a4b4a241fa921d398027808f067` |
| ReifyCongruence.olean | 136864 | `4e8542e48337b7d2f03f43130c87a4eab8471e4a68e68846cb2331ced94a52d3` |
| compiler.log | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| CongruenceAudit.lean | 696 | `8fd69678ad876b76adb67d8f18ce3606ab61c91dc8445a8380eb7a973fdc58d6` |
| audit.log | 1173 | `6e58e5f61a7f6a119bf95436adb25671a98af1ef0e356ed5f3ce7adb0fe2acea` |

`git diff --check` passed. This local genuine compile/probe does not claim the
coordinator's projected replay or complete validator matrices, which remain
integration work. Completion/admissibility, lexicographic minimality,
fallback unreachability on normalized maps, semantic reconstruction,
canonicality, whole-checker/compiler/native equivalence, and source-campaign
completion remain separate. No parent, vendor, runtime, auditor, or CI source
was changed, and no push or PR action ran.
