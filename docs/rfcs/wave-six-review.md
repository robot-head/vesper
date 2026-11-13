# Section status at the start of wave six

Planning summary, 2026-11-13, against the design snapshot in
[specification.txt](../specification.txt) (Google Doc revision modified
2026-11-13 04:46 UTC, 42 numbered sections plus Appendix A). It updates
[wave-five-review.md](wave-five-review.md) from the wave-five reports in
`docs/parallel-results/`. It is not a new audit. "Later" means not planned in
wave six; the reason is given.

| § | Section | Wave six | Left for later, and why |
| --- | --- | --- | --- |
| 1 | Research landscape | — | Prose |
| 2–6 | Core language, calculus, typing, effects | Lane 15 (capability effects), lane 16 (metatheory) | Full-language metatheory beyond lane 16's fragment |
| 7 | 5-stage architecture | B (Stage-5 verifier hooks), 23, 24 | — |
| 8 | Loom | — | Remaining distribution features per Loom reports |
| 9–10 | SemVer, workspace | — | Done |
| 11 | Lean extractor | — | Broader extraction coverage |
| 12 | Pulse | Lane 22 (Windows fibers) | io_uring needs a host that permits `io_uring_setup` |
| 13 | Bridge | Lane 1 (single out-pointers, MPK/POE domains) | Omniglot-style isolation beyond protection keys |
| 14 | Vector | — | Source `Isa[P]` lowering touches native lowering, which B, 23 and 24 change now |
| 15 | Strata | — | Source lifetime and alias rules touch the checker, which lane 15 changes now |
| 16 | Pact and Forge | — | §16.4 attribute/item macros (other agents delivered expression macros only) |
| 17 | Prism | Lane 6 (HTTP/3, QPACK) | WebTransport |
| 18 | Aura | — | WinUI 3, GTK 4, SwiftUI backends need desktop hosts; plan with the user |
| 19 | Ignis | — | DirectX 12, Vulkan, Metal execution needs GPUs (or a software Vulkan in CI); plan with the user |
| 20 | Wire/Json | Lane 7 (SIMD on-demand JSON) | Source derives (Forge item macros) |
| 21 | Lens | Lane 8 | — |
| 22 | Beacon | Lane 9 (OTAP, PII redaction) | Source `Trace` propagation |
| 23 | Vault | Lanes 15, 24 (source `ConstTime`) | Vector NTT for ML-KEM, register scrubbing |
| 24 | Quarry | Lane 10 (durability, vectorized execution) | Source `@sql` (Forge macros) |
| 25 | Chorus | Lane 17 | `@choreography` (Forge macros), hardware RDMA |
| 26 | Silicon | Lane 19 | Freestanding target (backend), `@svd_pac` (Forge macros) |
| 27 | eBPF | Lane 18 | Source eBPF backend, `@btf_core`; linear builder excluded |
| 28 | Tensor | Lane 21 (CPU inference) | Source tensor types, GPU, egglog fusion (optimizer held by other agents) |
| 29 | Crucible and migrate | B (CLIs), lanes 1, 20, 23 | `@fuzz_prop`, SMT-backed solving |
| 30 | Nexus | — | Compiler Wasm target (backend), WIT-to-trait generator, Pulse adapter |
| 31 | Chrono | Lane 2 (trace decoding, native checkpoints), B (`vesper replay`) | Live IPT/CoreSight capture needs hardware |
| 32 | Resonance | Lanes 15, 24 (source `Realtime`) | egglog fusion, SIMD SVF, BIBO Lean proofs, audio drivers |
| 33 | Aegis | Lane 4 (real vendor samples) | Evidence generation on real TEEs |
| 34 | Flux | Lane 11 | Source operators |
| 35 | Warp | Lane 5 | DPDK, hardware offloads |
| 36 | Kinetic | Lanes 3, 15, 24 | GPU broadphase, articulated and soft bodies |
| 37 | Aether | Lane 12 | KVM execution and unikernel targets need `/dev/kvm` |
| 38 | Grammar | — | Source `@wire_grammar` (Forge macros) |
| 39 | Stylus and Lint | — | Held by other agents earlier; no open claim at planning time |
| 40 | Codex | Lane 14 | — |
| 41 | Metronome | Lane 13 | Counters on hosts with a hardware PMU |
| 42 | Verified bootstrap | — | Held by other agents (#230-#242, PR #238) |
| A | Mechanized metatheory | Lane 16 | See §2–6 |

## Proposed wave seven

Once wave six merges: the compiler backends (Wasm, eBPF, freestanding Silicon,
Vector `Isa[P]`), source Strata lifetimes, and whatever Forge item/attribute
macros the other agents deliver (then `@choreography`, `@svd_pac`, `@btf_core`,
`@sql`, `@wire_grammar`, derives). Hardware backends need hosts that CI lacks
today; the user decides whether to provide them.
