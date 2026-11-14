# Section status at the start of wave eight

Planning summary, 2026-11-14, against the design snapshot in
[specification.txt](../specification.txt) (Google Doc revision modified
2026-11-13 04:46 UTC). It updates [wave-seven-review.md](wave-seven-review.md)
from the "Remaining gaps" sections of the merged wave-seven reports in
`docs/parallel-results/` and a section-by-section completion estimate (about
73% on `main` at b2bd14e, about 79% once the open wave-seven PRs merge; equal
weight per section, judgment calls, roughly ±5 points). "Later" means not
planned in wave eight; the reason is given.

| § | Section | Wave eight | Left for later, and why |
| --- | --- | --- | --- |
| 2–6, A | Core calculus and mechanized metatheory | Lanes 5 (recursion, type variables, ∀ρ, label escape), 6 (places, PCG, borrows, Zero-Pin), 4 (executable reference) | A link between the Lean rules and the Rust checker beyond lockstep testing |
| 7 | 5-stage architecture | Lane 2 (verified codegen); wave-seven #384, #360 open | — |
| 11 | Lean extractor | Lane 12 | Whole-package `Funs.lean`/`Specs.lean` for the self-hosted compiler |
| 12 | Pulse | Lanes 19 (portable fiber tests, write tracking), 21 (work stealing, AFD) | CET shadow stacks and fiber-local storage on Windows |
| 13 | Bridge | Lane 20 (compiler lowering, `isolated(mpk)`) | Omniglot-style isolation beyond protection keys |
| 14 | Vector | Lane 9 (VVec, SVE and RVV under QEMU); wave-seven #379 open | Source `VVec` lowering (Cranelift has no SVE) |
| 15 | Strata | Lanes 7 (collections, ShardSlab), 8 (Lean region model, AOT runtime) | `@fip` if it needs macro expansion; egglog bump coalescing |
| 16 | Pact and Forge | — | §16.4 macros (Codex) |
| 18 | Aura | Lane 25 (GTK depth) | WinUI 3, SwiftUI (deferred) |
| 21 | Lens | Lane 16 | — |
| 22 | Beacon | Lane 13 (source `Trace`, `Telemetrable`); wave-seven #389 open | — |
| 23 | Vault | — | Vector NTT (would hand-roll a primitive; needs an owner decision); register scrubbing |
| 24 | Quarry | Lane 22 | `@sql` (Forge macros), `O_DIRECT` |
| 26 | Silicon | Lane 26 (AIA, dma-ranges); wave-seven #373 open | `@svd_pac` (macros), boards |
| 28 | Tensor | Lanes 10 (source types), 11 (egglog fusion); wave-seven #393, #362 open | Real GPUs |
| 30 | Nexus | Lane 14 (Wasm rest); wave-seven #368 open | WasmFX beyond Wasmtime 50 |
| 32 | Resonance | Lane 11 (fusion of DSP chains) | Audio/MIDI drivers, `@realtime` arguments |
| 33 | Aegis | Lane 24 (SGX v3, SNP v6, CoRIM) | Evidence on real TEEs |
| 34 | Flux | Lane 18 (Kafka, live rescale) after #381 | Source operators |
| 36 | Kinetic | Lane 23 (joints, CCD, soft bodies) after #434 | Real-GPU performance |
| 37 | Aether | Lane 27 (virtio depth, seccomp) | KVM, unikernel builds (deferred) |
| 40 | Codex | Lane 17 | — |
| 41 | Metronome | Lane 15 | Hardware PMU counters (deferred) |
| 42 | Verified bootstrap | Lanes 1 (fixed point, DDC gate), 2 (direct verified codegen), 3 (self-hosted lexer/parser), 4 (Lean lockstep) | Self-hosted checker and later stages; CompCert Path B; Codex's witness and Lean4Lean work |

Sections not listed (§8–10, 17, 19, 20, 25, 27, 29, 31, 35, 38, 39) have no
wave-eight lane: they are done, held by Codex, blocked on macros, waiting on
an open wave-seven PR, or need deferred hardware.

## Proposed wave nine

Once wave eight merges: the self-hosted checker and Stage 1/2 builds (§42),
source `VVec`, Forge-macro dependents if Codex delivers item and attribute
macros, and any hardware rows the owner provides hosts for.
