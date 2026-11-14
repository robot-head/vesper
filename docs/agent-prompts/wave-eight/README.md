# Eighth parallel implementation wave: verified bootstrap, metatheory, source features and the remaining library depth

Status: **ready to assign**. One thread per lane; do not launch duplicate writers.
Each linked file is a complete standalone prompt. Start from a fresh
`origin/main` containing **`ea1ed2964c2583b5dc2da8e0fd7651daa39b00e2`** or later. Ownership and shared rules are in
the [wave-eight contracts](../../rfcs/wave-eight-contracts.md); the
[section status review](../../rfcs/wave-eight-review.md) shows what each lane
covers and what is left after this wave.

Source: the "Remaining gaps" sections of the wave-seven reports under
`docs/parallel-results/`, the "Proposed wave eight" list in
[wave-seven-review.md](../../rfcs/wave-seven-review.md), and a section-by-section
completion estimate on 2026-11-14 (about 73% of the design on `main`, about 79%
once the open wave-seven PRs merge), against the design snapshot in
[specification.txt](../../specification.txt) (Google Doc revision modified
2026-11-13 04:46 UTC).

Lanes 1-4 start §42 (verified bootstrap) in the parts that Codex does not hold:
the fixed-point and diverse double-compiling gate, a direct proof-carrying code
generator, a self-hosted lexer and parser written in Vesper, and an executable
Lean reference evaluator run in lockstep with the Rust compiler. Lanes 5-6
extend the mechanized metatheory. Lanes 7-14 are source-language and compiler
work (Strata, scalable vectors, Tensor types, egglog fusion, Lean extraction,
`Trace`, Wasm). Lanes 15-27 finish the gaps recorded in existing crates.

Only lanes 1 and 11 add workspace members (`vesper-codegen-verified`,
`vesper-fusion`); their merges go one at a time. Several lanes wait on open
wave-seven PRs; the Starts column says which.

## Lanes

