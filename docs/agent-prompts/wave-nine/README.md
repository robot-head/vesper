# Ninth parallel implementation wave: Vesper Chroma, self-hosting Stage 1 and the remaining depth

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../../manuscript/README.md) is the sole specification; use the
> [section map](../../section-map.md) for earlier numbering. Confirm live ownership before acting.

These assignments are retained for open claims and branch context. Check each
claim on the live board before starting or assigning a lane; an existing prompt
does not imply that its issue is unclaimed or unstarted.

Each linked file is a complete standalone prompt. Keep its baseline, ownership,
dependencies, and acceptance requirements. The
[wave-nine contracts](../../rfcs/wave-nine-contracts.md) record shared boundaries,
and the [status review](../../rfcs/wave-nine-review.md) describes the planned gaps.
The [manuscript](../../manuscript/README.md) is the sole specification; earlier
section identifiers in the assignments are mapped in the
[section map](../../section-map.md).

Lanes 3-7 continue §42 and the metatheory (self-hosted parser and checker,
the Stage 1 lock, verified codegen breadth, generative handler labels).
Lanes 8-19 are compiler and toolchain work. Lanes 20-34 finish the gaps
recorded in existing crates.

Lanes 1 and 8 add workspace members (`vesper-highlight`, `vesper-pulse-rt`);
their merges go one at a time. Several lanes touch files that Codex holds for
open CI repairs; the Starts column and the contracts say which hunk waits for
which PR.

## Lanes

