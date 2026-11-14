# Section status at the start of wave seven

Planning summary, 2026-11-14, against the design snapshot in
[specification.txt](../specification.txt) (Google Doc revision modified
2026-11-13 04:46 UTC, 42 numbered sections plus Appendix A; Drive
`modifiedTime` checked on 2026-11-14 and unchanged). It updates
[wave-six-review.md](wave-six-review.md) from the "Remaining gaps" sections of
the wave-six reports in `docs/parallel-results/`. It is not a new audit.
"Later" means not planned in wave seven; the reason is given.

Checked and already done (not replanned): Bridge single out-pointers (wave six
lane 1, PR #281); `vesper fuzz` and `vesper verify --refute` (integration B,
PR #295, with native coverage from lane 23, PR #307).

| § | Section | Wave seven | Left for later, and why |
| --- | --- | --- | --- |
| 1 | Research landscape | — | Prose |
| 2–6 | Core language, calculus, typing, effects | Lane 15 (metatheory: closures, handler state, effect rows), lane 30 | Place Capability Graph and second-class borrows in the mechanization; full-language metatheory |
| 7 | 5-stage architecture | Lanes 23 (spans, fuel, capability-passing ABI), 24 (Windows executables) | — |
| 8 | Loom | — | Remaining distribution features per the Loom reports |
| 9–10 | SemVer, workspace | — | Done |
| 11 | Lean extractor | — | Broader extraction coverage |
| 12 | Pulse | Lanes 2 (epoll, kqueue, IOCP drivers), 3 (stack images) | io_uring on a host that permits `io_uring_setup`; fiber-local storage and CET shadow stacks on Windows |
| 13 | Bridge | Lane 6 (`.vsys` slices, nullable pointers) | `isolated(mpk)` trampolines in the native backend; Omniglot-style isolation beyond protection keys |
| 14 | Vector | Lane 28 (source vector types, `Isa[P]`, Cranelift SIMD) | Scalable vectors (SVE, RVV) |
| 15 | Strata | Lane 29 (lexical regions, lifetimes) | ShardSlab, `tiled(N)`/AoSoA collections, `@fip` reuse |
| 16 | Pact and Forge | — | §16.4 attribute/item macros (Codex) |
| 17 | Prism | Lane 8 (WebTransport, datagrams, priorities, streaming bodies) | — |
| 18 | Aura | Lane 31 (GTK 4 under Xvfb) | WinUI 3 and SwiftUI backends need Windows/macOS desktops |
| 19 | Ignis | Lane 32 (Vulkan on Mesa lavapipe) | DirectX 12 and Metal; real-GPU performance |
| 20 | Wire/Json | Lane 9 (AVX-512/NEON, Eisel-Lemire, Lean) | Source derives (Forge item macros) |
| 21 | Lens | Lane 10 | — |
| 22 | Beacon | Lane 1 (tonic gRPC, collector interop, full columns) | Source `Trace` propagation and `Telemetrable` coherence (checker work alongside lanes 28-30) |
| 23 | Vault | Lane 30 (secret operations, interprocedural taint) | Vector NTT for ML-KEM, register scrubbing |
| 24 | Quarry | Lane 11 (B+Tree, MVCC, Lean WAL) | Source `@sql` (Forge macros); `O_DIRECT`/io_uring device |
| 25 | Chorus | Lane 16 (TLS over Warp, recursive duality, durable Raft) | `@choreography` (Forge macros), RDMA hardware |
| 26 | Silicon | Lanes 18 (devicetree), 27 (freestanding target) | `@svd_pac` (Forge macros), real boards |
| 27 | eBPF | Lanes 17 (library), 26 (source backend) | `@btf_core` (Forge macros); linear builder excluded |
| 28 | Tensor | Lanes 20 (formats, BPE, sampling, SIMD, Lean), 33 (Vulkan compute on lavapipe) | Source tensor types, real GPUs, egglog fusion (optimizer held by other agents) |
| 29 | Crucible and migrate | Lanes 19 (DPOR depth, Lean), 23 (fuel, refute coverage) | `@fuzz_prop`, MPK differential shadowing, SMT-backed solving |
| 30 | Nexus | Lanes 22 (WIT generator, SimPulse adapter), 25 (Wasm target) | WasmFX beyond what Wasmtime 50 supports |
| 31 | Chrono | Lane 3 (byte-image checkpoints, CoreSight, ETE flow) | Live IPT/CoreSight capture needs hardware |
| 32 | Resonance | Lanes 21 (BIBO proofs, SIMD SVF), 30 (`RtBus`) | egglog fusion, audio and MIDI drivers |
| 33 | Aegis | Lane 4 (AMD KDS, VLEK, Genoa/Turin, CCA, new formats) | Evidence generation on real TEEs |
| 34 | Flux | Lane 12 (durable sources/sinks, partial recovery, rescaling) | Source operators |
| 35 | Warp | Lane 7 (coalescing, CID retirement, idle timer, migration) | DPDK, AF_XDP and NIC offloads on hardware |
| 36 | Kinetic | Lanes 5 (SIMD batches, contacts, broadphase kernel), 30 (`ConstDenorm`), 33 (broadphase on Vulkan compute) | Real-GPU broadphase performance, articulated and soft bodies |
| 37 | Aether | Lane 13 (memfd minor faults, devices) | KVM execution and unikernel targets need `/dev/kvm`; `vesper build --unikernel` |
| 38 | Grammar | — | Source `@wire_grammar` (Forge macros) |
| 39 | Stylus and Lint | — | Held by other agents earlier; no open claim at planning time |
| 40 | Codex | Lane 14 | — |
| 41 | Metronome | — | Follow-up after PR #283 merges; counters on hosts with a hardware PMU |
| 42 | Verified bootstrap | — | Held by other agents (#322, PR #323 and earlier) |
| A | Mechanized metatheory | Lane 15 | See §2–6 |

## Hardware-only work

These need machines that the cloud host and the GitHub/BuildBuddy runners do
not have. On 2026-11-14 the owner chose software stand-ins where one exists:
lanes 31 (GTK 4 under Xvfb), 32 (Vulkan on lavapipe), 33 (GPU compute on
lavapipe) and QEMU in lane 27. The rest stays deferred:

| Work | Needs | Possible software stand-in in Linux CI |
| --- | --- | --- |
| Aura GTK 4 backend | a desktop session | Xvfb + GTK 4 |
| Aura WinUI 3, SwiftUI backends | Windows or macOS desktop | none on Linux; GitHub Windows/macOS runners can build but not drive a UI reliably |
| Ignis Vulkan backend, Tensor and Kinetic GPU compute | a GPU | Mesa lavapipe (software Vulkan) |
| Ignis DirectX 12, Metal backends | Windows or Apple GPU | WARP on Windows runners (DX12 only) |
| Aether KVM execution, unikernel images | `/dev/kvm` | none (nested virtualization needed) |
| Aegis evidence on real TDX/SEV-SNP/CCA guests | confidential VMs | none |
| Chrono Intel PT / CoreSight capture, Metronome hardware counters | bare-metal x86 or Arm with PMU access | none |
| Chorus RDMA, Warp AF_XDP/DPDK and offloads | RDMA/XDP-capable NICs | `veth` AF_XDP in a privileged container |
| Silicon on boards, Resonance audio drivers | RISC-V boards, audio interfaces | QEMU `virt` (lane 27 uses it when present) |

## Proposed wave eight

Once wave seven merges: source `Trace` propagation, the Strata collection suite,
Forge-macro dependents if the other agents deliver item and attribute macros,
the Metronome follow-up, and any hardware rows the owner later provides hosts for.