| Lane | Prompt | Claim | Design | Starts | Exclusive boundary |
| --- | --- | --- | --- | --- | --- |
| 1 | [Verified bootstrap: Stage 2/3 fixed-point gate, DDC cross-check and the bootstrap lock](01-bootstrap-ddc-gate.md) | #450 | §42 | now | NEW crates/vesper-codegen-verified/**; NEW bootstrap/** (stage lock files and the bootstrap driver script); NEW lean/BootstrapDdc.lean and lean/BootstrapDdc/**; Root registration patch for vesper-codegen-verified (own commit) |
| 2 | [Verified bootstrap: a direct, proof-carrying code generator for the scalar core (x86-64 and AArch64)](02-verified-direct-codegen.md) | #451 | §42, §7 | now | NEW src/verified_codegen.rs and src/verified_codegen/**; NEW lean/VerifiedCodegen.lean and lean/VerifiedCodegen/**; NEW tests/verified_codegen*.rs and tests/fixtures/verified_codegen/**; One routing hunk for `vesper build --codegen verified` in src/main.rs (own commit) |
| 3 | [Self-hosting Stage 1: a Vesper-source lexer and parser for the core subset, differential against the Rust seed](03-self-hosted-front-end.md) | #452 | §42.3, §38 | now | NEW packages/vesper-compiler/** (Vesper source, Loom.toml with `capability_ceiling = ["Fail"]`); NEW tests/self_host*.rs and tests/fixtures/self_host/** |
| 4 | [Stage 0b lockstep: an executable Lean reference checker and evaluator, run against vesperc-rs](04-lean-reference-lockstep.md) | #453 | §42.1, Appendix A, §29 | now | NEW lean/ReferenceEval.lean and lean/ReferenceEval/**; NEW tests/lockstep*.rs and tests/fixtures/lockstep/** |
| 5 | [Mechanized metatheory: recursion, type variables, first-class row polymorphism and handler-label escape](05-metatheory-recursion-polymorphism.md) | #454 | Appendix A, §4-§6 | now | lean/Metatheory/** and lean/MetatheoryFull.lean; tests/metatheory_full.rs and NEW tests/metatheory_poly*.rs |
| 6 | [Mechanized metatheory: mutable places, the Place Capability Graph, second-class borrows and Zero-Pin](06-metatheory-places-borrows.md) | #455 | Appendix A, §3-§5 | now | NEW lean/MetatheoryPlaces.lean and lean/MetatheoryPlaces/**; NEW tests/metatheory_places*.rs |
| 7 | [Strata collections: ShardSlab, tiled(N)/AoSoA columns, RegionVec/RegionString/RegionSlice and SlotArena](07-strata-collections.md) | #456 | §15 | now | crates/vesper-alloc/**; NEW lean/StrataCollections.lean |
| 8 | [Source Strata rest: Lean model of the region rules, aggregate native payloads and a region runtime for executables](08-strata-source-rest.md) | #457 | §15 | now; merge after PR #384 (lane 23) if it touches src/native*.rs | src/regions.rs and src/regions/**; NEW lean/StrataRegions.lean and lean/StrataRegions/**; tests/regions*.rs and tests/fixtures/regions/**; A narrow additive hunk in src/native*.rs for region payloads and the runtime link step (own commit) |
| 9 | [Scalable vectors: VVec strip-mined predication with SVE and RVV kernels, run under QEMU](09-scalable-vectors-qemu.md) | #458 | §14.3, §20 | now | crates/vesper-simd/**; NEW SVE/RVV modules in crates/vesper-serde/src/ (additive; existing paths unchanged); NEW .github/workflows/scalable-vectors.yml (QEMU user-mode job) |
| 10 | [Source Tensor: symbolic shape-typed Tensor types, quantized wire-struct blocks and a linear KvPageLease](10-source-tensor-types.md) | #459 | §28.2 | now; its src/check.rs hooks merge after PRs #379 and #375 (both touch src/check.rs) | NEW src/tensor_types.rs and src/tensor_types/**; Narrow hook calls in src/check.rs (own commit); Additive syntax in crates/vesper-syntax/** only if unavoidable (own commit); NEW tests/tensor_types*.rs and tests/fixtures/tensor_types/** |
| 11 | [egglog operator fusion for Tensor graphs and Resonance DSP chains](11-egglog-fusion.md) | #460 | §28, §32, §7 Stage 4 | now | NEW crates/vesper-fusion/**; NEW lean/FusionRules.lean; Root registration patch for vesper-fusion (own commit) |
| 12 | [Lean extractor: structs, enums and match, loops, effects and the places Lens could not prove](12-lean-extractor-breadth.md) | #461 | §11 | now | src/lean.rs, src/lean_ir.rs, src/lean_nodes.rs; crates/vesper-lean-syntax/** (additive); NEW tests/lean_extract*.rs and tests/fixtures/lean_extract/** |
| 13 | [Source Beacon: `Trace` context propagation and `Telemetrable` coherence in the checker](13-source-trace-telemetrable.md) | #462 | §22 | now; its src/check.rs hooks merge after PRs #379 and #375 | NEW src/trace_effects.rs and src/trace_effects/**; Narrow hook calls in src/check.rs (own commit); NEW tests/trace_effects*.rs and tests/fixtures/trace_effects/** |
| 14 | [Wasm target rest: strings, f64 remainder, monomorphization, labelled rows, CPS handlers and WASI imports](14-wasm-backend-rest.md) | #463 | §30, §7 | now; merge after PR #379 (it touches src/wasm_backend) | src/wasm_backend.rs and src/wasm_backend/**; tests/wasm_backend*.rs and tests/fixtures/wasm_backend/** |
| 15 | [Metronome rest: CAS baselines, --deterministic, --asm, zero-alloc and vectorization evidence, PmuSampleWire](15-metronome-rest.md) | #464 | §41 | now | crates/vesper-bench/**; crates/vesper-bench-pmu/**; src/bench_commands.rs |
| 16 | [Lens: check the 10 KB fixtures, a Send writer, and Loom capability ceilings in completion](16-lens-large-fixtures.md) | #465 | §21 | now | crates/vesper-lsp/**; src/lsp.rs and src/lsp/**; A narrow hunk in crates/vesper-doc for the preflight bound (own commit; lane 17 owns that crate) |
| 17 | [Codex docs: may-use footprints, mode-crossing tables, capability badges and a package-wide index](17-codex-docs-footprints.md) | #466 | §40 | now; merge after lane 16 if both touch crates/vesper-doc | crates/vesper-doc/**; src/doc_commands.rs, src/doctest.rs, src/doctest_commands.rs, src/tooling/doc.rs, src/tooling/doc/** |
| 18 | [Flux: a Kafka source and sink, and live rescaling without a stop](18-flux-kafka-rescale.md) | #467 | §34 | after PR #381 (wave-seven lane 12) merges | crates/vesper-flux/**; lean/FluxDbsp.lean |
| 19 | [Fiber ABI: lifecycle and fp-env tests on macOS and Windows, arena write tracking and deep register unwinding](19-fiber-abi-portable-rest.md) | #468 | §12, §31 | after PR #441 (fiber-abi flake fix) merges | crates/vesper-fiber-abi/**; crates/vesper-chrono/** (additive) |
| 20 | [Bridge in the compiler: .vsys imports, Ffi[Lib] capabilities, native calls and isolated(mpk) trampolines](20-bridge-native-lowering.md) | #469 | §13 | now; merge after PR #384 (src/native.rs) and PRs #379/#375 (src/check.rs) | NEW src/ffi_lowering.rs and src/ffi_lowering/**; Narrow hooks in src/check.rs and src/native.rs (own commits); NEW tests/ffi_lowering*.rs and tests/fixtures/ffi_lowering/** |
| 21 | [Pulse: multi-core work stealing, Windows AFD socket polling and non-blocking synchronous handles](21-pulse-work-stealing.md) | #470 | §12 | now | crates/vesper-pulse/** |
| 22 | [Quarry: latch coupling, leaf merge, secondary indexes, serializable isolation and streaming joins](22-quarry-concurrency-indexes.md) | #471 | §24 | now | crates/vesper-quarry/**; lean/QuarryWal.lean and lean/QuarryWal/** |
| 23 | [Kinetic: joints, CCD, sleeping, articulated and soft bodies, and contact gradients for boxes and hulls](23-kinetic-joints-ccd.md) | #472 | §36 | after Codex's PR #434 (Kinetic clippy fix) merges or closes | crates/vesper-kinetic/**; lean/KineticColoring.lean and lean/KineticColoring/** |
| 24 | [Aegis: SGX quote v3, SEV-SNP report v6 and CoRIM reference-value appraisal for CCA](24-aegis-sgx-snp6-corim.md) | #473 | §33 | now | crates/vesper-aegis/**; lean/AegisCollateral.lean |
| 25 | [Aura GTK 4: GLib main-loop and Pulse co-scheduling, layouts, CSS, accessibility checks and screenshots under Xvfb](25-aura-gtk-depth.md) | #474 | §18 | now | crates/vesper-aura-gtk/**; .github/workflows/aura-gtk.yml |
| 26 | [Silicon: RISC-V AIA (APLIC/IMSIC), MSIs, multiple PLICs, dma-ranges and the SiFive cache controller](26-silicon-aia-dma.md) | #475 | §26 | now | crates/vesper-silicon/**; lean/SiliconFrames.lean |
| 27 | [Aether: virtio queue features, seccomp in the VMM thread, hot-set persistence and huge-page memfds](27-aether-virtio-depth.md) | #476 | §37 | now | crates/vesper-aether/**; lean/AetherMemory.lean |

