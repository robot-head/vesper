# vesper-pact-binding

Root-free core of the Pact source bridge: versioned binding DTOs, package-qualified `DefPath` identities, dense id tables, mode-scheme derivation and verification, lowering to a Pact `ProgramDto`, the `check_coherence` gate and revision-bound evidence sessions.

It depends only on `vesper-core` and `vesper-pact`. Checked packages enter as `SourcePackage`s over a caller-implemented `SourceWorld` (the compiler implements it for its `World`). This lets the compiler's source trait frontend (`vesper::pact_frontend`) use Pact sessions directly, while `vesper-pact-bridge`, which runs the compiler, stays above the compiler: `vesper -> vesper-pact-binding -> {vesper-core, vesper-pact}` and `vesper-pact-bridge -> {vesper, vesper-pact-binding}`. There is no cycle.

`SourcePackage::with_fingerprint` folds a caller-computed declaration fingerprint into the session `Revision`, so any source declaration change invalidates evidence, not only changes visible in the lowered Pact program.

See [the lane report](../../docs/parallel-results/source-traits-models.md).
