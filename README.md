# Vesper

**Zero-GC Systems Programming with Modal True Linearity, One-Shot Lexical Effects, Disjoint Place Projections, and Prophecy-Free Lean 4 Verification.**

[![License: Apache-2.0 OR MIT](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](#license)
[![Compiler Backend: Cranelift SSA](https://img.shields.io/badge/codegen-Cranelift%20SSA%20%2B%20egglog-orange.svg)](#5-stage-compiler-architecture)
[![Metatheory: Lean 4 Verified (0 sorryAx)](https://img.shields.io/badge/metatheory-Lean%204%20(0%20sorryAx)-brightgreen.svg)](#formal-verification--mechanized-metatheory)
[![Supply Chain: Vesper Loom WASI-p2](https://img.shields.io/badge/supply%20chain-Loom%20WASI--p2%20Hermetic-purple.svg)](#vesper-loom-hermetic-package-management--confinement)

---

**Vesper** is a post-Rust, zero-garbage-collected systems programming language implemented in Rust, targeting **Cranelift SSA IR** for fast native/JIT code generation and **Lean 4** for compositional formal verification.

Vesper preserves Rust's zero-cost memory safety and data-race freedom while eliminating lifetime annotation noise (`'a`), `async` function coloring, `Pin` self-referential boilerplate, `tokio::select!` cancel-safety bugs, unsandboxed `build.rs` scripts, and backward-prophecy verification overhead.

---

## Table of Contents

1. [Three Core Pillars](#three-core-pillars)
2. [How Vesper Improves on Rust](#how-vesper-improves-on-rust)
3. [Vesper at a Glance](#vesper-at-a-glance)
4. [5-Stage Compiler Architecture](#5-stage-compiler-architecture)
5. [Unified Batteries-Included CLI (`vesper`)](#unified-batteries-included-cli-vesper)
6. [Vesper Loom: Hermetic Package Management & Confinement](#vesper-loom-hermetic-package-management--confinement)
7. [Formal Verification & Mechanized Metatheory](#formal-verification--mechanized-metatheory)
8. [Repository Layout (46-Crate Workspace)](#repository-layout-46-crate-workspace)
9. [Flagship Ecosystem Substrates](#flagship-ecosystem-substrates)
10. [Getting Started & Bootstrap Roadmap](#getting-started--bootstrap-roadmap)
11. [Research Foundations](#research-foundations)
12. [License](#license)

---

## Three Core Pillars

1. **Modal True Linearity (`copy`, `affine`, `linear`) with Mode Crossing:**
   Values default to Rust-like `affine` semantics (at-most-once use with automatic synchronous cleanup), while obligations declared `linear` enforce **exact-once consumption** ($\text{copy} \sqsubseteq \text{affine} \sqsubseteq \text{linear}$). A `linear` value cannot go out of scope implicitly or be forgotten via `mem::forget`; every control-flow and error branch must discharge it via explicit destructuring or a `sink` transition (`tx.commit()` or `tx.abort()`). Cleanup becomes an ordinary effectful function—solving Rust's `!Destruct` and "Async Drop" problems directly. Using OxCaml-style **mode crossing**, generic containers (`Option[T]`, `RegionBox[r, T]`) automatically strengthen or inherit `linear` obligations based on `T`.
2. **One-Shot Lexical Effect Capabilities (`eff [...]`):**
   Asynchronous completion I/O (`io_uring`, `kqueue`, `IOCP`), structured concurrency (`Scope`), checked fallibility (`Fail[E]`), lexical arenas (`r: Region`), and SIMD target profiles (`Isa[P]`) are unified under **one-shot lexical algebraic effect handlers**. Because continuations crossing `linear` or `inout` frames are restricted to single resumption, suspended fiber stacks never duplicate unique pointers and hold zero self-referential borrows—requiring **zero `Pin`**, **zero `Future` polling state machines**, and **zero colored higher-order traits**.
3. **Second-Class Disjoint Place Projections (`in`, `inout`, `sink`):**
   Synthesizing Mutable Value Semantics (Hylo) with Place Capability Graphs (PCGs), Vesper replaces lifetime-annotated first-class references (`&'a T`, `&'a mut T`) with parameter passing modes and trie-structured field-tree selectors (`inout broker.{index, metrics}`). Callers retain shared `in` access to `broker.config` while mutating `broker.{index, metrics}` across helper calls and scoped fibers with **zero `'a` lifetime parameters**.

---

## How Vesper Improves on Rust

| Systems Dimension | Rust (2024–2026 Ecosystem) | Vesper |
|---|---|---|
| **Resource Finalization** | `affine` only: `mem::forget` is safe, `Drop::drop(&mut self)` is synchronous and infallible; no `AsyncDrop`. | **True `linear` mode**: Theorem 2 guarantees exact-once consumption across all normal, `Fail`, and `Cancel` paths. |
| **Borrowing & Aliasing** | First-class `&'a T` / `&'a mut T` references infect structs, traits, and closures with `'a` lifetime bounds. | **Second-class `in` / `inout` / `sink` projections** over disjoint field trees (`inout x.{a, b.c}`) with zero lifetimes. |
| **Async & Cancel Safety** | Colored `async fn`, `Pin<&mut Self>` state machines, `'static + Send` bounds, and silent `tokio::select!` state drops. | **Colorless `eff [...]` rows** on pooled 64 KiB fibers; `[T-Let]` statically rejects `Cancel` / `?` while any `linear` value is live. |
| **Generics & Syntax** | `<T>` angle brackets require `::<T>` turbofish disambiguation and cascade IDE parse errors on unclosed `<`. | **Deterministic LL(2) `[T]` square-bracket generics** with `UpperIdent` / `LowerIdent` lexical separation; zero turbofish. |
| **Macros & Build Scripts** | `build.rs` and `proc_macro` dylibs execute arbitrary unsandboxed host code with full network and OS access. | **Fuel-metered `wasm32-wasip2` Wasmtime sandboxes** (`0` sockets, epoch-zero clocks) + `[T-MacroConfinement]` anti-smuggling. |
| **Supply-Chain Authority** | Ambient `std::fs` / `std::net` / `GlobalAlloc` in any transitive dependency; human-curated SemVer. | **Compile-time `capability_ceiling` in `Loom.toml`** (`Alloc` is explicit!) + `mif:blake3` mathematical SemVer variance. |
| **Formal Verification** | Requires *Charon + Aeneas* LLBC region reconstruction and backward prophecy functions for `&'a mut T`. | **Direct 1-to-1 prophecy-free Lean 4 extraction** (`vesper-verify-lean`) + `.olean` proof certificates (`0 sorryAx`). |

---

## Vesper at a Glance

### 1. Linear Transactions, Disjoint Field Projections & Compile-Time Cancel Safety

```rust
// A `linear` struct cannot be implicitly dropped or forgotten on any branch.
linear struct WriteTxn {
    wal_slot: u64,
    dirty_pages: BufferPoolSlice,
}

impl WriteTxn {
    // Solves "Async Drop": cleanup is an explicit `sink self` consumer performing `Storage` + `Fail`!
    fn commit(sink self) -> CommitReceipt eff [Storage, Fail[IoError]] {
        let WriteTxn { wal_slot, dirty_pages } = self; // Linear destructuring discharges obligation
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

// Mutates ONLY `broker.index` and `broker.metrics`; caller keeps shared `in broker.config` with 0 lifetimes!
pub fn append_batch(
    inout broker.{index, metrics}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Storage, Fail[IoError]] {
    let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);

    // Note: Writing `validate_crc(in batch)?;` here is a COMPILE-TIME ERROR (E0301):
    // `Fail` has resumption guarantee `affine`, and `[T-Let]` forbids `linear <= affine`!
    match validate_crc(in batch) {
        Ok(()) => {
            broker.index.insert_in_place(sink batch);
            broker.metrics.batches_written += 1;
            tx.commit() // Consumed on happy path
        }
        Err(err) => {
            Storage::recycle_batch(sink batch);
            tx.abort(); // Consumed on error path
            Fail::raise(err)
        }
    }
}
```

### 2. Two-Line Prophecy-Free Lean 4 Crash-Consistency Proof (`vesper verify`)

Because `vesper-mir-pcg` proves disjointness ($p_i \perp p_j$) and linear exhaustion prior to backend lowering, `vesper-verify-lean` extracts `append_batch` into a pure functional Lean 4 `EffM` state-tuple transition with lawful `@[simp]` field-tree lenses and **zero separation-logic heap framing**:

```lean
-- proofs/WalSafety.lean
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

---

## 5-Stage Compiler Architecture

```text
  .vsp Source + /// Doctests
         │
         ▼  [Stage 1: vesper-syntax]
  Logos SIMD Lexer + Resilient LL(2) Parser ──► Lossless Rowan Red-Green CST (Salsa <2ms)
         │                                           ├─► vesper-fmt  (Wadler-Lindig Stylus)
         ▼  [Stage 2: vesper-hir + vesper-pact]      └─► vesper-forge (WASI-p2 Hygienic Macros)
  Desugared lambda_Vesper HIR + OxCaml Mode Crossing + SLG Tabled Canonical Trait/Model Solver
         │
         ▼  [Stage 3: vesper-mir-pcg]
  Basic-Block Place IR + Place Capability Graph (Alias-XOR-Mutability + Resumption Frame + Linearity)
         │                                           ├─► vesper-lint   (Substructural & Arena Lints)
         │                                           └─► vesper-semver (mif:blake3 Variance Solver)
         ▼  [Stage 4: vesper-opt-egglog]
  Effectful E-Graph Equality Saturation (0ns Handler Fusion + @fip Constructor Reuse + SIMD/SQL/DBSP)
         │
         ├───────────────────────────────────────────┐
         ▼  [Stage 5A: vesper-codegen-clif]          ▼  [Stage 5B: vesper-verify-lean]
  Cranelift SSA IR (x86_64 / AArch64 / RISC-V /   Prophecy-Free Lean 4 Functional Extractor
  eBPF / PVH Unikernel / JIT Doctests & Benches)  + Zero-sorryAx .olean Certificate Auditor
```

---

## Unified Batteries-Included CLI (`vesper`)

Every developer workflow ships inside a **single static `vesper` binary** (`crates/vesper-cli`)—eliminating third-party Cargo subcommand sprawl:

| Command | Built-In Capability | Replaces in Rust |
|---|---|---|
| `vesper new` / `init` | Scaffolds package or workspace with secure-by-default `capability_ceiling = []` in `Loom.toml`. | `cargo new` |
| `vesper add @org/pkg` | Enforces Sigstore OIDC lineage, agent-aware anti-slopsquatting gates, and capability prompts. | `cargo add` + `cargo vet` |
| `vesper build` / `check` | Runs WASI `build.vsp` + macros, `mif:blake3` early-cutoff CAS builds, and `--unikernel` / `--tee` targets. | `cargo build` / `check` |
| `vesper fmt` | **Vesper Stylus**: Zero-config, idempotent, comment-complete Wadler-Lindig formatter over Rowan CSTs. | `cargo fmt` (`rustfmt`) |
| `vesper lint [--fix]` | **Vesper Lint**: FieldTree narrowing, unused-effect removal, and SemVer-safe CST fixes over a whole package (or one file). `Alloc`-to-`Region` demotion and `egglog` rules are not implemented yet. | `cargo clippy` + `dylint` |
| `vesper doc` | **Vesper Codex**: Checked API HTML/JSON with `FieldTree` borrow heatmaps, linear typestate graphs, 64B wire rulers, an offline signature-search page (`--bundle`), and proof badges linking audited `.olean` certificates from the Loom store. | `cargo doc` (`rustdoc`) |
| `vesper test` | `--doc` single-JIT-module doctests over a whole package, with exact `compile_fail` codes and `sim(...)` doctests on deterministic *SimPulse* clock, network & disk. Parallel unit tests, `--sim`, `--sim-ui` (*SimAura*) & `--mutate` are not implemented yet. | `cargo test` + `nextest` + `turmoil` + `cargo-mutants` |
| `vesper bench` | **Vesper Metronome**: 12-cycle user-space `RDPMC`/`kpc` PMU counters, CAS paired A/B *Stabilizer* runs, & *Coz* causal profiling. | `criterion` + `divan` + `iai-callgrind` + `cargo-show-asm` |
| `vesper fuzz` | **Vesper Crucible**: Cranelift SSA edge-coverage + Bitwuzla/Z3 concolic fuzzing, `ChoiceTape` shrinking, & MPK `@differential_shadow`. | `cargo-fuzz` + `proptest` + `kani` |
| `vesper verify` | Extracts prophecy-free Lean 4 specs, runs `--refute` concolic pre-checks, mounts CAS `.olean` certs, & audits `0 sorryAx`. | `charon` + `aeneas` + `hax` |
| `vesper semver` | Mathematically proves modal, `FieldTree`, and `eff [...]` contextual substitutability ($I_{\text{new}} \preceq_{\text{pub}} I_{\text{old}}$) with witness synthesis. | `cargo-semver-checks` |
| `vesper resolve --optimize` | Dual-tier solver: `<50ms` `resolvo` CDCL everyday path + Max-SMT (`bitwuzla`/`z3`) CVE/capability/trust-cost minimizer. | Cargo resolver |
| `vesper migrate` | Lifts C headers and Rust crates into `.vsys` manifests, `wire struct` views, `linear` handles, and lifetime-free `inout` projections. | `bindgen` + `c2rust` |
| `vesper replay` | **Vesper Chrono**: `<2%` overhead multi-core effect flight recorder, Intel PT / Arm CoreSight reverse step, & linear lineage queries. | `rr` + `gdb` / `lldb` |
| `vesper top` | Zero-instrumentation live TUI inspecting `io_uring` fibers, GPU pass timestamps, and parked `Mode::Linear` obligations. | `tokio-console` |
| `vesper lsp` | **Vesper Lens**: `<2ms` incremental `salsa` LSP daemon with `PoisonedRecovery` obligation-ranked autocomplete. | `rust-analyzer` |
| `vesper audit` / `publish` | Reachable OSV CVE scan, SLSA v1.1 SBOM, Lila 2-of-N reproducibility check, SemVer + `.olean` gate, & keyless Sigstore publish. | `cargo-audit` + `cargo-deny` |

---

## Vesper Loom: Hermetic Package Management & Confinement

`Loom.toml` is strictly declarative (parsed in $<1\text{ ms}$) and makes transitive authority budgets, WASI build sandboxes, lint policies, and Lean 4 verification targets first-class:

```toml
[package]
name = "@robot-head/krabka-wal"
version = "0.4.0"
edition = "2026"
license = "Apache-2.0"
capability_ceiling = ["Storage", "Net", "Clock", "Fail"]
forbid_unsafe_ffi = true

[dependencies]
# Pure library: compiler statically guarantees ZERO filesystem, network, clock, heap, or FFI access!
"@vesper-core/bytes" = { version = "^1.3.0", effects = [] }
# Scoped I/O dependency: granted Storage and Fail, statically barred from Net or Ffi:
"@vesper-io/uring-reactor" = { version = "^0.9.2", effects = ["Storage", "Fail"] }

[build-sandbox]
script = "build.vsp"        # Compiled to wasm32-wasip2; executed in Wasmtime
wasi_read_paths = ["proto/"]
fuel_limit = 50_000_000
allow_network = false

[lints.vesper]
overbroad_inout_projection = "warn"
unused_effect_capability   = "deny"
demote_alloc_to_region     = "warn"

[verification]
export_lean4_specs = ["src/wal_txn.vsp"]
```

---

## Formal Verification & Mechanized Metatheory

Vesper's core calculus ($\lambda_{\text{Vesper}}$) and Stage 3 **Place Capability Graph (`PcgState`)** checker are mechanized in pure, zero-dependency **Lean 4** (`proofs/VesperMetatheory.lean`) with **zero `sorryAx` axioms**:

* **Substructural Lattice & `[T-SubMode]` (`mode_submode_allows_count`):** Mechanizes $\text{copy} \sqsubseteq \text{affine} \sqsubseteq \text{linear}$ over dynamic use sets $\mathbb{N} \supset \{0, 1\} \supset \{1\}$ and proves `linear_not_discardable`.
* **OxCaml Field-Modality Well-Formedness (`adt_kind_never_relaxes_linear_component`):** Proves that `[K-Adt]`'s premise $k_i \sqsubseteq m_i$ prevents `@@ copy` overrides from hiding or discarding nested `Mode::Linear` obligations.
* **`[T-Let]` Resumption Frame Soundness (`abort_soundness_preserves_linear` & `uncancellable_io_allows_live_linear`):** Proves that abortable effects (`Fail`, `Cancel`, where $q = \text{affine}$) force all live residual bindings to satisfy $m_z \sqsubseteq \text{affine}$, while `linear effect` I/O (`Storage`, `Net`, `Async`) preserves live `linear` handles across yield points.
* **`[T-CallProj]` Exclusivity & SemVer Narrowing (`call_proj_axm_exclusivity` & `semver_narrowing_preserves_disjoint`):** Proves Alias-XOR-Mutability across field-tree selectors and proves that narrowing `inout` projections in minor releases preserves all caller disjointness proofs.
* **Theorem 2 — Total Linear Exhaustion (`vesper_total_linear_exhaustion`):** Proves that every well-typed closed program consumes $100\%$ of dynamically minted `Mode::Linear` resources (`minted.length = consumed.length`) across both normal value returns and early `Fail` unwinds.

---

## Repository Layout (46-Crate Workspace)

```text
vesper/
├── Cargo.toml                     # Unified 46-crate Rust workspace manifest
├── proofs/
│   └── VesperMetatheory.lean      # Standalone zero-sorryAx Lean 4 metatheory of lambda_Vesper & PCG
└── crates/
    ├── Tier 1: Stage 1–5 Compiler, Runtime & Verification Core (12 crates)
    │   ├── vesper-span/           # Interned symbols, FileId, TextSpan & multi-span Ariadne diagnostics
    │   ├── vesper-syntax/         # Logos SIMD lexer, lossless Rowan CST & resilient LL(2) parser
    │   ├── vesper-hir/            # Desugared lambda_Vesper HIR, Mode lattice, FieldTree & EffectRow
    │   ├── vesper-mir-pcg/        # Basic-block Place IR & Place Capability Graph linearity/borrow solver
    │   ├── vesper-opt-egglog/     # Effectful e-graph equality saturation, @fip reuse & operator fusion
    │   ├── vesper-codegen-clif/   # Cranelift SSA native/JIT backend, 4-6ns fiber ABI & fuzz counters
    │   ├── vesper-pulse/          # Native io_uring/kqueue/IOCP fiber runtime, SimPulse & vesper top
    │   ├── vesper-ffi/            # Padding-free wire struct layouts, .vsys ABI & 1.5ns MPK/POE sandboxing
    │   ├── vesper-simd/           # 100% safe predicated SIMD (VVec) across AVX-512, SVE2, RVV 1.0 & NEON
    │   ├── vesper-alloc/          # Zero-atomic ShardSlab heap, VirtualArena regions & tiled(N) AoSoA
    │   ├── vesper-verify-lean/    # Prophecy-free Lean 4 functional extractor & .olean hygiene auditor
    │   └── vesper-semver/         # Merkle Interface Fingerprints (mif:blake3) & SemVer variance solver
    │
    ├── Tier 2: Vesper Loom Package Manager & Supply Chain (5 crates)
    │   ├── vesper-loom-calculus/  # ICFP 2026 Package Calculus IR, monotonic features & capability ceilings
    │   ├── vesper-loom-solver/    # Dual-engine Resolvo CDCL (<50ms) + Bitwuzla/Z3 Max-SMT optimizer
    │   ├── vesper-loom-sandbox/   # Wasmtime (wasm32-wasip2) fuel-metered build.vsp, macro & lint sandbox
    │   ├── vesper-loom-cas/       # BLAKE3 + redb content-addressed store & CoW reflink materializer
    │   └── vesper-loom-trust/     # Keyless Sigstore OIDC, TUF, Lila quorum & anti-slopsquatting gate
    │
    ├── Tier 3: Language Abstraction & Unified Developer Tooling (10 crates)
    │   ├── vesper-pact/           # SLG tabled canonical trait solver & zero-newtype named models
    │   ├── vesper-forge/          # Scope-set + PreResolvedDefId hygienic macros & [T-MacroConfinement]
    │   ├── vesper-lsp/            # Vesper Lens <2ms Salsa LSP daemon & obligation-guided autocomplete
    │   ├── vesper-fmt/            # Vesper Stylus comment-complete Wadler-Lindig CST formatter
    │   ├── vesper-lint/           # Vesper Lint substructural/FieldTree/egglog analyzer & MIF autofixer
    │   ├── vesper-doc/            # Vesper Codex typestate/wire diagrams, WASM Hoogle & JIT doctests
    │   ├── vesper-bench/          # Vesper Metronome RDPMC/kpc PMU runner, Stabilizer A/B & Coz profiler
    │   ├── vesper-crucible/       # Concolic SMT fuzzer, ChoiceTape shrinker & Rust/C migration lifter
    │   ├── vesper-chrono/         # Effect-boundary time-travel debugger, IPT/ETM & linear provenance
    │   └── vesper-cli/            # Single static `vesper` toolchain binary
    │
    └── Tier 4: Flagship Domain, Kernel, Cloud, ML & Protocol Substrates (19 crates)
        ├── vesper-prism/          # @vesper-web/prism     — Zero-alloc HTTP/1.1/2/3 & typestate WebSockets
        ├── vesper-aura/           # @vesper-ui/aura       — Native WinUI 3, GTK 4 & SwiftUI desktop UI
        ├── vesper-ignis/          # @vesper-game/ignis    — DirectX 12.620, Vulkan 1.4 & Metal 4 3D engine
        ├── vesper-serde/          # @vesper-wire/*        — Arena Serde, 2-stage SIMD JSON & RelPtr32
        ├── vesper-beacon/         # @vesper-obs/beacon    — Linear ActiveSpan, SIMD W3C & OTLP-Arrow
        ├── vesper-vault/          # @vesper-crypto/vault  — SSA-verified ConstTime, NonceTicket & PQ KEM
        ├── vesper-quarry/         # @vesper-db/quarry     — Hermetic WASI @sql, PagePin & AoSoA morsels
        ├── vesper-chorus/         # @vesper-dist/chorus   — Choreographic EPP consensus & zero-copy RDMA
        ├── vesper-silicon/        # @vesper-hw/silicon    — Bare-metal kernels, IrqGuard, MMIO & WCSD
        ├── vesper-ebpf/           # @vesper-kernel/ebpf   — Pre-flight eBPF verifier, XDP & BpfRingSlot
        ├── vesper-tensor/         # @vesper-ml/tensor     — Symbolic tensor shapes, fusion & Paged-KV LLM
        ├── vesper-nexus/          # @vesper-wasm/nexus    — WasmFX sandboxes & linear WIT host/guest plugins
        ├── vesper-resonance/      # @vesper-rt/resonance  — Hard-realtime @realtime DSP, PDC & 1-10kHz control
        ├── vesper-aegis/          # @vesper-tee/aegis     — TDX / SEV-SNP / Arm CCA cVMs & RTMR attestation
        ├── vesper-flux/           # @vesper-stream/flux   — Zero-GC DBSP Z-Set incremental stream views
        ├── vesper-warp/           # @vesper-net/warp      — AF_XDP / DPDK user-space QUIC/TCP & BBRv3
        ├── vesper-kinetic/        # @vesper-sim/kinetic   — Cross-CPU DetFloat XPBD physics & GGPO rollback
        ├── vesper-aether/         # @vesper-cloud/aether  — <1.2ms PVH unikernels & clone-safe UFFD forks
        └── vesper-grammar/        # @vesper-parse/grammar — Non-malleable binary/PEG parsers & Arrow IPC
```

---

## Flagship Ecosystem Substrates

Every domain framework in Tier 4 is co-designed around Vesper's substructural modes (`Mode::Linear`), `wire struct` zero-copy views, `tiled(16) struct` (`AoSoA`) L1 cache layouts, and `vesper-verify-lean` proofs:

* **Networking, Web & Distributed Systems:** **Warp** (`UmemFrame` 4-ring AF_XDP conservation + hardware EDT BBRv3 QUIC), **Prism** (`Responder[HeadersPending]` typestate HTTP/3 + 64 KiB per-request `VirtualArena`), **Chorus** (`@choreography` deadlock-free Endpoint Projection + `InFlightRdmaWrite`), and **Flux** (DBSP Z-Set incremental materialized views + `FrontierTicket`).
* **Storage, Serialization & Verified Parsers:** **Quarry** (zero-socket WASI `@sql`, `PagePin` `io_uring` `O_DIRECT` buffer pool, and `egglog` predicate pushdown), **Wire/Json** (`JsonStr[r]` lifetime-free strings, carryless-multiply SIMD JSON, and `RelPtr32` archives), and **Grammar** (Vest/EverParse strong non-malleable codecs, `UnconsumedTail` anti-smuggling leases, and zero-copy 64B Arrow IPC).
* **Security, Confidential Computing & Cloud Unikernels:** **Vault** (Stage 5 Cranelift constant-time SSA taint verifier, hardware DIT/DOITM, `NonceTicket`, and `X25519MLKEM768`), **Aegis** (Intel TDX / AMD SEV-SNP / Arm CCA single-fetch `HostBounceGrant` + `Loom.lock`/`.olean` RTMR attestation), and **Aether** ($<1.2\text{ ms}$ ring-0 PVH unikernels + $400\ \mu\text{s}$ `VMGENID` clone-safe snapshot forks).
* **Kernel, Bare-Metal & Hard-Realtime Control:** **Silicon** (`@isr` sleep-in-atomic prevention, `IrqGuard`, `@svd_pac` W1C-safe MMIO, `PendingTlbShootdown`, and static Worst-Case Stack Depth), **eBPF** (512B stack bound, canonical XDP packet guards, `BpfRingSlot`, and pre-flight abstract verifier), and **Resonance** (`@realtime` zero-alloc confinement, hardware `FTZ`/`DAZ`, wait-free `RtRetirementSink`, and automatic Plugin Delay Compensation).
* **Native UI, Tri-Backend 3D Graphics, Physics & Local AI:** **Aura** ($O(\Delta)$ views compiled natively to WinUI 3, GTK 4/libadwaita, and SwiftUI `ObservationRegistrar`), **Ignis** (single-source WASI Slang shaders + static Enhanced/Fence/Sync2/Stage barrier compiler across DirectX 12, Vulkan 1.4, and Metal 4), **Kinetic** (`eff [DetFloat]` cross-ISA bit-deterministic XPBD physics), and **Tensor** (symbolic axis checking + `KvPageLease` PagedAttention LLM inference).

---

## Getting Started & Bootstrap Roadmap

### Prerequisites

* **Rust Toolchain:** Rust `1.85+` (`cargo`, `rustup`) for building the 46-crate host workspace.
* **Lean 4 Toolchain (Optional, for `vesper verify` & Metatheory):** [`elan`](https://github.com/leanprover/elan) (`leanprover/lean4:v4.16.0+`).

### Verifying the Core Metatheory & Building the Host Workspace

```bash
# 1. Clone the repository
git clone https://github.com/vesper-lang/vesper.git
cd vesper

# 2. Machine-check the standalone zero-sorryAx Lean 4 metatheory (lambda_Vesper + PCG)
lean proofs/VesperMetatheory.lean

# 3. Build the unified `vesper` CLI toolchain in release mode
cargo build --release -p vesper

# 4. Run workspace unit tests, Stylus formatting checks, Substructural lints, & JIT doctests
./target/release/vesper fmt --check
./target/release/vesper lint crates/vesper-doc/examples/package
./target/release/vesper test --doc crates/vesper-doc/examples/package
```

### Compiler Bootstrap Milestones

* **Milestone M1 (`vesper-syntax` + `vesper-hir`):** Resilient LL(2) parsing into lossless `rowan` CSTs, bidirectional OxCaml mode crossing (`[T-Cross]`), and `WriteTxn` / `append_batch` typechecking.
* **Milestone M2 (`vesper-mir-pcg` + `vesper-codegen-clif` + `vesper-pulse`):** `PcgState` disjoint `FieldTree` + `[T-Let]` Resumption Frame verification, Cranelift `vmctx` evidence passing, and pooled fiber execution.
* **Milestone M3 (`vesper-semver` + `vesper-loom-*` + `vesper-verify-lean`):** `mif:blake3` early-cutoff CAS builds, `resolvo` CDCL + Max-SMT solving, Wasmtime `build.vsp` sandboxes, and zero-`sorryAx` `.olean` certificate auditing.
* **Milestone M4 (Developer Experience Suite):** Bring up `vesper-pact`, `vesper-forge`, `vesper-lsp` (*Lens*), `vesper-fmt` (*Stylus*), `vesper-lint` (*Lint*), `vesper-doc` (*Codex*), `vesper-bench` (*Metronome*), `vesper-crucible`, and `vesper-chrono`.
* **Milestone M5 (Flagship Systems & Application Substrates):** Deliver the Tier 4 domain crates (`Vault`, `Beacon`, `Quarry`, `Chorus`, `Warp`, `Silicon`, `eBPF`, `Aegis`, `Aether`, `Flux`, `Grammar`, `Resonance`, `Tensor`, `Nexus`, `Prism`, `Aura`, `Ignis`, and `Kinetic`).

---

## Research Foundations

Vesper synthesizes breakthroughs from 2023–2026 programming languages, systems, and security venues:

* **Modalities, MVS & Aliasing:** *Mode Crossing* (OxCaml, ICFP 2026); *Hylo & Mutable Value Semantics* (IWACO 2026); *Transient Sequences* (ICFP 2026); *Tree Borrows* (PLDI 2025); *Place Capability Graphs* & *Verified Polonius* (2026).
* **Lexical Effects & Continuations:** *Affect: An Affine Type and Effect System* (POPL 2025); *Zero-Overhead Lexical Effect Handlers* (OOPSLA 2025); *Virtualizing Continuations* (PLDI 2026); *Iris-WasmFX* (2026).
* **Equality Saturation & Allocation:** *Effectful E-Graphs / eggcc* (OOPSLA 2026); *Fully In-Place (FIP) Calculus* (JFP 2025); *Region Locality Under Fragmentation* (ISMM 2026); *Arena Reachability Types* (2026).
* **Verification, Security & Package Calculus:** *Lean4Lean* (TYPES 2025); *Package Managers a la Carte* (ICFP 2026); *CargoScan* (ESOP 2026); *Omniglot* (OSDI 2025 Best Paper); *SafeFFI* & *Vest* (USENIX Security 2025–2026); *DBSP* (VLDB 2023); *Choreographic Programming* (PLDI 2026).

---

## License

Licensed under either of:

* **Apache License, Version 2.0** (`LICENSE-APACHE` or <http://www.apache.org/licenses/LICENSE-2.0>)
* **MIT License** (`LICENSE-MIT` or <http://opensource.org/licenses/MIT>)

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Vesper by you shall be dual-licensed as above, without any additional terms or conditions.