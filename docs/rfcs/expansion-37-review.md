# Review of the expanded design through section 37

The [recorded source revision](../specification-source.json) contains all 37
sections and is preserved without rewriting its proposals. Six independent
readers covered sections 1–7/20, 8–11/21–23, 12–19/24–25, 26–29, 30–34 and
35–37 respectively. The coordinator reread the active new foundations and
combined their findings here. This is implementation review, not a new proof or
an edit to the upstream Google Doc.

## Newly identified implementation work

| Section | Work and current boundary |
| --- | --- |
| 20 Wire/Json | This batch: strict bounded host JSON/index/cursor/scratch and codec helpers. Source derives, vector lowering and archives follow separately. |
| 21 Lens | Compiler/LSP recovery, obligation completion and hermetic loading depend on the changing frontend; deferred during wave three. |
| 22 Beacon | This batch: traceparent v00, explicit span ownership, bounded telemetry and accurate histogram contract. Source Trace/evidence propagation and OTAP follow separately. |
| 23 Vault | Nonce/secret/constant-time rules need compiler and audited crypto/platform boundaries; no cryptographic or constant-time implementation claimed here. |
| 24 Quarry | This batch: parsed typed SQL/catalog, sound supported optimization and executing host plans. Source @sql, storage/page pool and SIMD follow separately. |
| 25 Chorus | Choreography/session projection, consensus, RDMA and source simulation require their own protocols and integration; not a generic networking claim. |
| 26 Silicon | Freestanding target, interrupt/MMIO/DMA capabilities and machine stack-bound analysis require new backend/platform work. |
| 27 eBPF | Emitted CFG verifier analysis, actual kernel load evidence and source capabilities require a dedicated backend milestone. |
| 28 Tensor | This batch: bounded symbolic shapes, checked host IR and real CPU fusion/quantized kernels. Source types, GPU/ISA integration and full inference follow separately. |
| 29 Crucible | Concolic instrumentation, obligation-aware shrinking and migration need compiler/foreign boundary work outside frozen paths. |
| 30 Nexus | WasmFX and linear WIT/resource isolation require their actual target/runtime contracts; existing Wasmtime components do not imply this support. |
| 31 Chrono | Flight recording/checkpoints depend on actual native activation layouts; hardware branch tracing and replay remain separate. |
| 32 Resonance | Realtime effect confinement needs transitive compiler enforcement, not name filtering; DSP scheduling must model delayed feedback explicitly. |
| 33 Aegis | Signed hardware attestation and trusted page acceptance need real platforms; host normalization checks alone are not those proofs. |
| 34 Flux | This batch: exact bounded Z-set/DBSP execution and owned frontiers with model lemmas. Durable distributed checkpoints, source operators and AoSoA/SIMD follow separately. |
| 35 Warp | AF_XDP/DPDK DMA owners, QUIC/TCP/BBR and source authority need staged networking milestones and actual device/protocol evidence. |
| 36 Kinetic | DetFloat requires cross-backend rounding/codegen enforcement before bit-determinism claims; XPBD/GPU/rollback need dedicated work. |
| 37 Aether | This batch: checked layout/snapshot maps and verified actual CAS pages with numeric model lemmas. KVM/UFFD, unikernel targets, VMGENID entropy and timing remain separate. |

Section 19 has also expanded to DirectX 12, Vulkan 1.4 and Metal 4. Its added
cross-backend scaffold cannot be treated as a general translator: narrowing
fence IDs and fixed wait-layout/producer-stage choices require target-specific
validation. The existing Ignis host milestone makes no such executor claim.

## Corrections required when implementing the examples

- Preserve the user-approved `k_i ⊑ m_i` field premise and existing checked
  arithmetic, one-shot/loan and resumption-frame rules throughout sections 1–7.
- JSON's constant-time skipping needs a prior validation pass if malformed
  primitives/unknown subtrees must be rejected. Short-key matching must handle
  JSON escaping/ASCII constraints, and all size-bound arithmetic must be checked.
- Beacon's listed context fields do not fit a 24-byte serialized record. The
  sample histogram overlaps buckets, clamps large values and occupies more
  than 2 KiB with exemplars; its claimed precision/range does not follow.
- Quarry catalog replacement/alias handling needs explicit validation, and
  predicate pushdown needs typed dependencies and SQL NULL/outer-join rules.
  Wrapping totals do not substitute for the established checked semantics.
- Tensor scaffold multiplication/modulo needs overflow/zero checks. Unresolved
  symbolic obligations and FP fusion/reassociation cannot silently become success.
- Flux saturating weights violate the stated integer-group exactness. Changing
  both join inputs requires all three delta terms. One timestamp cannot account
  for all live frontier holds; owning a ticket cannot prove eventual progress.
- WCSD/eBPF sketches lack complete machine CFG/provenance bounds. A decreasing
  shortlex shrink order proves termination, not a globally minimal failing tape.
  Finite differential samples are not migration equivalence proofs.
- Chrono's binary search requires ordered tape events. A realtime call-name
  allowlist does not establish transitive effect confinement. Normalized Aegis
  claims and transcript prefixes do not authenticate signed full attestations.
- Aether must reject range overflow and incomplete snapshot metadata. HVA range
  separation does not establish physical/EPT separation. Sharing requires an
  immutable identity/permission boundary, and arbitrary HKDF is not injective.

Performance figures and hardware/constant-time/security theorems remain
unvalidated proposals unless a report states the actual experiment/model and
its assumptions. These observations guide supported implementation slices;
unsupported cases fail explicitly rather than weakening safety requirements.
