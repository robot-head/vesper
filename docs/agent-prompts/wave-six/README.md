# Sixth parallel implementation wave: depth in every started section

Status: **ready to assign**. One thread per lane; do not launch duplicate writers.
Each linked file is a complete standalone prompt. Start from a fresh
`origin/main` containing **`cda092f2f6427e23c566d7e63259000d3d6c699f`** or later. Ownership and shared rules are in
the [wave-six contracts](../../rfcs/wave-six-contracts.md); the
[section status review](../../rfcs/wave-six-review.md) shows what each lane
covers and what is left after this wave.

Source: the remainders recorded in [wave-five-review.md](../../rfcs/wave-five-review.md)
and in each lane's report under `docs/parallel-results/`, against the design
snapshot in [specification.txt](../../specification.txt) (Google Doc revision
modified 2026-11-13 04:46 UTC, 42 numbered sections plus Appendix A; Drive
`modifiedTime` was checked at planning time and matches).

Every lane except 6 edits crates that already exist, so most lanes touch the root
only through `Cargo.lock` when they add a dependency. Merge `origin/main` into a
lane branch right before merging it.

## Integration B (claimed in wave five)

| Step | Prompt | Claim | Starts | Exclusive boundary |
| --- | --- | --- | --- | --- |
| B | [CLI and compiler hooks for the wave-five crates](00-integration-b.md) | #136 | after #149 and #151 merge (#163 merged) | `src/main.rs` routing, new `src/*_commands.rs`, new verifier-hook module and narrow `src/native.rs` hooks, root deps on wave-five crates |

## Lanes

