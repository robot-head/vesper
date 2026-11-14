# Seventh parallel implementation wave: the compiler targets and the remaining library depth

Status: **ready to assign**. One thread per lane; do not launch duplicate writers.
Each linked file is a complete standalone prompt. Start from a fresh
`origin/main` containing **`c025b9abb3f90c518b9634143c8bc712b9ea9c30`** or later. Ownership and shared rules are in
the [wave-seven contracts](../../rfcs/wave-seven-contracts.md); the
[section status review](../../rfcs/wave-seven-review.md) shows what each lane
covers and what is left after this wave.

Source: the "Remaining gaps" of every wave-six report under
`docs/parallel-results/` and the "Proposed wave seven" list in
[wave-six-review.md](../../rfcs/wave-six-review.md), against the design snapshot
in [specification.txt](../../specification.txt) (Google Doc revision modified
2026-11-13 04:46 UTC; Drive `modifiedTime` was checked on 2026-11-14 and still matches).

Lanes 1-22 add depth to host-library crates that already exist. Lanes 23-30
work in the root compiler: the native backend (23, 24), three new targets
(Wasm 25, eBPF 26, freestanding Silicon 27) and three source-language features
(Vector 28, Strata regions 29, capability effects 30). No lane adds a workspace
member, so most lanes touch the root only through `Cargo.lock` when they add a
dependency; those merges are still serialized (merge `origin/main` into the lane
branch right before merging, regenerate the lockfile with cargo, wait for CI).

## Lanes

