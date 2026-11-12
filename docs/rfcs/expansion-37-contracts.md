# Independent foundations from the expanded 37-section design

Source: [live design](https://docs.google.com/document/d/1zO7boUVwNTHRA-1ZNL4bDYi68OjQZBCimLWkETH6bso/edit), refreshed in
[specification.txt](../specification.txt). Revision, upstream modification time,
normalization and content digest are recorded in [source metadata](../specification-source.json).
All 37 sections are present; source order is preserved, including section 35 at
its beginning and sections 29–31 before section 21.

Wave three is still merging under the user's external agent. No frontend
AST/syntax/checker/interface/Pact, native compiler/session/fiber, Forge runner,
SimPulse/source simulator, SIMD scanner/model, or Prism production path is owned
by this batch. Root manifests/lock, CI/workflows, CLI/module wiring, global docs,
shared schemas, Lean extractor and existing TUF paths remain coordinator-owned.

| Lane | Exclusive implementation | Document review coverage |
| --- | --- | --- |
| JSON/codecs | New `crates/vesper-serde/**` | 1–7 and 20 |
| Beacon | New `crates/vesper-beacon/**` | 8–11 and 21–23 |
| Quarry | New `crates/vesper-quarry/**` | 12–19 and 24–25 |
| Tensor | New `crates/vesper-tensor/**` | 26–29 |
| Flux | New `crates/vesper-flux/**`, new `lean/FluxDbsp.lean` | 30–34 |
| Aether | New `crates/vesper-aether/**`, new `lean/AetherMemory.lean` | 35–37 |

Each owns only its matching new report under `docs/parallel-results/expansion-37-*.md`.
All source/SDK/runtime code outside these boundaries is read-only. New crate
APIs are independent: no lane waits for another new crate to finish or invents
unreviewed shared compiler schemas. Coordinator registers all completed crates
and supplies runnable CLI/examples plus focused CI validation as appropriate.

Deliver working bounded host-library slices with actual data execution and
meaningful negative/failure tests. Source macros/effects/type enforcement,
SIMD/egglog lowering, native device execution, network/database services and
hardware isolation remain separate integrations unless actually implemented and
validated. No scaffold is treated as a correctness proof or performance result.

In particular:

- JSON must validate malformed unknown subtrees, Unicode and limits before
  advertising valid slices. Escaped output uses explicit bounded caller scratch;
  no unsupported source-region/linear-object rollback claim.
- Beacon must define v00 TraceContext scope and histogram range/error/memory
  accurately; the example's saturating bucket index cannot justify all-u64
  relative accuracy. Backpressure returns live span owners.
- Quarry derives predicate dependencies from typed expressions, preserves SQL
  null/outer-join semantics and invalidates plans on schema changes. Raw predicate
  strings or caller-asserted table sets are not optimization evidence.
- Tensor checks dimension arithmetic/zero ratios/budgets and resolved shapes
  before executing host kernels. Fusion needs actual semantic comparisons and
  must not assume floating-point reassociation is exact.
- Flux uses checked exact arithmetic, never saturating weights under an integer
  group-equivalence claim. Failed batches leave state and owners recoverable.
  Source cancellation/WAL/crash simulation and distributed exactly-once claims
  are outside this slice.
- Aether uses checked half-open ranges, complete bounded snapshot mappings and
  verified actual CAS bytes. HVA checks do not prove physical/EPT isolation.
  Sharing permissions require explicit immutable identity. No assumed HKDF
  injectivity, clone-safe nonce claim, KVM/UFFD execution or boot-time claim.

Arithmetic stays `vesper.arith.checked/1`; interface version 2, fiber ABI v2 and
the user-approved `k_i ⊑ m_i` field premise remain fixed. Generated Lean stays
AST-based. New handwritten model proofs must compile under pinned Lean 4.24,
with no sorry, new axioms, native_decide or bv_decide under kernel-only claims.

Each agent uses a dedicated worktree and small private build target, one build
job, zero development/test debug information and disabled incremental caches.
Commit exclusive files only; temporary root wiring for local tests is not a
handoff patch. The coordinator reviews, integrates, runs combined checks and
pushes a coherent stopping point. Platform compilation and model tests are
reported separately from native runtime and hardware evidence.
