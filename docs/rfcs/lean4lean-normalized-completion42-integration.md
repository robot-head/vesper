# Normalized completion integration (#555)

The new theorems connect the existing construction and pruning certificates:
every condition path retained by the actual complete `Normalize.normalize`
has a completion order, and its rows satisfy `MapWF`. Both statements apply
to every `Lean.Level`, without input restrictions. The sources add two
theorems and an imports-only final module; they define no new algorithm.

The tested source is `f43763d8772da7a8cfc140495b1caab68e347578`, based on
own reconstruction PR #553 at `3ed3984a66738e444484b3ced350470c07cbfe3f`.
The final documentation update follows that frozen source. Public issue #555
claimed the exact paths after review of all 83 open issue bodies and all
28 open PRs' complete filename lists. No external ownership conflict was found.

## Actual execution

Pinned local Bazel 9.3.1 built and ran
`//:test/lean4lean_normalized_completion42` using actual Lean 4.24.0:

- Build: 59.851 seconds, with 2,333 disk-cache hits.
- Uncached test command: 256.954 seconds. The Rust test ran for 208.11
  seconds: one passed, zero failed or ignored.
- All 197 authored theorem roots, including both new endpoints, were audited
  across the exact 21-module source closure. There are no proof aliases.
- The snapshot records 660 original declaration occurrences, 649 distinct
  declarations, 633 canonical retained declarations and 16 matched old
  implementation helpers. The duplicate inventory remains checked. Neither
  new module introduces a helper omission.
- Exact inventory, constant information, kernel replay, verified reimport,
  empty IR and empty extension checks pass. The unchanged genuine negative
  controls cover changed sources, missing/corrupt objects and forbidden
  alternate proof aliases.
- The projected object is 8,345,768 bytes, BLAKE3
  `b40421b774217626addf4997a90a4343b06ac2eb4e5f6f7ec6907a0a0b630de5`.

The complete stdout, XML, evidence, auditor commands and executable identities
are retained with `/tmp/vesper-bazel-normalized-completion-v1-receipt.json`.
Its SHA-256 is
`af7fae671ee5faf23e3733f2f7c602b8ad13051177c7fc03c9d6e64cce01ed50`.
An audit's `auditor_blake3` identifies the included `Audit.lean` source;
the receipt separately identifies the actual CLI executable.
The CLI SHA-256 is
`6aef5208260f57c715fd879a69de027f43a7fa4670e69b742de969be53c63542`;
the actual test executable SHA-256 is
`6f5d74daa3e3ae5fb1f929c47e4c3205664d7a7d9b2797f0599417928d87ab19`.

## Scope and preservation

Strict Rust 1.100 Clippy passes for all twelve exact proof-test sources through
a minimal package with genuine dependencies, in 2.536 seconds. This is a
scoped lint check, not a whole-workspace build or a runtime test. The initial
minimal-package setup omitted the compile-time CLI variable; the retained
retry binds the genuine archived CLI without executing it.

Rustfmt, BUILD synchronization and whitespace checks pass. All eleven earlier
packet specifications and catalogs, 64 earlier proof/vendor/test source files,
and the entire 47,588-byte validator suffix remain byte-identical. The suffix
SHA-256 is `2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
The source cap, axiom policy, timeouts and controls are unchanged. Previous
packet runtime results retain their original source identities; those eleven
matrices were not rerun for this additive extension.

The scoped lint/static receipt is
`/tmp/lean4lean-normalized-completion42-static-v1/final-validation-f43763d.json`.
The [composition report](lean4lean-normalized-completion42-composition.md)
records the independent review and original declaration checks.

This establishes normalized-map completion and well-formedness. It does not
establish map-to-tree or normalized-reification denotation, canonicality,
whole-checker correctness, native equivalence or source-campaign completion.
Hosted checks remain separate from these local results. This child PR targets
the reconstruction branch and must not auto-merge into that unprotected base.