| Lane | Prompt | Claim | Design | Starts | Exclusive boundary |
| --- | --- | --- | --- | --- | --- |
| 1 | [Vesper Chroma: Tier 0/Tier 1 semantic highlighting, OKLCH palettes with contrast audits, `vesper highlight --html` and a wasm32 build](01-chroma-highlight-core.md) | #578 | §43, §21, §40 | now | NEW crates/vesper-highlight/** (everything except src/shot/**, which lane 2 owns once this merges); NEW src/highlight_commands.rs and one routing hunk for `vesper highlight` in src/main.rs (own commit); NEW lean/ChromaHighlightSafety.lean; NEW tests/highlight*.rs and tests/fixtures/highlight/**; Root registration patch for vesper-highlight (own commit) |
| 2 | [Vesper Chroma: `vesper shot` vector SVG and 4x PNG code cards with ligature shaping, callout pins and verified examples](02-chroma-shot-cards.md) | #579 | §43 | after lane 1 (vesper-highlight) merges | crates/vesper-highlight/src/shot/** (new module; lane 1 owns the rest of the crate); Dependency lines for this module in crates/vesper-highlight/Cargo.toml (own commit); NEW src/shot_commands.rs and one routing hunk for `vesper shot` in src/main.rs (own commit); NEW tests/shot*.rs and tests/fixtures/shot/**; NEW assets/fonts/** for one bundled programming font under the SIL Open Font License, with its licence file |
| 3 | [Self-hosted front end: traits, models and impls in the Vesper-source parser, a token buffer, recovery and diagnostic text](03-self-hosted-parser-rest.md) | #580 | §42.3, §38, §3 | now | packages/vesper-compiler/src/{lexer,cursor,expressions,types_items,driver}.ves (the existing parser files) and NEW parser files under packages/vesper-compiler/src/ whose names start with `parse_` or `syntax_`; tests/self_host.rs, NEW tests/self_host_parse*.rs, tests/fixtures/self_host/** |
| 4 | [Self-hosted Stage 1 checker: name resolution, modes and effect rows for the core subset, written in Vesper and run differentially against `vesper check`](04-self-hosted-checker.md) | #581 | §42.3, §4-§6 | now | NEW packages/vesper-compiler/src/ files whose names start with `ast_`, `resolve_` or `check_`; NEW tests/self_host_check*.rs and tests/fixtures/self_host_check/** |
| 5 | [Bootstrap Stage 1: build the self-hosted front end with the seed and verified codegen, record the Stage 1 lock and DDC-check it](05-bootstrap-stage1-lock.md) | #582 | §42.2, §42.5 | now | bootstrap/** (driver, corpus, lock files); crates/vesper-codegen-verified/**; NEW lean/BootstrapStage1.lean if a model is added (lean/BootstrapDdc* stays as is unless extended additively) |
| 6 | [Verified direct codegen: enums and match, floats, strings, an RV64 target and Windows PE output](06-verified-codegen-breadth.md) | #583 | §42.1 item 3, §7 | now | src/verified_codegen.rs and src/verified_codegen/**; lean/VerifiedCodegen.lean and lean/VerifiedCodegen/**; tests/verified_codegen*.rs and tests/fixtures/verified_codegen/** |
| 7 | [Mechanized metatheory: generative handler labels, lacks constraints and first-class type variables](07-metatheory-generative-labels.md) | #584 | Appendix A, §4-§6 | now | lean/Metatheory/** and lean/MetatheoryFull.lean; tests/metatheory_full.rs and NEW tests/metatheory_labels*.rs |
| 8 | [Pulse in executables: a linkable fiber and driver runtime library, and session executables on Linux, macOS and Windows](08-pulse-session-executables.md) | #585 | §12, §7 | now | NEW crates/vesper-pulse-rt/** (a `staticlib` runtime: fiber switch from vesper-fiber-abi, the portable Pulse drivers, and a driver `main`); src/native_windows.rs; A narrow hunk in src/native_session.rs for the link step (own commit, after #576); NEW tests/session_exe*.rs and tests/fixtures/session_exe/**; Root registration patch for vesper-pulse-rt (own commit) |
| 9 | [Native handler ABI: resuming user effects and closures passed at run time without specialization](09-native-handler-abi.md) | #586 | §6, §7, §12 | now | NEW src/native_handlers.rs and src/native_handlers/**; A narrow hunk in src/native.rs (own commit, after #576); NEW tests/native_handlers*.rs and tests/fixtures/native_handlers/** |
| 10 | [Source scalable vectors: `VVec[P, T]`/`VMask[P, T]` strip-mined with predicated tails, and profile-polymorphic kernels](10-source-vvec-profiles.md) | #587 | §14 | now | src/vector.rs and src/vector/**; src/native_vector.rs; Additive syntax in crates/vesper-syntax/** only if unavoidable (own commit); NEW tests/vvec*.rs and tests/fixtures/vvec/** |
| 11 | [Source Strata: region generic parameters with renaming at calls, RegionSlice/RegionRef/SlotArena in source and linear payload tracking](11-strata-region-generics.md) | #588 | §15 | now | src/regions.rs and src/regions/**; lean/StrataRegions.lean and lean/StrataRegions/**; Additive syntax in crates/vesper-syntax/** for `[r: Region]` parameters (own commit); A narrow hook in src/check.rs (own commit, after #567); tests/regions*.rs and tests/fixtures/regions/** |
| 12 | [Source Tensor rest and wire structs: record `wire` in the item schema, tensors in impls and traits, named axes and region-backed views](12-source-tensor-rest.md) | #589 | §28.2, §13.2 | now | src/tensor_types.rs and src/tensor_types/** (including the wire layout checks in src/tensor_types/wire.rs); Additive `wire` flag in the frozen item schema in crates/vesper-syntax/** (own commit); tests/tensor_types*.rs and tests/fixtures/tensor_types/** |
| 13 | [Wasm target: WASI stdin and filesystem, list parameters, loop reclamation and first-class continuations](13-wasm-backend-wasi.md) | #590 | §30, §7 | now | src/wasm_backend.rs and src/wasm_backend/**; tests/wasm_backend*.rs and tests/fixtures/wasm_backend/** |
| 14 | [eBPF: wire-struct packet views, packet writes, XDP_TX and redirect, tracepoint and kprobe entries, and the library's remaining helpers](14-ebpf-packets-tracing.md) | #591 | §27 | now | src/ebpf_backend.rs, src/ebpf_backend/**, src/ebpf_commands.rs; crates/vesper-ebpf/**; lean/ files this lane adds for the eBPF model (NEW names starting with `Ebpf`); NEW tests/ebpf_backend_packets*.rs and tests/fixtures/ebpf_backend/** additions |
| 15 | [Freestanding images: `vesper build --target`, entry parameters, interrupts with an IrqGuard, the AArch64 MMU and a stack guard](15-freestanding-images-rest.md) | #592 | §26, §7 | now | src/freestanding/** and src/native_freestanding.rs; One routing hunk for `vesper build --target` in src/main.rs (own commit); NEW tests/freestanding_rest*.rs and tests/fixtures/freestanding/** additions |
| 16 | [Lean extractor: a generated simp set, partial-def/companion equivalence, `decreases`, and contracts over inout, generic and multi-Fail functions](16-lean-extractor-proofs.md) | #593 | §11 | now | src/lean.rs, src/lean_ir.rs, src/lean_nodes.rs; crates/vesper-lean-syntax/** (additive); tests/lean_extract*.rs and tests/fixtures/lean_extract/** |
| 17 | [Lens: incremental rechecking under the 2 ms budget, generics/closures/traits in the editor, cancellation, and Chroma semantic tokens](17-lens-incremental.md) | #594 | §21, §43.2 | now | crates/vesper-lsp/**; src/lsp.rs and src/lsp/** except src/lsp/lens_tests.rs (Codex holds it for #575) |
| 18 | [Codex docs: alias-aware may-use footprints, cross-file links, proof-audit into the store, and Funs/Specs expansion](18-codex-docs-alias-links.md) | #595 | §40 | now | crates/vesper-doc/**; src/doc_commands.rs, src/doctest.rs, src/doctest_commands.rs, src/tooling/doc.rs, src/tooling/doc/** |
| 19 | [Crucible: Vesper source fibers on the DPOR step machine (`vesper test --sim`) and multi-worker stealing in SimPulse](19-crucible-source-fibers.md) | #596 | §29, §12.6 | now | crates/vesper-crucible/**; src/crucible_commands.rs; lean/CrucibleDpor.lean and lean/CrucibleDpor/**; NEW tests/sim_fibers*.rs and tests/fixtures/sim_fibers/** |
| 20 | [Flux: Kafka idempotent producer and transactions, consumer groups, TLS/SASL, compression and per-bin live migration](20-flux-kafka-exactly-once.md) | #597 | §34 | now | crates/vesper-flux/**; lean/FluxDbsp.lean; .github/workflows/flux-kafka.yml |
| 21 | [Fiber ABI: unwinding into host frames, vector callee-saved registers on every frame and userfaultfd write-protect tracking](21-fiber-host-unwinding.md) | #598 | §12, §31 | now | crates/vesper-fiber-abi/** except crates/vesper-fiber-abi/tests/unwind.rs (Codex holds it for #572; add NEW test files instead); crates/vesper-chrono/** (additive) |
| 22 | [Pulse pool: lock-free work-stealing deques, CPU pinning, per-worker buffers, a long-lived pool and a pollable driver fd](22-pulse-pool-depth.md) | #599 | §12.4-§12.5 | after Codex's PR for #573 (crates/vesper-pulse/src/pool.rs and tests/pool.rs) merges | crates/vesper-pulse/** (after #573) |
| 23 | [Aura GTK 4: libadwaita containers, Aura subtrees in virtual-list rows, keyboard and contrast accessibility checks, and GLib/Pulse fd fusion](23-aura-gtk-adwaita.md) | #600 | §18 | now | crates/vesper-aura-gtk/**; .github/workflows/aura-gtk.yml |
| 24 | [Quarry: index selection in the SQL planner, online DDL and index builds, O_DIRECT and benchmarks](24-quarry-planner-ddl.md) | #601 | §24 | now | crates/vesper-quarry/**; lean/QuarryWal.lean and lean/QuarryWal/** |
| 25 | [Kinetic: motors and joint friction, Featherstone articulation, multi-threaded RigidWorld, soft-rigid coupling and GPU broadphase in World](25-kinetic-articulation-coupling.md) | #602 | §36 | now | crates/vesper-kinetic/**; lean/KineticColoring.lean and lean/KineticColoring/** |
| 26 | [Beacon: gRPC and Arrow compression, an HPACK decoder, gauges, sums and exponential histograms, exemplars, TLS and structured attributes](26-beacon-otlp-complete.md) | #603 | §22 | now | crates/vesper-beacon/**; crates/vesper-beacon-arrow/**; lean/BeaconRedaction.lean and lean/BeaconRedaction/** |
| 27 | [Tensor: llama.cpp-exact Q4_K, Q8 activations, GPT-2 byte-level BPE and tokenizer.json, MXFP8, and quantized GPU kernels on lavapipe](27-tensor-quant-gpu.md) | #604 | §28 | now | crates/vesper-tensor/**; crates/vesper-gpu-compute/** (Tensor kernels; keep the broadphase API stable for lane 25); .github/workflows/gpu-compute.yml |
| 28 | [Ignis Vulkan: depth/stencil and more formats, swapchains under Xvfb, split and sub-range barriers, queue-family transfers and sync-fd polling](28-ignis-vulkan-depth.md) | #605 | §19 | now | crates/vesper-ignis-vulkan/**; .github/workflows/ignis-vulkan.yml |
| 29 | [Aether: virtio devices served to real Linux guest drivers under QEMU, net RSS, ring reset and vsock SEQPACKET](29-aether-qemu-guests.md) | #606 | §37 | now | crates/vesper-aether/**; lean/AetherMemory.lean; NEW .github/workflows/aether-qemu.yml |
| 30 | [Silicon: Lean model of AIA and dma-ranges, guest interrupt files, PCI MSI maps and IMSIC delivery from the APLIC](30-silicon-aia-hypervisor.md) | #607 | §26 | now | crates/vesper-silicon/**; lean/SiliconFrames.lean and NEW lean/SiliconAia.lean |
| 31 | [Scalable vectors and JSON: the RVV SIGILL under QEMU 10, wider element types and LMUL>1, f32 float decoding and an RVV JSON path](31-simd-serde-scalable-rest.md) | #608 | §14.3, §20 | now | crates/vesper-simd/**; crates/vesper-serde/**; .github/workflows/scalable-vectors.yml |
| 32 | [Prism and Warp interop: WebTransport against wtransport and headless Chromium, HTTP/3 against h3, and the remaining Warp lifecycle rules](32-quic-interop.md) | #609 | §17, §35 | now | crates/vesper-prism/**; crates/vesper-prism-h3/**; crates/vesper-warp/** |
| 33 | [Strata collections: CacheBTree and ChunkSeq, VirtualArena-backed regions with 2 MiB hints, Windows reservations and a lock-free remote queue](33-strata-collections-rest.md) | #610 | §15 | now | crates/vesper-alloc/**; lean/StrataCollections.lean |
| 34 | [Bridge: linear extern types and sink finalizers in .vsys, pointer results and narrow integers, `vesper build` wiring and a sandbox stack](34-bridge-linear-extern.md) | #611 | §13 | now | crates/vesper-ffi/**; src/ffi_lowering.rs and src/ffi_lowering/**; One routing hunk for `.vsys` imports in `vesper build` in src/main.rs (own commit); tests/ffi_lowering*.rs and tests/fixtures/ffi_lowering/** |

Waits: lane 2 starts after lane 1 merges; lane 17's semantic-token step also
waits on lane 1; lane 23's GLib/Pulse fd fusion waits on lane 22; lane 22
waits on Codex's PR for #573 (`crates/vesper-pulse/src/pool.rs`); lanes 8 and
9 merge their `src/native_session.rs` / `src/native.rs` hunks after Codex's PR
for #576; lanes 10 and 11 merge any `src/check.rs` hook after Codex's change
for #567. Lanes 10, 11 and 12 add additive syntax in `crates/vesper-syntax`:
each in its own commit, and the later one rebases.

## Not in this wave

- **Other agents (Codex)** hold §16.4 attribute and item macros, the §39
  formatter (#170, #214), the §42 rewrite witnesses, optimizer certificates,
  resource model and Lean4Lean replay (#402-#560), and the CI repairs
  #567-#576. Everything that needs item or attribute macros still waits:
  `@choreography`, `@svd_pac`, `@btf_core`, `@sql`, `@wire_grammar`, serde
  derives, `@fuzz_prop`, `@differential_shadow`, `@realtime(...)` arguments,
  `@bench`, `@test`, `@pii`, `@span_schema`, `@stack_limit` and `@fip` if it
  needs macro expansion. Chroma reads `@shot` as a directive in source text,
  not as a compiled attribute.
- **Egglog fusion** (wave-eight lane 11, #460, PR #487) is still open; no lane
  duplicates it.
- **The Lean reference evaluator** (`lean/ReferenceEval*`) is being changed by
  Codex for #497; its fragment extension (closures, state, regions) waits.
- **Deferred by the owner (hardware without a stand-in)**: Aura WinUI 3 and
  SwiftUI; Ignis DirectX 12 and Metal; real-GPU performance; TEE evidence on
  real guests; KVM execution and unikernel images; RDMA NICs; Intel PT and
  CoreSight capture; hardware PMU counters; AF_XDP/DPDK and NIC offloads;
  RISC-V boards; audio and MIDI drivers.
- **The website** (`website/**`) is the owner's; Chroma lanes build the
  highlighter, cards and wasm module but do not edit the site.
- **Excluded by the owner**: the eBPF linear `BpfRingSlot`/`BpfSocketRef`
  builder (PR #165).
