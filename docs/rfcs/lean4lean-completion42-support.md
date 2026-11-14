# Actual variable support through subsumption (#541)

This lane owns only `lean/Lean4LeanLevel42/VariableSupport.lean` and this
report. It imports Completion and MapSubsumption, with no MapHelpers or heavy
normalization/reification modules added to the actual dependency closure.
Every operation is the unchanged actual helper or the existing exact
minimizeEntry source-expression abbreviation.

Actual subsumeVars either retains an original variable record or supplies an
actual right-list record of the same name and at least its offset. The proof
follows each real comparator/offset branch without assuming either input list
is sorted. Node-level facts carry that witness through the constant test and
the actual subset wrapper; the latter also supplies the supporting key subset.

The fixed-map minimize fold preserves a variable or finds its same-name
witness in the actual map with the visited key erased. Self comparisons retain
variables regardless of the right node, while external comparisons supply
actual fixed-map lookups. Specialization uses the original actual toList and
Std's lookup/membership theorem. No semantic valuation, WF, or sorting premise
is required.

Both minimizeEntry whole-map support and key-origin helpers explicitly require
the actual current lookup of the original row. A retained minimized variable
makes its node syntactically nonempty, so the actual entry update inserts it;
an external erased-context witness keeps its outside-key lookup unchanged.
Every output key either is the visited, already-existing key or retains its
old lookup.

The outer fixed-source fold carries the current original-node lookup of every
remaining row. Actual distinct source keys and the outside-key lookup theorem
preserve this invariant after each visit. Support and key origin compose
simultaneously, then specialize to the unchanged actual subsumption fold.
Finally, MapCompletes preservation uses both independently proved support and
final-key origin via the frozen core transport theorem. Support alone is not
used to infer that newly introduced keys have completion certificates.

The isolated branch starts at frozen contract commit `3e0a98b`. The root-owned
core algebra commit `51410adaae202f20f8c1b16d6708c3df4f086a07` was explicitly
requested and cherry-picked as local dependency commit `37da17b`. The own
commit changes only this new module and report; Completion remains root-owned.

The coordinator granted a compiler slot after the congruence lane released
one. Each attempt verified all 63 records of the immutable
`/tmp/lean4lean-completion42-core-v3/completion-core-identities.json`, whose
SHA-256 is
`2b069f56969ece810e457ae393c32ab0de2c09fd12fd909bced5fdc0136436c7`.
Each then copied that entire root into a fresh private version and unsealed
only the copy. Every compile used one complete private `LEAN_PATH`, without
prefix-directory shadowing or fallback dependency roots.

The private 64-record `support-input-identities.json` includes the original
complete core manifest and has SHA-256
`a0583c10905066effde49a3d4ca3ca2304fe9a5a6309b88666a12f7f01e0901b`.
All byte counts and SHA-256 hashes were verified after copying and after the
successful compile/probe. Completion, MapSubsumption, MapMinimize and
NodeResidual source files matched their working-tree sources byte for byte,
as did the final authored source. No dependency source or object was changed
or recompiled in this lane.

The successful genuine command ran in
`/tmp/lean4lean-completion42-support-v3`, using pinned Lean 4.24.0, one thread,
and the granted slot. It returned exit status 0 with no diagnostics:

```sh
LEAN_PATH=/tmp/lean4lean-completion42-support-v3 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/VariableSupport.olean Lean4LeanLevel42/VariableSupport.lean
```

The two previous attempts used the same command with `v1` and `v2` respectively
substituted in `LEAN_PATH` and as the current directory. Both returned exit
status 1 and remain preserved:

- v1's retained-variable branch had rewritten node emptiness to false but had
  not reduced the actual `if false = true` expression before rewriting insert
  lookup (`134:10`). An explicit Boolean/if reduction fixed that proof step.
- v2 reached the actual insert lookup but reflexivity could not reduce the
  decidable test `if p = p` for an arbitrary name list (`136:6`). Explicit
  rewriting with `if_pos rfl` closed it. No theorem statement, hypothesis, or
  support argument changed between these attempts and v3.

| Retained failed artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| v1 `VariableSupport.lean` | 8915 | `ffeb8c2f18044e10278cd57bc68abbabacdf18ddf009377e2062c61f01ed94d0` |
| v1 `compiler.log` | 849 | `7acb842e5af3f21eb25ee486bc0b6430856282775abc0f600f834cc90bbc212a` |
| v2 `VariableSupport.lean` | 8966 | `3dfdfae05743b087ca0d83aadb33493db783745f4b867fe6ca3f2a6ac17fe7e3` |
| v2 `compiler.log` | 691 | `aa8a5b8d82fa24c1ff1faabb3a4c10e6ea4298cb85757e567c6374bc91a7ecf6` |

Each version retains `compiler-result.json` with the exact command, current
directory, exit status, output-chunk identity, and complete diagnostics. No
object from a failed attempt was consumed.

The independently handwritten `VariableSupportAudit.lean` imports the genuine
module and prints axioms for all twelve public authored theorem roots. This
command also returned exit status 0:

```sh
LEAN_PATH=/tmp/lean4lean-completion42-support-v3 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 VariableSupportAudit.lean
```

A post-check matched the twelve reported names exactly to the twelve public
theorem commands in the authored source and checked every reported axiom
against the unchanged standard-three set. The nonempty-node helper uses only
`propext`; all remaining roots use `propext`, `Classical.choice`, and
`Quot.sound`. This direct root probe supplements the coordinator's genuine
original/projected root and alias audits rather than replacing them.

| Final artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `VariableSupport.lean` | 8968 | `adaf206b3c22018629205dddebbbc11f140f4c2d95a808cf46cf124e7d165984` |
| `VariableSupport.olean` | 288864 | `6205803087e24b0de74de0c2f79c113f490097595094ffd579b99b15f77e878b` |
| `VariableSupportAudit.lean` | 798 | `bdfc93451d478bb3cd10acf66c9810c7c5a365983917b8fb8ff54c8302cadd85` |
| `audit.log` | 1291 | `1640f87b886db664666be1dd5f1d466b92a0c168eacd8a3c5d7cee0cee6b73c2` |

The final root's complete 72-file `support-identities.json` has SHA-256
`ef060205c1d3c576b0e1743065f248ca884ea9e6504dc516a22272f2b0cbd837`.
All three versioned roots are read-only. The granted compiler slot was
explicitly released after compilation, the twelve-root probe, and post-build
identity checks. No Cargo, push, or PR command ran in this lane.

These are variable-support, key-origin, and completion-preservation results.
They do not construct NormalizeAux completion certificates or prove actual
lexChain admissibility/minimality, tree denotation, normalized reification,
canonicality, opaque Level equality, or whole-checker/native equivalence.
