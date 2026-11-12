[vesper_repository_readme.md](https://github.com/user-attachments/files/34738710/vesper_repository_readme.md)
# Vesper Programming Language

[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](#license)
[![Edition](https://img.shields.io/badge/edition-2026-indigo.svg)](#language-overview)
[![Backend](https://img.shields.io/badge/codegen-Cranelift%20SSA%20%2B%20WasmFX-teal.svg)](#5-stage-compiler-architecture)
[![Verification](https://img.shields.io/badge/verification-Lean%204%20(0%20sorryAx)-brightgreen.svg)](#prophecy-free-lean-4-verification-vesper-verify)
[![Workspace](https://img.shields.io/badge/workspace-45%20Rust%20crates-orange.svg)](#monorepo-workspace-architecture-45-crates)

**Vesper** is a zero-GC, formally verifiable systems programming language implemented in Rust, targeting **Cranelift SSA IR** for fast native/JIT machine code and **Lean 4** for compositional functional verification.

Vesper preserves Rust's zero-cost memory safety and data-race freedom while eliminating lifetime annotation noise (`'a`), `async`/`await` function coloring, `Pin` self-referential boilerplate, `tokio::select!` cancel-safety bugs, and unsandboxed `build.rs` / `proc_macro` supply-chain risks.

---

## Three Core Pillars

1. **Modal True Linearity ($\text{copy} \sqsubseteq \text{affine} \sqsubseteq \text{linear}$):**
   Values default to Rust-like `affine` cleanup, while resources marked `linear` (`WriteTxn`, `NonceTicket`, `PagePin`, `UmemFrame`, `ActiveSpan`) cannot be implicitly dropped or forgotten via `mem::forget`. Every control-flow path—including early `?` error returns—must explicitly consume a `linear` obligation via destructuring or a `sink self` transition that can perform async I/O and return errors, solving Rust's `!Destruct` and "Async Drop" gaps. OxCaml-style **mode crossing** (`[K-Adt]`, `[T-Cross]`) automatically strengthens generic wrappers like `Option[T]` to `copy`, `affine`, or `linear` based on `T`.
2. **One-Shot Lexical Effect Capabilities (`eff [...]`):**
   Async completion I/O (`io_uring`, `kqueue`, `IOCP`), structured concurrency (`Scope`), checked fallibility (`Fail[E]`), explicit cancellation (`Cancel`), CPU SIMD ISA profiles (`Isa[P]`), constant-time hardware execution (`ConstTime`), and lexical memory arenas (`r: Region`) are unified under stack-switched **one-shot algebraic effect handlers**. Continuations never duplicate unique pointers, eliminating `Pin`, `Future` polling state machines, and colored higher-order combinators.
3. **Second-Class Disjoint Place Projections (`in`, `inout`, `sink`):**
   Synthesizing Mutable Value Semantics (MVS) with Place Capability Graphs (PCGs), borrows are second-class parameter passing modes over field-tree selectors (`inout broker.{index, metrics}`) rather than first-class `'a`-annotated pointer types. Callers retain shared `in` access to `broker.config` across mutating calls with zero `'a` lifetime parameters, and validated functions extract 1-to-1 into prophecy-free **Lean 4** models.

---

## How Vesper Advances Beyond Rust

| Systems Dimension | Rust (2024+) | Vesper (2026) |
| :--- | :--- | :--- |
| **Protocol & Resource Finalization** | Affine (`Drop` is sync/infallible; `mem::forget` is safe; no `AsyncDrop`). | **True `linear` mode** (`Theorem 2: Total Linear Exhaustion`); `sink self` finalizers perform I/O & return `Result`. |
| **Borrowing & Aliasing** | First-class `&'a T` / `&'a mut T` lifetimes infect structs, traits, and closures. | **Second-class projections** (`in`, `inout broker.{index, metrics}`, `sink`) checked via PCG disjointness ($p_i \perp p_j$). |
| **Async & Cancel Safety** | Colored `async fn` + `Pin` state machines; `tokio::select!` drops futures at arbitrary `.await` points. | **Colorless `eff [...]` pooled fibers** (`~4-6 ns` switch); `[T-Let]` statically blocks `Cancel`/`Fail` across live `linear` values. |
| **Generic Syntax** | Angle brackets `<T>` require `::<T>` turbofish disambiguation in expressions. | **LL(2) square-bracket generics** (`foo[WriteTxn](x)` vs `arr[idx](x)`) with resilient <2 ms IDE recovery. |
| **SIMD & Memory Layouts** | Arch SIMD intrinsics require `unsafe`; AoS default; external `zerocopy` proc-macros. | **100% safe `eff [Isa[P]]` predicated SIMD (`VVec`)**, built-in `tiled(N) struct` (AoSoA), and 0-padding `wire struct`. |
| **Build Scripts & Macros** | `build.rs` and `proc_macro` execute arbitrary unsandboxed host code with network/FS access. | **Zero-`build.rs` Wasmtime (`wasm32-wasip2`) sandboxes** with fuel metering, 0 sockets, and `[T-MacroConfinement]`. |
| **Supply Chain & SemVer** | Ambient `std::fs`/`std::net` authority; human-curated SemVer; external audit plugins. | **Compile-time `Loom.toml` capability ceilings**, Sigstore anti-slopsquatting gates, and `mif:blake3` SemVer solver. |
| **Formal Verification** | Requires LLBC lifetime reconstruction (*Charon*) and backward prophecy functions (*Aeneas*). | **Built-in `vesper verify`** extracts pure state-passing Lean 4 + `.olean` proof certificates (`0 sorryAx`). |

---

## Vesper in 60 Seconds

### 1. Linear Transactions, Disjoint Field Projections & Compile-Time Cancel Safety

```vesper
// A `linear` struct cannot go out of scope unconsumed; no implicit Drop or forget() exists.
linear struct WriteTxn {
    wal_slot: u64,
    dirty_pages: BufferPoolSlice,
}

impl WriteTxn {
    // Consumes `sink self`, performing asynchronous `Storage` I/O and fallible `Fail` effects:
    fn commit(sink self) -> CommitReceipt eff [Storage, Fail[IoError]] {
        let WriteTxn { wal_slot, dirty_pages } = self;
        Storage::flush_wal(wal_slot, in dirty_pages)?;
        Storage::release_slice(sink dirty_pages);
        CommitReceipt { wal_slot }
    }

    fn abort(sink self) eff [Storage] {
        let WriteTxn { wal_slot, dirty_pages } = self;
        Storage::discard_wal(wal_slot);
        Storage::release_slice(sink dirty_pages);
    }
}

// Mutates ONLY `broker.index` and `broker.metrics`; callers freely hold `in broker.config`
// across this call with zero `'a` lifetime annotations!
pub fn append_batch(
    inout broker.{index, metrics}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Storage, Cancel, Fail[IoError]] {
    // Legal BEFORE acquiring a linear obligation (`res(Cancel) = affine`):
    Cancel::check()?;

    let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);

    // Note: Calling `validate_crc(in batch)?` directly here is a COMPILE-TIME ERROR (E0301)
    // because `tx` is `linear` and `Fail` is abortable (`linear <= affine` is false)!
    match validate_crc(in batch) {
        Ok(()) => {
            broker.index.insert_in_place(sink batch);
            broker.metrics.batches_written += 1;
            tx.commit() // Discharges `tx` on the happy path
        }
        Err(err) => {
            Storage::recycle_batch(sink batch);
            tx.abort(); // Discharges `tx` on the error path
            Fail::raise(err)
        }
    }
}
```

### 2. Prophecy-Free Lean 4 Verification (`vesper verify`)

Because `inout broker.{index, metrics}` is a second-class disjoint value projection rather than an aliased mutable pointer, `vesper verify` extracts `append_batch` directly into pure Lean 4 (`EffM`) with auto-generated `@[simp]` lens laws. Proving transactional crash atomicity takes two tactic lines:

```lean
-- proofs/WalSafety.lean (audited for 0 `sorryAx` on `vesper publish`)
theorem appendBatch_crc_err_aborts_cleanly
    (st : RefStorageModel) (proj : BrokerIndexMetricsProj)
    (cfg : BrokerConfig) (batch : RecordBatch) (crcErr : IoError)
    (h_crc : validateCrc batch = .error crcErr) :
    let (st', res) := RefStorageModel.run st (appendBatch proj cfg batch)
    res = .error crcErr /\
    st'.activeWalSlots = st.activeWalSlots /\
    st'.pinnedSlices   = st.pinnedSlices := by
  simp [appendBatch, h_crc, WriteTxn.abort, RefStorageModel.run]
  omega
```

### 3. Safe Scalable SIMD, AoSoA Cache Tiling & Zero-Global-Heap Regions

Functions omitting `Alloc` from `eff [...]` are **statically guaranteed zero-global-heap**. Local scratchpads use contiguous virtual-memory bump arenas (`Region::scoped`), while `tiled(16) struct` packs hot columns into 64-byte L1 cache lines for 100% safe predicated SIMD (`VVec[P, T]`) across SSE4.2, AVX2, AVX-512, Arm NEON/SVE2, and RISC-V RVV 1.0:

```vesper
tiled(16) copy struct LogOffsetEntry {
    hot base_offset: u64,
    hot batch_crc: u32,
    hot byte_len: u32,
    cold producer_id: u64,
    cold compression_codec: u8,
}

// Zero `unsafe`, zero global heap (`Alloc` absent from `eff [...]`), length-agnostic SIMD:
fn count_matching_crc[P: IsaProfile](
    in index.{batch_crc}: TiledSeq[LogOffsetEntry],
    target_crc: u32,
) -> u64 eff [Isa[P]] {
    let mut hits: u64 = 0;
    for tile_idx in 0..index.tile_count() {
        let crc_vec = index.load_tile_vec[P, batch_crc](tile_idx);
        let mask    = crc_vec.simd_eq(VVec::splat[P, u32](target_crc));
        hits = hits.wrapping_add(mask.count_ones() as u64);
    }
    hits
}
```

---

## 5-Stage Compiler Architecture

Vesper uses a single unified demand-driven `salsa` compiler pipeline shared between batch builds (`vesper build`) and the sub-2ms Language Server (`vesper lsp`), avoiding the `rustc` vs. `rust-analyzer` split:

```text
  .vsp Source + Lossless /// Docs + WASI Macros (wasm32-wasip2, 0 sockets, fuel-metered)
       |
       v
+---------------------------------------------------------------------------------------+
| Stage 1: Resilient Lexing & Lossless CST (`vesper-syntax`, `vesper-forge`)            |
|  - `logos` SIMD lexer + LL(2) square-bracket parser -> `rowan` Red-Green CST          |
|  - Two-way scope-set + `PreResolvedDefId` macro hygiene & `[T-MacroConfinement]`      |
+---------------------------------------------------------------------------------------+
       |  (Firewall A: `FileSignatureMif` blocks downstream invalidation on body edits)
       v
+---------------------------------------------------------------------------------------+
| Stage 2: Bidirectional Mode, Pact & Effect Inference (`vesper-hir`, `vesper-pact`)    |
|  - Lattice `copy <= affine <= linear`, OxCaml `[T-Cross]`, SLG tabled traits/models   |
+---------------------------------------------------------------------------------------+
       |
       v
+---------------------------------------------------------------------------------------+
| Stage 3: Place Capability Graph Checker (`vesper-mir-pcg`)                            |
|  - FieldTree disjointness (`p_i _|_ p_j`), `[T-Let]` Resumption Frame Condition,      |
|    `Mode::Linear` path exhaustion, and `PoisonedRecovery` IDE autocomplete absorption |
+---------------------------------------------------------------------------------------+
       |
       v
+---------------------------------------------------------------------------------------+
| Stage 4: Effectful Equality Saturation (`vesper-opt-egglog`)                          |
|  - 0 ns tail-resumptive handler fusion, `@fip` in-place constructor reuse (0 RC),     |
|    `VirtualArena` bump coalescing, SIMD FMA/dot-product & DBSP/Tensor operator fusion |
+---------------------------------------------------------------------------------------+
       |
       +----------------------------------------+---------------------------------------+
       |                                        |                                       |
       v                                        v                                       v
+------------------------------+  +-----------------------------------+  +--------------+
| Stage 5A: Native / JIT / VM  |  | Stage 5B: Lean 4 Proof Extractor  |  | WasmFX / BPF |
| (`vesper-codegen-clif`)      |  | (`vesper-verify-lean`)            |  | (`nexus` /   |
| - x86_64, AArch64, RISC-V    |  | - Lawful disjoint lenses + `EffM` |  |  `ebpf` /    |
| - `r15`/`x28` EvidenceVector |  | - `bv_decide` & `0 sorryAx` gate  |  |  `aether`)   |
+------------------------------+  +-----------------------------------+  +--------------+
```

---

## Unified Batteries-Included CLI (`vesper`)

**Vesper Loom** replaces fragmented external Cargo plugins (`cargo-vet`, `cargo-deny`, `cargo-audit`, `cargo-semver-checks`, `cargo-fuzz`, `rustfmt`, `clippy`, `rustdoc`, `tokio-console`) inside one static `vesper` binary:

| Command | Built-In Capability |
| :--- | :--- |
| `vesper new` / `vesper init` | Scaffolds a package or workspace with a strict `capability_ceiling = []` zero-authority-by-default `Loom.toml`. |
| `vesper add @org/pkg` | Enforces Sigstore OIDC lineage and edit-distance **anti-slopsquatting** gates; prompts before granting non-empty `effects`. |
| `vesper build` / `vesper check` | Runs fuel-metered Wasmtime `build.vsp` & macros, hydrates `~/.vesper/store/` CAS with `mif:blake3` early cutoff (`--unikernel`, `--tee`). |
| `vesper fmt` | **Vesper Stylus**: Zero-config, comment-complete $O(N)$ Wadler-Lindig formatter over lossless `rowan` CSTs. |
| `vesper lint` (`--fix`) | **Vesper Lint**: Substructural `FieldTree` narrowing, `Alloc`-to-`Region` demotion, `egglog` simplifications & MIF-verified autofixes. |
| `vesper doc` | **Vesper Codex**: Generates living docs with Linear Typestate graphs, `FieldTree` heatmaps, 64B wire rulers, WASM Hoogle & `.olean` badges. |
| `vesper test` | Parallel unit/property tests, `<100 ms` single-JIT-module doctests (`--doc`), deterministic `SimPulse` (`--sim`), `SimAura` (`--sim-ui`) & `--mutate`. |
| `vesper fuzz` (`--differential`) | Zero-nightly Cranelift edge-coverage + Bitwuzla/Z3 concolic fuzzing with `ChoiceTape` shrinking & 1.5 ns MPK `@differential_shadow`. |
| `vesper verify` (`--refute`) | Runs Crucible concolic counterexample pre-checks, extracts Lean 4 specs, mounts CAS `.olean` certs, and audits `0 sorryAx`. |
| `vesper semver` | Mathematically verifies modal, `FieldTree`, and effect-row contextual substitutability ($I_{\text{new}} \preceq_{\text{pub}} I_{\text{old}}$) with counterexample synthesis. |
| `vesper resolve --optimize` | Dual-engine `<50 ms` `resolvo` CDCL solver + Max-SMT optimizer minimizing CVEs, capability surface, staleness, and trust cost. |
| `vesper audit` / `vesper publish` | Reachable-call-graph OSV audit, SLSA v1.1 SBOM, Lila multi-builder reproducibility quorum, SemVer + `.olean` gates & keyless Sigstore publish. |
| `vesper migrate` | Lifts C headers and Rust crates into `.vsys` manifests, padding-free `wire struct` types, `Mode::Linear` handles, and `inout` projections. |
| `vesper top` / `vesper replay` | Zero-instrumentation live Pulse fiber & parked `Mode::Linear` obligation inspector + **Vesper Chrono** hardware IPT/CoreSight reverse debugger. |
| `vesper lsp` | Launches **Vesper Lens**, the sub-2ms `salsa` incremental Language Server with obligation-guided autocomplete. |

### Declarative Capability Confinement (`Loom.toml`)

```toml
[package]
name = "@robot-head/krabka-wal"
version = "0.4.0"
edition = "2026"
license = "Apache-2.0"
# Compile-time upper bound on lexical effects ANY module or dependency may use:
capability_ceiling = ["Storage", "Net", "Clock", "Fail"]
forbid_unsafe_ffi = true

[dependencies]
# Statically guaranteed ZERO filesystem, network, clock, global heap, or FFI access:
"@vesper-core/bytes" = { version = "^1.3.0", effects = [] }
"@vesper-io/uring-reactor" = { version = "^0.9.2", effects = ["Storage", "Fail"] }

[lints.vesper]
overbroad_inout_projection = "warn"
unused_effect_capability   = "deny"
demote_alloc_to_region     = "warn"

[verification]
export_lean4_specs = ["src/wal_txn.vsp"]
```

---

## Monorepo Workspace Architecture (45 Crates)

The repository is organized as a modular Cargo workspace (`resolver = "2"`) spanning four architectural tiers:

```text
vesper/
|-- Cargo.toml                         # Unified 45-crate Rust workspace manifest
|-- proofs/
|   `-- VesperMetatheory.lean          # Standalone zero-sorryAx Lean 4 mechanization of lambda_Vesper & PCG
`-- crates/
    |-- # === Tier 1: Stage 1-5 Compiler, Runtime & Verification Core (12 crates) ===
    |-- vesper-span/                   # Interned symbols, FileId, TextSpan & Ariadne diagnostics
    |-- vesper-syntax/                 # Logos SIMD lexer, Rowan lossless CST & resilient LL(2) parser
    |-- vesper-hir/                    # Desugared lambda_Vesper HIR, Mode lattice & bidirectional checker
    |-- vesper-mir-pcg/                # Place IR & Place Capability Graph linearity/disjointness solver
    |-- vesper-opt-egglog/             # Effectful equality saturation, @fip reuse & operator fusion
    |-- vesper-codegen-clif/           # Cranelift SSA native/JIT backend, fiber ABI & fuzz counters
    |-- vesper-pulse/                  # io_uring/kqueue/IOCP fiber runtime, SimPulse & vesper top
    |-- vesper-ffi/                    # Vesper Bridge: 0-padding wire struct, .vsys & 1.5ns MPK sandbox
    |-- vesper-simd/                   # Vesper Vector: safe predicated VVec across AVX-512/SVE2/RVV
    |-- vesper-alloc/                  # Vesper Strata: ShardSlab, VirtualArena regions & AoSoA TiledSeq
    |-- vesper-verify-lean/            # Prophecy-free Lean 4 extractor & .olean 0-sorryAx auditor
    |-- vesper-semver/                 # Merkle Interface Fingerprints (mif:blake3) & SemVer solver
    |
    |-- # === Tier 2: Vesper Loom Package Manager & Supply Chain (5 crates) ===
    |-- vesper-loom-calculus/          # ICFP 2026 Package Calculus IR & capability ceiling verifier
    |-- vesper-loom-solver/            # Resolvo CDCL (<50ms) + Bitwuzla/Z3 Max-SMT optimizer
    |-- vesper-loom-sandbox/           # Wasmtime (wasm32-wasip2) fuel-metered build.vsp & macro host
    |-- vesper-loom-cas/               # BLAKE3 + redb content-addressed store & CoW reflink materializer
    |-- vesper-loom-trust/             # Sigstore OIDC, TUF, Lila quorum & slopsquatting gate
    |
    |-- # === Tier 3: Language Abstraction & Unified Developer Tooling (9 crates) ===
    |-- vesper-pact/                   # SLG tabled canonical trait solver & type-branded named models
    |-- vesper-forge/                  # Two-way hygienic quotations & WASI macro anti-smuggling gate
    |-- vesper-lsp/                    # Vesper Lens: sub-2ms Salsa LSP & obligation autocomplete
    |-- vesper-fmt/                    # Vesper Stylus: comment-complete Wadler-Lindig CST formatter
    |-- vesper-lint/                   # Vesper Lint: substructural HIR/PCG/egglog static analyzer
    |-- vesper-doc/                    # Vesper Codex: typestate/wire docs, Hoogle & batch JIT doctests
    |-- vesper-crucible/               # Concolic SMT fuzzer, ChoiceTape shrinker & C/Rust migrator
    |-- vesper-chrono/                 # Time-travel reverse debugger, IPT/CoreSight & linear lineage
    |-- vesper-cli/                    # Unified static `vesper` toolchain binary
    |
    `-- # === Tier 4: Flagship Application, Kernel, Cloud, ML & Protocol Substrates (19 crates) ===
        |-- vesper-prism/              # @vesper-web/prism:     0-Alloc HTTP/1.1/2/3 & linear WebSockets
        |-- vesper-aura/               # @vesper-ui/aura:       Native WinUI 3, GTK 4 & SwiftUI desktop UI
        |-- vesper-ignis/              # @vesper-game/ignis:    D3D12, Vulkan 1.4 & Metal 4 3D engine
        |-- vesper-serde/              # @vesper-wire/*:        Arena Serde, On-Demand SIMD JSON & RelPtr32
        |-- vesper-beacon/             # @vesper-obs/beacon:    Linear ActiveSpan & 0-copy OTLP-Arrow
        |-- vesper-vault/              # @vesper-crypto/vault:  CT SSA taint verifier, DIT & NonceTicket PQ AEAD
        |-- vesper-quarry/             # @vesper-db/quarry:     Hermetic WASI @sql, PagePin & AoSoA query engine
        |-- vesper-chorus/             # @vesper-dist/chorus:   Choreographic consensus EPP & 0-copy RDMA
        |-- vesper-silicon/            # @vesper-hw/silicon:    Bare-metal kernels, IrqGuard, MMIO & WCSD
        |-- vesper-ebpf/               # @vesper-kernel/ebpf:   Pre-flight eBPF verifier, XDP & BpfRingSlot
        |-- vesper-tensor/             # @vesper-ml/tensor:     Symbolic shape-checked tensors & Paged-KV LLM
        |-- vesper-nexus/              # @vesper-wasm/nexus:    WasmFX sandboxes & linear WIT host/guest plugins
        |-- vesper-resonance/          # @vesper-rt/resonance:  Lock-free @realtime DSP, PDC & 1-10kHz robotics
        |-- vesper-aegis/              # @vesper-tee/aegis:     TDX, SEV-SNP & Arm CCA Iago-safe confidential VMs
        |-- vesper-flux/               # @vesper-stream/flux:   DBSP Z-Set incremental materialized stream views
        |-- vesper-warp/               # @vesper-net/warp:      AF_XDP / DPDK 100/400GbE QUIC/TCP & BBRv3
        |-- vesper-kinetic/            # @vesper-sim/kinetic:   Bit-deterministic DetFloat XPBD physics & rollback
        |-- vesper-aether/             # @vesper-cloud/aether:  <1.2ms PVH unikernels & clone-safe UFFD microVMs
        `-- vesper-grammar/            # @vesper-parse/grammar: Non-malleable binary/PEG parsers & Arrow IPC
```

---

## Quickstart & Building from Source

### Prerequisites

* **Rust Toolchain:** Rust `1.85+` (`rustup update stable`)
* **Lean 4 (Optional, for `vesper verify` and metatheory proofs):** [`elan`](https://github.com/leanprover/elan) (`leanprover/lean4:v4.16.0+`)

### Build the Unified `vesper` Toolchain

```bash
git clone https://github.com/vesper-lang/vesper.git
cd vesper

# Build the static `vesper` CLI binary in release mode
cargo build --release -p vesper-cli

# Run the compiler, PCG borrow/linearity checker, and workspace unit suites
cargo test --workspace
```

### Verify the Mechanized Core Metatheory (`0 sorryAx`)

Vesper's substructural mode lattice, OxCaml `[K-Adt]` field-modality well-formedness, `[T-Let]` Resumption Frame safety, `[T-CallProj]` Alias-XOR-Mutability, `[SV-Fn]` SemVer narrowing, and **Theorem 2 (`vesper_total_linear_exhaustion`)** are mechanized in pure core Lean 4 with zero external dependencies:

```bash
lean proofs/VesperMetatheory.lean
```

### Create, Check, Test, and Verify a Vesper Package

```bash
# 1. Scaffold a zero-authority-by-default package
./target/release/vesper new my-wal-engine
cd my-wal-engine

# 2. Format, run substructural lints, and execute single-JIT-module SimPulse doctests
vesper fmt
vesper lint --fix --semver-safe
vesper test --sim --doc

# 3. Run concolic counterexample pre-checks and extract Lean 4 .olean proof certificates
vesper verify --refute
vesper doc --open
```

---

## Specification & Implementation Roadmap

The complete formal grammar, typing rules, operational semantics, domain frameworks, and mechanized proofs are documented in the **Vesper Programming Language Specification (Sections 1–40 & Appendix A)**:

* **Sections 1–6 & Appendix A:** Research foundations, LL(2) EBNF grammar, $\lambda_{\text{Vesper}}$ modal kinding, `[T-Let]` Resumption Frame Condition, `[T-CallProj]` disjoint field trees, `[T-Handle]` one-shot lexical handlers, and `VesperMetatheory.lean`.
* **Sections 7–11:** 5-stage Cranelift + `egglog` compiler pipeline, **Vesper Loom** package calculus & WASI sandboxing, formal SemVer variance ($I_{\text{new}} \preceq_{\text{pub}} I_{\text{old}}$), and the prophecy-free **Lean 4** extractor.
* **Sections 12–16, 20–21, 29–31, 39–40:** **Pulse** async runtime, **Bridge** FFI & MPK isolation, **Vector** SIMD, **Strata** arenas & AoSoA collections, **Pact & Forge** traits/WASI macros, **Wire/Json** codecs, **Lens** LSP, **Crucible** concolic fuzzer, **Chrono** time-travel debugger, **Stylus & Lint**, and **Codex** living documentation.
* **Sections 17–19, 22–28, 32–38:** Flagship domain engines (**Prism**, **Aura**, **Ignis**, **Beacon**, **Vault**, **Quarry**, **Chorus**, **Silicon**, **eBPF**, **Tensor**, **Resonance**, **Aegis**, **Flux**, **Warp**, **Kinetic**, **Aether**, and **Grammar**).

### Bootstrap Milestones

- [x] **Specification & Metatheory:** Complete Sections 1–40 formal specification & zero-`sorryAx` `VesperMetatheory.lean` mechanization.
- [ ] **Milestone M1 (`vesper-syntax` + `vesper-hir`):** Resilient LL(2) parser, `Mode` + `EffectRow` bidirectional inference, and `WriteTxn` / `append_batch` typechecking tests.
- [ ] **Milestone M2 (`vesper-mir-pcg` + `vesper-codegen-clif`):** `PcgState` disjointness/exhaustion solver + Cranelift JIT execution of tail-resumptive `Fail` and stack-switched `Pulse` fibers.
- [ ] **Milestone M3 (`vesper-semver` + `vesper-loom-*` + `vesper-verify-lean`):** `mif:blake3` early cutoff, `resolvo` + Max-SMT solver, Wasmtime `build.vsp` sandboxes, and `.olean` certificate linking.
- [ ] **Milestone M4 (Core Std & Developer Tooling):** `wire struct` + MPK sandboxing, predicated `VVec` SIMD, `VirtualArena` + `tiled(N) struct`, `Lens` LSP, `Stylus` (`fmt`), `Lint`, and `Codex` (`doc`).
- [ ] **Milestone M5 (Domain & Kernel Flagships):** Bring up `Vault`, `Beacon`, `Quarry`, `Chorus`, `Silicon`, `eBPF`, `Tensor`, `Prism`, `Aura`, `Ignis`, `Warp`, `Aether`, and `Grammar`.

---

Strict host-data tools are available as `vesper json check FILE --json` and
`vesper traceparent HEADER --json`. The recorded [design snapshot](docs/specification.txt)
has [revision metadata](docs/specification-source.json) and a
[review of the expanded sections](docs/rfcs/expansion-37-review.md). The independent
foundation libraries have self-checking examples under `scripts/test-expansion-foundations.sh`.

## Contributing

Contributions across compiler passes, Cranelift/WasmFX backends, Lean 4 tactics, and domain substrates are welcome! Every pull request is gated by `vesper fmt --check`, `vesper lint`, `vesper semver`, `vesper test --sim --doc`, and the zero-`sorryAx` Lean 4 certificate check.

## License

Dual-licensed under either of:

* **Apache License, Version 2.0** (`LICENSE-APACHE` or <http://www.apache.org/licenses/LICENSE-2.0>)
* **MIT License** (`LICENSE-MIT` or <http://opensource.org/licenses/MIT>)

at your option.