| Lane | Prompt | Claim | Design | Starts | Exclusive boundary |
| --- | --- | --- | --- | --- | --- |
| 1 | [Beacon: tonic gRPC transport and OpenTelemetry Collector interop](01-beacon-grpc-collector.md) | #324 | §22 | now | crates/vesper-beacon/**; crates/vesper-beacon-arrow/**; lean/BeaconRedaction.lean and lean/BeaconRedaction/** |
| 2 | [Pulse: portable completion drivers (IOCP, kqueue, epoll) behind one scheduler](02-pulse-portable-drivers.md) | #325 | §12 | now | crates/vesper-pulse/** |
| 3 | [Fiber stack images and Chrono byte-image checkpoints](03-fiber-stack-images.md) | #326 | §12, §31 | now | crates/vesper-fiber-abi/** (additive API on every platform); crates/vesper-chrono/**; lean/ChronoReplay.lean and lean/ChronoReplay/** |
| 4 | [Aegis: AMD KDS CRLs, VLEK, Genoa/Turin and Arm CCA samples, newer report formats](04-aegis-collateral-rest.md) | #327 | §33 | now | crates/vesper-aegis/**; lean/AegisToctou.lean and NEW lean/AegisCollateral.lean |
| 5 | [Kinetic: SIMD constraint batches, parallel colours, rigid-body contacts and a broadphase kernel](05-kinetic-batches-contacts.md) | #328 | §36 | now | crates/vesper-kinetic/**; lean/KineticColoring.lean and lean/KineticColoring/** |
| 6 | [Bridge: wire-struct and Option slices, nullable pointers in .vsys](06-bridge-vsys-rest.md) | #329 | §13 | now | crates/vesper-ffi/**; crates/vesper-migrate/** (additive only) |
| 7 | [Warp: 1-RTT coalescing, CID retirement, idle timer and server migration](07-warp-lifecycle-rest.md) | #330 | §35 | now | crates/vesper-warp/**; crates/vesper-warp-tls/**; crates/vesper-warp-shard/** (additive only) |
| 8 | [Prism: WebTransport, HTTP Datagrams, priorities and streaming bodies](08-prism-webtransport.md) | #331 | §17 | now | crates/vesper-prism-h3/**; crates/vesper-prism/** (additive only) |
| 9 | [Wire/Json: wider SIMD paths, Eisel-Lemire floats and a fuller Lean model](09-serde-json-rest.md) | #332 | §20 | now | crates/vesper-serde/** |
| 10 | [Lens: deeper recovery, pattern obligations and real proof lenses](10-lens-rest.md) | #333 | §21 | now | crates/vesper-lsp/**; src/lsp.rs and src/lsp/** |
| 11 | [Quarry: B+Tree pages, MVCC, group commit and a Lean WAL model](11-quarry-btree-mvcc.md) | #334 | §24 | now | crates/vesper-quarry/**; NEW lean/QuarryWal.lean and lean/QuarryWal/** |
| 12 | [Flux: durable sources and sinks, partial recovery and rescaling](12-flux-sources-recovery.md) | #335 | §34 | now | crates/vesper-flux/**; lean/FluxDbsp.lean |
| 13 | [Aether: shared-memfd minor faults, hot-set prefetch and microVM devices](13-aether-devices.md) | #336 | §37 | now | crates/vesper-aether/**; lean/AetherMemory.lean |
| 14 | [Codex: FieldTree heatmaps, local Hoogle search, .olean links and JIT doctests](14-codex-docs-rest.md) | #337 | §40 | now | crates/vesper-doc/**; src/doc_commands.rs, src/doctest.rs, src/doctest_commands.rs, src/tooling/doc.rs, src/tooling/doc/**; README.md: the `vesper doc` row and the build line (`cargo build --release -p vesper-cli` names a package that does not exist); src/main.rs: the help text only |
| 15 | [Mechanized metatheory: functions, handler state and effect rows](15-metatheory-closures.md) | #338 | Appendix A, §4-§6 | now | lean/Metatheory/** and lean/MetatheoryFull.lean; tests/metatheory_full.rs (and NEW tests/metatheory_*.rs) |
| 16 | [Chorus: TLS sessions over Warp, recursive duality and a durable Raft log](16-chorus-handshake-raft.md) | #339 | §25 | now | crates/vesper-chorus/**; lean/ChorusQuorum.lean and lean/ChorusQuorum/** |
| 17 | [eBPF library: run every program type, tail calls, more kfuncs and CO-RE](17-ebpf-library-rest.md) | #340 | §27 | now | crates/vesper-ebpf/**; lean/EbpfFirewall.lean |
| 18 | [Silicon: devicetree nexus maps, ranges and ISA extensions](18-silicon-devicetree-rest.md) | #341 | §26 | now | crates/vesper-silicon/**; lean/SiliconFrames.lean |
| 19 | [Crucible: preemption bounding, state caching and a Lean DPOR model](19-crucible-dpor-rest.md) | #342 | §29 | now | crates/vesper-crucible/**; lean/CrucibleShrink.lean and lean/CrucibleShrink/**; NEW lean/CrucibleDpor.lean |
| 20 | [Tensor: quantized formats, safetensors/GGUF, BPE, sampling and SIMD kernels](20-tensor-formats-sampling.md) | #343 | §28 | now | crates/vesper-tensor/**; NEW lean/TensorKv.lean |
| 21 | [Resonance: BIBO and torque-clamp proofs, SIMD SVF and slab packing](21-resonance-proofs-simd.md) | #344 | §32 | now | crates/vesper-resonance/**; NEW lean/ResonanceStability.lean |
| 22 | [Nexus: WIT-to-trait generator, Pulse adapter and per-tenant rings](22-nexus-wit-pulse.md) | #345 | §30 | now | crates/vesper-nexus/** |
| 23 | [Native backend: source spans on Cranelift instructions, fuel and the capability-passing ABI](23-native-spans-fuel-abi.md) | #346 | §7 Stage 5, §29 | now | src/native.rs, src/native_batch.rs, src/native_session.rs, src/native_coverage.rs, src/native_verifiers.rs; NEW tests/native_spans*.rs |
| 24 | [Native executables on Windows (MSVC objects and linking)](24-windows-native-executables.md) | #347 | §7, §12 | now; merge after lane 23 if both touch src/native.rs (merge origin/main first) | NEW src/native_windows.rs (COFF object emission and MSVC link driver); A narrow additive target-selection hunk in src/native.rs, in its own commit; The host-native step of the Windows job in .github/workflows/platforms.yml |
| 25 | [Compiler Wasm target: wasm32-wasip2 components from Vesper source](25-wasm-backend.md) | #348 | §30, §7 | now | NEW src/wasm_backend.rs and src/wasm_backend/**; NEW tests/wasm_backend*.rs and tests/fixtures/wasm_backend/**; One routing hunk for `vesper build --target wasm32-wasip2` (own commit) |
| 26 | [Compiler eBPF target: XDP and tc programs from Vesper source](26-ebpf-backend.md) | #349 | §27, §7 | now | NEW src/ebpf_backend.rs and src/ebpf_backend/**; NEW tests/ebpf_backend*.rs and tests/fixtures/ebpf_backend/**; One routing hunk for `vesper build --target bpf` (own commit) |
| 27 | [Compiler freestanding target: bare-metal RISC-V and AArch64 images](27-silicon-freestanding.md) | #350 | §26, §7 | now | NEW src/native_freestanding.rs and src/freestanding/**; A narrow additive entry point in src/native.rs, in its own commit; NEW tests/freestanding*.rs and tests/fixtures/freestanding/** |
| 28 | [Source Vector: vector types and `eff [Isa[P]]` lowered to Cranelift SIMD](28-vector-isa-source.md) | #351 | §14 | now | NEW src/vector.rs and src/vector/** (types, checking, lowering helpers); Additive vector operations in src/ir.rs and the evaluator (own commit); NEW src/native_vector.rs plus one dispatch arm in src/native.rs; Narrow hook calls in src/check.rs; Additive syntax in crates/vesper-syntax/** only if unavoidable; NEW tests/vector*.rs and tests/fixtures/vector/** |
| 29 | [Source Strata: lexical regions and lifetime checking](29-strata-source-regions.md) | #352 | §15 | now | NEW src/regions.rs and src/regions/**; Narrow hook calls in src/check.rs; Additive syntax in crates/vesper-syntax/** only if unavoidable; NEW tests/regions*.rs and tests/fixtures/regions/** |
| 30 | [Source capability effects: secret operations, ConstDenorm, RtBus and interprocedural taint](30-capability-effects-rest.md) | #353 | §23, §32, §36 | now | src/capability_effects.rs; NEW tests/capability_effects_more*.rs and fixtures under tests/fixtures/capability_effects/**; NEW lean/CapabilityEffects.lean |