Lanes that wait: 18 starts after PR #381 (Flux, wave-seven lane 12); 19 after
PR #441 (fiber-abi flake fix); 23 after Codex's PR #434 (Kinetic). Lanes 8 and
20 merge after PR #384 (wave-seven lane 23, `src/native*.rs`); lanes 10, 13 and
20 merge their `src/check.rs` hooks after PRs #379 and #375; lane 14 merges after
PR #379, which also edits `src/wasm_backend`. Lane 17 merges after lane 16 if
both changed `crates/vesper-doc`.

## Not in this wave

- **Other agents (Codex)** hold §16.4 attribute and item macros, the §39
  formatter (#170, #214), and the §42 rewrite witnesses, optimizer
  certificates, resource model and Lean4Lean replay (#402-#448). Everything
  that needs item or attribute macros still waits: `@choreography`,
  `@svd_pac`, `@btf_core`, `@sql`, `@wire_grammar`, serde derives,
  `@fuzz_prop`, `@differential_shadow`, `@realtime(...)` arguments, `@bench`,
  `@test`, and `@fip` if it needs macro expansion.
- **Excluded by the owner**: the eBPF linear `BpfRingSlot`/`BpfSocketRef`
  builder (PR #165).
- **Open wave-seven lanes** (Beacon #324, Prism #331, Flux #335, Crucible #342,
  Tensor #343, Nexus #345, native #346, Windows #347, eBPF backend #349,
  freestanding #350, Vector #351, capability effects #353, Ignis Vulkan #356,
  GPU compute #357) keep their paths until they merge.
- **Deferred by the owner (hardware without a stand-in)**: Aura WinUI 3 and
  SwiftUI; Ignis DirectX 12 and Metal; real-GPU performance; TEE evidence on
  real guests; KVM execution and unikernel images; RDMA NICs; Intel PT and
  CoreSight capture; hardware PMU counters; AF_XDP/DPDK and NIC offloads;
  RISC-V boards; audio and MIDI drivers.
- **Vault vector NTT for ML-KEM**: it would hand-roll a cryptographic
  primitive, which the shared rules forbid; it waits for an owner decision.
