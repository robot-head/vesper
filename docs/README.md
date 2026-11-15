# Documentation

The [manuscript](manuscript/README.md) is the sole source of truth for Vesper's
language and platform specification. Its native Typst sources produce the
[published PDF](https://vesper-lang.org/vesper-specification.pdf).

Edit requirements in the manuscript. Other documentation describes implementation
APIs and operational workflows and should link to the manuscript rather than
maintain specification copies. Prompts, designs, and supporting reports needed by
open issues or unmerged branches remain in
[retained work context](agent-prompts/README.md). Completed
archives and duplicate specification snapshots are available in Git history.

## Workflows

- [Build and test with Bazel](bazel.md)
- [Platform validation and CI](ci-platforms.md)
- [Package workflows](package-workflows.md)
- [Content-addressed storage](cas-workflows.md)
- [Package audits](package-audits.md)
- [Builder attestations](builder-attestations.md)
- [Proof audits](proof-audits.md)
- [SIMD scanner](simd-scanner.md)

Library APIs and examples are documented with their [crates](../crates/README.md).
Retained work plans use [earlier section identifiers](section-map.md). Their
implementation boundaries do not override the manuscript. Machine-readable
reports, proof fixtures, licenses, and source manifests keep their existing
paths for tooling and reproducibility.
