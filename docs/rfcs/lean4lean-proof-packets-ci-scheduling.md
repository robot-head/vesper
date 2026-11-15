# Fixed proof-packet CI scheduling (#443)

The fixed Lean4Lean proof packets follow the existing long, platform-neutral
Lean-replay scheduling policy: full execution on Linux and Windows, with a
macOS-only Bazel compatibility exclusion. Each dependent proof PR adds its
own target entry. Regular-input `copy_test_cwd` remains enabled.

The measured failure is the foundation packet on PR #407 at
`5e19c99cdfe1c9bc702a0ff8158f63161b7cbb83`. Platforms run
[43571220456, macOS job 132375851033](https://github.com/robot-head/vesper/actions/runs/43571220456/job/132375851033)
reports `//:test/lean4lean_foundations42` TIMEOUT after 300.3 seconds:
708 other tests passed and six existing suites were skipped. The genuine
test started and ran for more than sixty seconds. No source-guard or
assertion failure appears. Per-process output was captured by libtest, so
the exact phase and launcher-copy overhead are unknown. This evidence does
not establish that the complete packet passed on macOS.

The same head's Windows workflow passed completely, as did both Linux
checks. Existing genuine Linux foundation executions passed in 127.08
seconds and in 134.40 seconds through the unchanged regular-copy launcher.
The same macOS job passed `proof_audit_private` in 164.3 seconds. That test
preserves relevant process/path portability coverage; it does not replace
a successful macOS execution of the full foundation packet.

The test independently enforces a 300-second whole-matrix budget, clips
process deadlines to remaining time and passes `--timeout 300` to the
auditor. Increasing Bazel's outer timeout alone is not a demonstrated fix.
No internal or outer timeout, memory/artifact cap, policy, expected answer,
ignore attribute, Rust/Lean source or workflow changes here. Every actual
authored root and alternate-proof alias, exact source/provenance/artifact
association, kernel projection, strict axiom audit and genuine negative
control remains required on Linux and Windows.

The sibling construction, ordering, map, recursive-construction and full
normalization packets use the same scheduling because their subject and
runner are platform-neutral. No macOS timeout measurement for each sibling
is claimed. Their ordinary Linux matrices remain independently required;
their regular-input launcher validation remains separately documented in
[the runfiles report](lean4lean-proof-packets-runfiles.md).

The change is confined to existing Bazel compatibility configuration and
documentation. Static target/source checks, BUILD synchronization and
whitespace validation are required before push. Hosted macOS success after
this scheduling change must be observed before it is reported as passing.