| Lane | Prompt | Claim | Design | Starts | Exclusive boundary |
| --- | --- | --- | --- | --- | --- |
| 1 | [Bridge single out-pointers and MPK/POE isolation domains](01-bridge-pointers-mpk.md) | #243 | §13.2, §13.4, §29.3 | now | crates/vesper-ffi/**; crates/vesper-migrate/** |
| 2 | [Chrono hardware trace decoding and native activation checkpoints](02-chrono-hardware-checkpoints.md) | #244 | §31 | now | crates/vesper-chrono/**; lean/ChronoReplay.lean and lean/ChronoReplay/** |
| 3 | [Kinetic: correctly rounded trig, SIMD constraint batches, LBVH, GJK/EPA and gradients](03-kinetic-depth.md) | #245 | §36 | now | crates/vesper-kinetic/**; lean/KineticColoring.lean and lean/KineticColoring/** |
| 4 | [Aegis vendor collateral: real TDX and SEV-SNP sample verification](04-aegis-vendor-collateral.md) | #246 | §33 | now | crates/vesper-aegis/**; lean/AegisToctou.lean |
| 5 | [Warp connection lifecycle: Retry, migration, coalescing, close and 0-RTT](05-warp-lifecycle.md) | #247 | §35 | now | crates/vesper-warp/**; crates/vesper-warp-tls/** |
| 6 | [Prism HTTP/3 and QPACK over Warp](06-prism-http3.md) | #248 | §17 | now | NEW crates/vesper-prism-h3/**; Root registration patch for vesper-prism-h3 |
| 7 | [Wire/Json: two-stage SIMD on-demand JSON parsing](07-serde-simd-json.md) | #249 | §20.3, §20.5 | now | crates/vesper-serde/** |
| 8 | [Lens: poisoned recovery, obligation completion, proof lenses and latency](08-lens-remainders.md) | #250 | §21 | now | crates/vesper-lsp/**; src/lsp.rs and src/lsp/** |
| 9 | [Beacon: OTel Arrow transport and PII redaction proofs](09-beacon-otap-redaction.md) | #251 | §22 | now | crates/vesper-beacon/**; crates/vesper-beacon-arrow/**; NEW lean/BeaconRedaction.lean |
| 10 | [Quarry: durable pages, WAL recovery and vectorized execution](10-quarry-durable.md) | #252 | §24 | now | crates/vesper-quarry/**; NEW lean/QuarryWal.lean |
| 11 | [Flux: durable distributed checkpoints and recovery](11-flux-durable.md) | #253 | §34 | now | crates/vesper-flux/**; lean/FluxDbsp.lean |
| 12 | [Aether: UFFD lazy page restore and KVM slot plans](12-aether-uffd-kvm.md) | #254 | §37 | now | crates/vesper-aether/**; lean/AetherMemory.lean |
| 13 | [Metronome: hardware counters, layout randomization and causal profiling](13-metronome-depth.md) | #255 | §41 | now | crates/vesper-bench/**; crates/vesper-bench-pmu/**; src/bench_commands.rs; The `vesper bench` row and the bench lines of README.md only |
| 14 | [Codex docs: proof badges, SimPulse doctests and default CLI paths](14-codex-docs-cli.md) | #256 | §40, README | now | crates/vesper-doc/**; src/doc_commands.rs, src/doctest.rs, src/doctest_commands.rs, src/tooling/doc.rs, src/tooling/doc/**; src/lint_commands.rs; README.md lines about `vesper lint` and `vesper test --doc` only |
| 15 | [Source capability effects: ConstTime, Realtime and DetFloat in the checker](15-source-capability-effects.md) | #257 | §23.2, §32.2, §36.2 | now | NEW src/capability_effects.rs; Narrow hook calls in src/check.rs; crates/vesper-syntax/** only if new syntax is unavoidable, additively; NEW tests/capability_effects*.rs and tests/fixtures/capability_effects/** |
| 16 | [Mechanized metatheory: progress and preservation for a larger fragment](16-metatheory.md) | #258 | Appendix A, §4-§6 | now | NEW lean/Metatheory/** and lean/MetatheoryFull.lean; A NEW tiny test crate or test file that checks them under VESPER_LEAN |
| 17 | [Chorus: Warp session transport, n-ary choices and duality proofs](17-chorus-depth.md) | #259 | §25 | after #151 (Chorus) merges | crates/vesper-chorus/**; lean/ChorusQuorum.lean and lean/ChorusQuorum/** |
| 18 | [eBPF: more program types, tail calls, BTF CO-RE and verifier precision](18-ebpf-depth.md) | #260 | §27 | now | crates/vesper-ebpf/**; lean/EbpfFirewall.lean |
| 19 | [Silicon: RISC-V stack analysis, SVD/Devicetree input and Zicbom DMA](19-silicon-depth.md) | #261 | §26 | after #149 (Silicon) merges | crates/vesper-silicon/**; lean/SiliconFrames.lean |
| 20 | [Crucible: virtual arenas, SimPulse timers and I/O, and DPOR exploration](20-crucible-depth.md) | #262 | §29 | now | crates/vesper-crucible/**; lean/CrucibleShrink.lean and lean/CrucibleShrink/** |
| 21 | [Tensor: end-to-end CPU inference with paged attention](21-tensor-inference.md) | #263 | §28 | now | crates/vesper-tensor/** |
| 22 | [Pulse fibers on Windows x86_64](22-windows-fibers.md) | #264 | §12 | now | Windows-specific modules in crates/vesper-fiber-abi/** and crates/vesper-pulse/** |
| 23 | [Crucible native coverage instrumentation in the Cranelift backend](23-crucible-native-coverage.md) | #265 | §29, §7 Stage 5 | after integration B (#136) merges | NEW src/native_coverage.rs; Narrow hook lines in src/native.rs behind an explicit flag; Narrow `vesper fuzz` flag wiring in the command module integration B adds |
| 24 | [Run Stage-5 verifiers from source capability effects](24-effects-to-verifiers.md) | #266 | §23, §32, §36, §7 Stage 5 | after lane 15 and integration B (#136) merge | The verifier-hook module integration B adds; NEW tests/capability_verifiers*.rs |

Lanes 1-16, 18 and 20-22 can start now. Lane 17 waits for Chorus (#151) and
lane 19 for Silicon (#149). Lanes 23 and 24 are phase two: they build on the
hook module integration B adds, and lane 24 also on lane 15.

## Not in this wave

- Other agents (Codex) hold §42 verified bootstrap and rewrite witnesses
  (#230-#242, PR #238). No wave-six lane edits `src/optimize*` or those Lean files.
- The eBPF linear `BpfRingSlot`/`BpfSocketRef` builder (PR #165): an automated
  safety filter stopped two threads on it and the owner excluded it.
- Work that needs Forge attribute/item macros, which the source macro system
  does not support yet: `@choreography`, `@svd_pac`, `@btf_core`, `@sql`,
  `@wire_grammar`, serde derives, `@fuzz_prop`, `@differential_shadow`.
- New compiler backends (Wasm for Nexus, eBPF from source, Silicon freestanding,
  Vector `Isa[P]` lowering) and source Strata lifetimes: they all touch native
  lowering or the checker that lanes B, 15, 23 and 24 change in this wave.
- Hardware that cloud and CI hosts lack: Aura (WinUI 3, GTK 4, SwiftUI) and
  Ignis (DirectX 12, Vulkan, Metal) backends, real TEEs, RDMA NICs, KVM
  execution, io_uring on a permitting host. Lanes that touch these report them
  unavailable; running them for real needs the user's hardware.
- Smaller remainders in Vault, Resonance and Nexus (listed in the review).
