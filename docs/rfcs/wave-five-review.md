# Section status at the start of wave five

Baseline `af03122ea74d403f203170926945f9d35786b289` (2026-11-12), against the
design snapshot in [specification.txt](../specification.txt), which matches
the Google Doc revision modified 2026-11-12 09:18 UTC. Status comes from the
integration reports in `docs/parallel-results/` and the crates on `main`; it is
a planning summary, not a new audit.

"Started" means a supported, tested slice exists. No section is complete in
the sense of the design's full text: most remaining work is source-language
integration (checker effects, Forge macros, native lowering), real hardware or
kernel paths, or durability. Those are listed so later waves can claim them.

| § | Section | On `main` | Remaining after wave five |
| --- | --- | --- | --- |
| 1 | Research landscape | Prose, nothing to build | — |
| 2–6 | Core language, grammar, calculus, typing, effects | Compiler, checker, evaluator, native, Lean foundations | Metatheory beyond Appendix A's slice |
| 7 | 5-stage architecture | Stages exist; egglog optimizer in `ir_opt` | Stage-5 verifier hooks (integration B) |
| 8 | Loom | Calculus, solver, CAS, TUF, audit, admission, attestations | Remaining distribution features per Loom reports |
| 9 | SemVer | Done for interface v2 | — |
| 10 | Workspace | Done; every crate registered | — |
| 11 | Lean extractor | AST emitter, checked arithmetic, effects | Broader extraction coverage |
| 12 | Pulse | Fibers on Linux and macOS AArch64, source-native suspension, SimPulse | Windows fibers, io_uring on a permitting host |
| 13 | Bridge | Wire layouts, sys-specs | Omniglot/MPK isolation |
| 14 | Vector | SIMD scanner and kernel model | Source `Isa[P]` lowering |
| 15 | Strata | Regions, slabs, collections | Source lifetime and alias rules |
| 16 | Pact and Forge | Goals, bridge, WASI host, hygiene | §16.4 source macros (claimed by other agents, #103–#110) |
| 17 | Prism | Framing, responses, connections, routing | HTTP/3 over Warp |
| 18 | Aura | Reconciliation | WinUI 3, GTK 4 and SwiftUI backends |
| 19 | Ignis | Barrier graph, command lifetimes | DirectX 12, Vulkan 1.4 and Metal 4 execution |
| 20 | Wire/Json | Host JSON, binary archives | Source derives, SIMD on-demand parsing |
| 21 | Lens | LSP, recovery, incremental reparse | PoisonedRecovery, obligation completion, proof lenses, latency target |
| 22 | Beacon | TraceContext, spans, Arrow export | Source Trace propagation, OTAP transport |
| 23 | Vault | — | **Wave five lane 1 (#121)**; then source `eff [ConstTime]` |
| 24 | Quarry | Typed SQL, plans, pages | Source `@sql`, durable storage, SIMD execution |
| 25 | Chorus | — | **Lane 2 (#122)**; then `@choreography` macro, Warp transport |
| 26 | Silicon | — | **Lane 3 (#123)**; then freestanding target, `@svd_pac` |
| 27 | eBPF | — | **Lane 4 (#124)**; then source eBPF backend, `@btf_core` |
| 28 | Tensor | Shapes, host IR, CPU kernels, KV pages | Source tensor types, GPU, full inference |
| 29 | Crucible | — | **Lanes 5 (#125) and 11 (#131)**; then Cranelift instrumentation |
| 30 | Nexus | — | **Lane 6 (#126)**; then compiler Wasm target |
| 31 | Chrono | — | **Lane 7 (#127)**; then native activation checkpoints, IPT/CoreSight |
| 32 | Resonance | — | **Lane 8 (#128)**; then source `eff [Realtime]`, egglog fusion |
| 33 | Aegis | — | **Lane 9 (#129)**; then real TEE platforms |
| 34 | Flux | Z-set/DBSP execution, frontiers, checkpoints | Durable distributed checkpoints, source operators |
| 35 | Warp | QUIC core, BBRv3, UMEM, wheel, crypto, shards, TCP, source sim, end-to-end path | **Lanes 12–14 (#132–#134) and integration A (#135)**; then DPDK, hardware offloads |
| 36 | Kinetic | — | **Lane 10 (#130)**; then GPU broadphase, source `eff [DetFloat]` |
| 37 | Aether | Layout and snapshot maps, CAS pages, VirtIO capture | KVM/UFFD execution, unikernel targets |
| 38 | Grammar | DER, Protobuf, IPC codecs, grammar tooling | Source `@wire_grammar` generation |
| 39 | Stylus and Lint | Formatter and lint profiles | Formatter macro and proof work (claimed by other agents, #111–#120) |
| 40 | Codex | `vesper doc`, doctests | Proof-backed badges, SimPulse I/O doctests |
| 41 | Metronome | Bench harness, PMU slice | RDPMC/macOS counters, layout randomization, causal profiling |
| A | Mechanized metatheory | Appendix slice in `lean/MetatheoryAppendix.lean` | Full-language progress and preservation |

## Proposed later waves

Wave six should pick up the source-language integrations that wave five's host
libraries make possible: checker effects for `ConstTime`, `Realtime` and
`DetFloat`, and the Forge macros (`@choreography`, `@svd_pac`, `@btf_core`)
once the §16.4 macro work held by other agents merges. Hardware backends
(Aura, Ignis, Aether KVM, TEE platforms) need hosts that CI does not have
today and should be planned with the user.
