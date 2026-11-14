# Actual ordered insertion proofs (#409)

This lane adds only `Lean4LeanLevel42/OrderedInsert.lean` and this report under
the pushed `05aaeac` construction contract. It imports frozen ListProofs and
its Semantics dependency and calls the actual pinned
`Lean.Level.Normalize.orderedInsert`; it defines no replacement algorithm.

The fourteen public theorems establish:

- Successful insertion permutes to the inserted element followed by the old
  list, and increases length by one, for any type and comparator.
- Exact Name membership, preservation of old members, and inserted membership.
- An actual `none` result implies membership using Name.cmp equality reflection;
  a nonmember therefore yields an actual `some` result.
- Output activation is exactly the inserted name's positivity conjoined with
  old activation; `none` with an enabled input establishes positivity.
- NodeWF transfers to the enlarged key. Input Nodup plus nonmembership implies
  output Nodup, again without a comparator/sorting assumption.

The computational counterexample inserts numeric Name zero into `[one, zero]`,
returns `[zero, one, zero]`, proves prior membership and proves output is not
Nodup. Thus it does not claim member-implies-none on unsorted input. Two further
kernel computations show an inserted zero-valued name disabling an enabled key,
and actual equality producing none on an enabled positive input.

These are construction/list facts. They do not prove sortedness, canonical-form
completeness, whole-map or whole-normalizer correctness, checker verification,
compiler refinement or native equivalence.

## Actual compilation and dependency associations

All eight parent source/object records in
`/tmp/lean4lean-foundations42-semantics-v2/identities.json` were verified before
use. The four dependency sources also byte-match their frozen worktree/vendor
files. An initial source-only search root refused the ListProofs import because
Lean selected its existing top-level module directory; this was not reported
as a pass. The source proof was unchanged when a distinct coherent root was
assembled with exact verified dependency copies.

The genuine compile in `/tmp/lean4lean-construction42-ordered-v2` exited zero,
with empty stdout/stderr, in 0.721 seconds measured by the command runner:

```text
LEAN_PATH=/tmp/lean4lean-construction42-ordered-v2:/home/agent/.local/share/vesper-lean/lib/lean
LEAN_NUM_THREADS=1
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/OrderedInsert.olean Lean4LeanLevel42/OrderedInsert.lean
```

All eight copied dependencies still matched afterward, and the new source
byte-matches the owned module. The immutable root's `identities.json` records
all ten files with BLAKE3 and SHA256 associations. New artifacts:

| File | Bytes | BLAKE3 |
| --- | ---: | --- |
| OrderedInsert.lean | 5,914 | 72502ba49a328eb39a17f5697351e595acb7b487ddf056c2fe01ae66053793df |
| OrderedInsert.olean | 154,768 | 60cf5d47326e74c694f661d364f3bd1eb1cdbeea2f7041878f024fd3d8fbbbaf |

No Cargo invocation or dependency/profile mutation occurred. The compiler lease
was released after the successful compile. Whitespace verification passed.
Complete-package projection, all-alias association, strict standard-axiom audit
and the shared parent/new acceptance matrices remain the integration gate;
this source lane does not claim those have executed.