All thirty lanes can start now. Lane 24 merges after lane 23 if both changed
`src/native.rs`. Lanes 27 and 28 also add narrow hunks to `src/native.rs`, and
lanes 28 and 29 to `src/check.rs` and possibly `crates/vesper-syntax/**`; the
second to merge in each pair merges `origin/main` first.

## Not in this wave

- **Other agents (Codex)** hold §16.4 attribute and item macros and §42
  (resource model #322 / PR #323, rewrite witnesses). Everything that needs
  item or attribute macros therefore still waits: `@choreography`,
  `@svd_pac`, `@btf_core`, `@sql`, `@wire_grammar`, serde derives,
  `@fuzz_prop`, `@differential_shadow`, `@realtime(...)` arguments, `@test`.
- **Excluded by the owner**: the eBPF linear `BpfRingSlot`/`BpfSocketRef`
  builder (PR #165).
- **Metronome** (#255, PR #283) is still merging from wave six; a follow-up
  lane is planned from its report once it lands.
- **Source `Trace` propagation** (§22) and the Strata collection suite
  (`tiled(N)`/AoSoA): they change the checker and IR alongside lanes 28-30.
- **Hardware the CI and cloud hosts lack, waiting on the owner's decision**:
  Aura (WinUI 3, GTK 4, SwiftUI) and Ignis (DirectX 12, Vulkan, Metal)
  backends; Tensor and Kinetic GPU execution; real TEE evidence (TDX, SEV-SNP,
  CCA guests); KVM execution (Aether); RDMA NICs (Chorus); Intel PT/CoreSight
  capture (Chrono); hardware PMU counters (Metronome); POE on AArch64;
  io_uring on a permitting host; AF_XDP/DPDK and NIC offloads (Warp); RISC-V
  boards; audio and MIDI drivers (Resonance). Some of these can run on
  software stand-ins in Linux CI (GTK 4 under Xvfb, Vulkan on Mesa lavapipe,
  RISC-V under QEMU); lanes that touch them report what ran.
