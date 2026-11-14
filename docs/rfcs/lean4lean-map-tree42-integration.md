# Raw map-to-tree denotation integration (#557)

The actual raw-map reconstruction now has a proved numeric denotation:

```lean
theorem norm_toTree_reify_value (ρ : Valuation) (s : NormLevel)
    (hKeys : ∀ p n, s.get? p = some n → KeySorted p)
    (hCompletes : MapCompletes s) :
    levelValue ρ s.toTree.reify = mapValue ρ s
```

The proof uses the unchanged `NormLevel.toTree`, `Tree.modify` and
`Tree.reify`. The two independent premises suffice; no additional MapWF,
variable sorting, nonempty-row or NoMVars premise is needed. Nonrecursive
observers expose actual paths/payloads and child ordering. The original-map
fold maintains its global upper bound, processed-row lower bound and zero
payload at pending distinct targets. It assumes neither target freshness
nor the false equality with a processed-prefix map value.

The tested source is `e8a2ec48ddafc5b071e8efdae410a635a5b0e22a`, based on
own PR #556 at `2eaea8909c4bfa4e76344608eb4b128375aed2ec`. Subsequent changes
only record documentation. Public issues #557–560 preceded source edits after
review of all 82 open issue bodies and all 27 open PRs' complete filenames.
Both proof lanes compiled independently and passed original declaration,
axiom and safety checks before the integrated replay.

## Genuine fixed-packet execution

Pinned local Bazel 9.3.1 built and executed
`//:test/lean4lean_map_tree42` with actual Lean 4.24.0:

- Build: 58.685 seconds, with 2,333 disk-cache hits.
- Uncached test command: 226.533 seconds. The Rust harness ran 181.31 seconds:
  one passed, zero failed or ignored, within the unchanged 300-second budget.
- All 255 authored roots passed strict audit in batches of 128 and 127:
  203 existing roots and 52 new roots across the exact 24-module closure.
- The snapshot contains 772 occurrences, 770 distinct declarations and 754
  canonical retained declarations, with two identical duplicate occurrences
  and 16 matched old helper omissions (13 vendor, three LevelSemantics).
  There are no new omissions or proof aliases.
- Exact inventory, original constant information, kernel replay, verified
  reimport, empty IR and empty extension checks pass. Every unchanged genuine
  corruption, changed-source and forbidden-alias control passes.
- Projection: 6,669,016 bytes, BLAKE3
  `7e2607bc679eeb9677bd13c4bdeca8b3a102aa8a1a3fa87ebe791bd18bc54e6d`.

Full stdout, XML, evidence, six auditor invocations and executable identities
are retained with `/tmp/vesper-bazel-map-tree-v1-receipt.json`, SHA-256
`fbb65044a68d1ba90104fe2af20767b28ca9b9db485704d2ab529dd3f19d36bc`.
Actual CLI SHA-256:
`6aef5208260f57c715fd879a69de027f43a7fa4670e69b742de969be53c63542`.
Actual test executable SHA-256:
`e64ec753a80d7411930be18e3ba1e62288f640dc9d5dbfc7294b2105bc95efcb`.
An audit's `auditor_blake3` binds the included Audit.lean source, separately
from the CLI executable identity.

## Preservation and limits

Scoped strict Rust 1.100 Clippy passes for all thirteen exact proof-test
sources through a minimal package with genuine dependencies, in 2.247 seconds.
Rustfmt, BUILD synchronization, whitespace and source-graph checks pass.
All twelve prior packet specifications/catalogs, 67 earlier selected sources
and the entire 47,588-byte validator suffix remain unchanged. Its SHA-256 is
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Earlier runtime results retain their original source identities; unchanged
older matrices were not rerun for this additive packet.

The scoped lint receipt is
`/tmp/lean4lean-map-tree42-static-v1/clippy-e8a2ec48-receipt.json`.
The [path proof](lean4lean-map-tree42-path-value.md) and
[map-fold proof](lean4lean-map-tree42-map-value.md) reports retain exact
original artifacts, safety inventories and failed elaboration attempts.

This theorem requires independently complete, sorted stored keys. Normalized
level composition remains separate, as do canonicality, whole-checker/native
equivalence and campaign completion. Hosted checks are a separate gate.
The PR targets own #556; auto-merge into its unprotected base is excluded.
