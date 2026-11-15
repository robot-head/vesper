# Section status at the start of wave nine

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

This work plan records the gaps and lane boundaries used by the retained
wave-nine assignments. The manuscript now integrates the language, platform,
Chroma, and refinement designs. The table keeps the earlier section identifiers
used by those assignments; the [section map](../section-map.md) locates the
corresponding manuscript chapters. Check the live issue and branch state before
starting work or relying on a reservation.

## Status by section

| § | Section | Wave nine | Left for later, and why |
| --- | --- | --- | --- |
| 2–6, A | Core calculus and mechanized metatheory | Lane 7 (generative labels, lacks constraints, ∀α) | The link between the Lean rules and the Rust checker beyond lockstep; the reference evaluator's larger fragment (Codex is editing `lean/ReferenceEval*` for #497); the places items wave eight listed as not covered |
| 7 | 5-stage architecture | Lanes 8 (session executables), 9 (handler ABI) | — |
| 11 | Lean extractor | Lane 16 | Whole-package `Funs.lean`/`Specs.lean` for the self-hosted compiler |
| 12 | Pulse | Lanes 8, 21 (host unwinding, uffd-wp), 22 (lock-free pool, pinning, pollable fd) | Lowering `s.spawn` onto the pool with source portability checks; CET shadow stacks and Windows fiber-local storage |
| 13 | Bridge | Lane 34 (linear extern types, sink finalizers, sandbox stack, WRPKRU inspection) | Arm POE; observing what C does with pointers |
| 14 | Vector | Lanes 10 (source `VVec`, profile polymorphism), 31 (QEMU RVV fix, wider lanes) | Native SVE/RVV code generation (Cranelift has none) |
| 15 | Strata | Lanes 11 (region generics, source slices/refs/arenas), 33 (CacheBTree, ChunkSeq, VirtualArena regions) | `tiled(N)` source structs, `@fip`, egglog bump coalescing |
| 16 | Pact and Forge | — | §16.4 macros (Codex) |
| 17 | Prism | Lane 32 (full-stack WebTransport/HTTP/3 interop) | Server push (refused by design) |
| 18 | Aura | Lane 23 (libadwaita, subtrees in rows, a11y checks, fd fusion) | WinUI 3, SwiftUI (deferred); Lean WCAG proofs |
| 19 | Ignis | Lane 28 | DirectX 12, Metal (deferred); ray tracing |
| 20 | Wire/Json | Lane 31 (f32 decoding, RVV path, Lean UTF-8 tables) | Source derives (macros) |
| 21 | Lens | Lane 17 (incremental, editor profile, cancellation, Chroma tokens) | — |
| 22 | Beacon | Lane 26 | `@pii`, `@span_schema` (macros); native/Wasm `Trace` lowering |
| 23 | Vault | — | Register scrubbing (needs asm in a vetted path); `eff [Isa[P]]` typing of libcrux calls |
| 24 | Quarry | Lane 24 | `@sql` (macros) |
| 25 | Chorus | — | `@choreography` (macros), RDMA hardware, third-party QUIC peer for Chorus |
| 26 | Silicon | Lanes 15 (freestanding rest), 30 (AIA model, hypervisor delivery) | `@svd_pac`, `@stack_limit` (macros), boards |
| 27 | eBPF | Lane 14 | `@btf_core` (macros); linear builder excluded |
| 28 | Tensor | Lanes 12 (source rest, `wire` flag), 27 (quantization, tokenizers, GPU kernels) | Real GPUs; egglog fusion is wave-eight lane 11 (PR #487) |
| 29 | Crucible | Lane 19 (source fibers on the step machine) | `@fuzz_prop`, `@differential_shadow` (macros), SMT-backed solving |
| 30 | Nexus | Lane 13 (WASI stdin/filesystem, lists, continuations) | WasmFX beyond Wasmtime 50 |
| 32 | Resonance | — | Slab-packed delay lines and SIMD ramps (small; next wave), audio/MIDI drivers, `@realtime` arguments |
| 33 | Aegis | — | Evidence on real TEEs (deferred) |
| 34 | Flux | Lane 20 (idempotent producer, transactions, groups, TLS/SASL, per-bin migration) | Source operators |
| 35 | Warp | Lane 32 (remaining lifecycle rules) | AF_XDP/DPDK and offloads (hardware) |
| 36 | Kinetic | Lane 25 | Real-GPU performance |
| 37 | Aether | Lane 29 (Linux guest drivers under QEMU TCG) | KVM, unikernel builds (deferred) |
| 40 | Codex | Lane 18 | WCSD stack bounds for Hard-Realtime |
| 41 | Metronome | — | Hardware PMU counters (deferred); `@bench` (macros); `--asm` diff view (next wave) |
| 42 | Verified bootstrap | Lanes 3 (parser rest), 4 (self-hosted checker), 5 (Stage 1 lock), 6 (codegen breadth) | Stage 2/3 self-compilation (needs the self-hosted checker and a self-hosted backend), CompCert Path B, Codex's witness and Lean4Lean work |
| 43 | Chroma | Lanes 1 (highlighter, palettes, HTML, wasm, Lean), 2 (`vesper shot`) | Website integration (the owner's site) |

Sections not listed (§1, 8–10, 31, 38, 39) have no wave-nine lane: they are
prose, done, held by Codex or blocked on macros, or need deferred hardware.

## Proposed wave ten

Once wave nine merges: Stage 2 self-compilation (§42) on top of the self-hosted
checker, lowering Pulse spawns onto the work-stealing pool, Resonance and
Metronome follow-ups, the Lean reference evaluator's larger fragment once
Codex's #497 work lands, Forge-macro dependents if Codex delivers item and
attribute macros, and website integration of Chroma if the owner wants it.
