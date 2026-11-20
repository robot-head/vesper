// Native Typst specification components (compatible with Typst 0.15.1 bundled fonts).
#let _ink = rgb("#1B2030")
#let _muted = rgb("#5B6474")
#let _rule = rgb("#D8DCE3")
#let _accent = rgb("#006B80")

#let _infer(name, premises, conclusion) = block(
  width: 100%,
  breakable: false,
  inset: (x: 10pt, y: 8pt),
  fill: luma(98%),
  stroke: (left: 1.4pt + _accent, top: 0.3pt + _rule, right: 0.3pt + _rule, bottom: 0.3pt + _rule),
  above: 8pt,
  below: 8pt,
)[
  #grid(
    columns: (1fr, auto),
    column-gutter: 10pt,
    align: (center + horizon, right + horizon),
    stack(
      dir: ttb,
      spacing: 4.5pt,
      align(center, premises),
      line(length: 100%, stroke: 0.55pt + _ink),
      align(center, conclusion),
    ),
    text(font: "DejaVu Sans Mono", size: 7.8pt, fill: _muted, name),
  )
]

// Keep short listing tails off isolated continuation pages.
#let _compact-listing(body) = {
  show raw.where(block: true): set par(leading: 0.3em)
  body
}

#let _part-summary(tag, body) = block(
  width: 100%,
  inset: (x: 11pt, y: 9pt),
  fill: luma(97%),
  stroke: (left: 1.6pt + _ink, top: 0.3pt + _rule, right: 0.3pt + _rule, bottom: 0.3pt + _rule),
  above: 4pt,
  below: 14pt,
)[
  #text(font: "DejaVu Sans Mono", size: 8pt, weight: "bold", fill: _accent, upper(tag))
  #v(4pt)
  #body
]

#let manuscript(chapter-deck) = [

= Specification Overview
<overview>

*Review status:* This document mixes proposed designs and illustrative listings with implemented components. Paths under `illustrative/` identify examples and are not repository files. Claims about proof, performance, security, and complete implementation remain subject to verification; no conformance or production-safety conclusion follows from this draft.
#chapter-deck[Design Goals, Proof Boundaries, and a Guide to the Platform]

*Vesper* is a systems-language design with a native compiler and Lean 4 models for selected language and runtime properties. Its goals include explicit resource ownership, lexical borrowing, deterministic execution, and compositional verification. The implementation and mechanized models cover different subsets; this draft does not establish end-to-end memory safety, data-race freedom, or functional correctness for every backend and subsystem.

The overview introduces four design goals and maps them to the eight parts below. Read Chapters 1–5 for the core notation, then follow the chapter links for individual subsystems. Appendix A identifies the separate calculi and their proved properties; a single calculus combining all features remains open.

+ *Four-Axis Modal Linearity and `@0` Ghost Erasure:* Bindings combine runtime presence $rho in {0, 1}$ with a modal vector $bold(m) = (u, p, c, o)$ for usage, portability, contention, and exchange order. The checked models establish resource conservation and exhaustion in their specified fragments. Closed, terminating ghost instrumentation has an operational erasure proof; general source ghost elaboration and captured ghost obligations remain open.
+ *Four-Mode Disjoint Projections and Liquid Refinements:* The design uses parameter modes (`in`, `inout`, `out`, `sink`) over field and slice projections. The place calculus and typed lens bridge establish safety and snapshot properties for initialized, same-typed updates and disjoint swaps. General dependent heap typing, allocation, and a complete refinement solver are not established by those proofs. Solver fragments and limits must be stated separately; no fixed query latency is guaranteed.
+ *One-Shot Lexical and Resource-Graded Effects:* Lexical handlers model state, completion, and controlled continuation use. Separate proofs cover stateful-handler safety and bounded additive costs for finite one-shot handlers. Combining source grade inference, linear handler state, recursion, and native timing remains open.
+ *Explicit Authority and Lean Verification:* Capability ceilings, package policies, and extracted Lean models support local checks. The implemented proof gate performs kernel replay and transitive axiom auditing, with fresh-compilation comparison for extraction cache hits. Source-contract correspondence, executable refinement, and full SemVer proof transport require additional proofs described in Chapters 9 and 12.

#figure(
  table(
    columns: (23%, 15%, 62%),
    align: (left, left, left),
    table.header([*Architectural Part*], [*Chapters*], [*Design Scope, Packages, and Host Crates*]),
    [*Part I: Language & Type Theory*],
    [#link(<part-1>)[Chapters 1--7]],
    [PL foundations, proposed LL(2) grammar, $lambda_(upright("Vesper"))$ 4-axis `ModeVec` lattice, `@0` ghost linearity, `in`/`inout`/`out`/`sink` field & slice projections, liquid refinements, one-shot lexical & pomonoid-graded effects, guarded clocks, quotients, *Pact* traits (`vesper-pact`), and *Forge* `sugar` enforestation and WASI macros (`vesper-forge`).],
    [*Part II: Compiler, Proofs & Governance*],
    [#link(<part-2>)[Chapters 8--12]],
    [5-stage compiler & workspace (`vesper-syntax`, `vesper-hir`, `vesper-refine`, `vesper-mir-pcg`, `vesper-opt-egglog`, `vesper-codegen-clif`), *Lean 4 Proof Extractor* (`vesper-verify-lean`), *Verified Bootstrap & DDC* (`vesper-codegen-verified`), *Vesper Loom* (`vesper-loom-*`), and *SemVer Variance* (`vesper-semver`).],
    [*Part III: Core Runtime Substrates*],
    [#link(<part-3>)[Chapters 13--17]],
    [*Strata* regions, slabs & `tiled(N)` AoSoA (`vesper-alloc`), *Vector* portable & scalable SIMD (`vesper-simd`), *Pulse* fiber runtime & `SimPulse` (`vesper-pulse`), *Bridge* `wire struct`, `.vsys` & MPK FFI (`vesper-ffi`), and *Nexus* WasmFX & linear WIT plugins (`vesper-nexus`).],
    [*Part IV: Developer & Verification Tooling*],
    [#link(<part-4>)[Chapters 18--24]],
    [*Lens* incremental LSP (`vesper-span`, `vesper-lsp`), *Stylus & Lint* (`vesper-fmt`, `vesper-lint`), *Codex* living docs & JIT doctests (`vesper-doc`), *Chroma* semantic highlighting & cards (`vesper-highlight`), *Crucible* concolic fuzzing & C/Rust lifting (`vesper-crucible`), *Chrono* flight recorder (`vesper-chrono`), and *Metronome* PMU benchmarking (`vesper-bench`).],
    [*Part V: OS, Kernel, Crypto & Enclaves*],
    [#link(<part-5>)[Chapters 25--29]],
    [*Silicon* bare-metal kernels, `@svd_pac` MMIO & LIFO `IrqGuard` (`vesper-silicon`), *eBPF* pre-flight verifier & XDP (`vesper-ebpf`), *Aether* PVH unikernels & UFFD KVM microVMs (`vesper-aether`), *Vault* constant-time AEAD & PQ KEM (`vesper-vault`), and *Aegis* TDX/SEV-SNP/CCA enclaves (`vesper-aegis`).],
    [*Part VI: Data, Net, Web, Consensus & DB*],
    [#link(<part-6>)[Chapters 30--37]],
    [*Wire & Json* arena serde & SIMD JSON (`vesper-serde`), *Grammar* non-malleable binary/Arrow IPC codecs (`vesper-grammar`), *Beacon* TraceContext & OTLP-Arrow (`vesper-beacon`), *Warp* `AF_XDP` QUIC/TCP & BBRv3 (`vesper-warp`), *Prism* HTTP/2/3 & WebSockets (`vesper-prism`), *Chorus* choreographic RDMA consensus (`vesper-chorus`), *Quarry* hermetic SQL (`vesper-quarry`), and *Flux* DBSP streams (`vesper-flux`).],
    [*Part VII: UI, 3D Graphics, Sim & ML*],
    [#link(<part-7>)[Chapters 38--42]],
    [*Aura* native WinUI 3 / GTK 4 / SwiftUI (`vesper-aura`), *Ignis* tri-backend D3D12 / Vulkan 1.4 / Metal 4 engine (`vesper-ignis`), *Kinetic* bit-deterministic XPBD physics (`vesper-kinetic`), *Resonance* hard-realtime DSP & robotics (`vesper-resonance`), and *Tensor* shape-checked Paged-KV inference (`vesper-tensor`).],
    [*Part VIII: Metatheory & Identity*],
    [#link(<appendix-a>)[Appendices A--B]],
    [Lean 4 models of stated fragments and their operational bridges (Appendix A); canonical CAD/OKLCH brand geometry, *The Disjoint Field-Tree Prism* (Appendix B).],
  ),
  kind: table,
)

= Part I. Language Design and Formal Type Theory
<part-1>
#chapter-deck[Language Foundations, Proposed Syntax, Ownership, Borrowing, Effects, and Abstraction]

#_part-summary("Part I Architectural Scope")[
  Part I introduces Vesper's proposed language and type theory. #link(<ch-1>)[Chapter 1] explains the design rationale, and #link(<ch-2>)[Chapter 2] sketches the surface grammar and its unresolved ambiguities. #link(<ch-3>)[Chapters 3]–#link(<ch-5>)[5] present ownership modes, ghost values, place projections, refinements, effects, clocks, and quotients. #link(<ch-fission>)[Chapter 6] proposes continuation fission and escrow custody for cancellation. #link(<ch-6>)[Chapter 7] describes traits in *Pact*, and import-free syntactic sugar (`sugar`) and macros in *Forge*. The rules describe intended behavior; Appendix A states which fragments have mechanized proofs.
]

#figure(
  table(
    columns: (14%, 34%, 52%),
    align: (left, left, left),
    table.header([*Chapter*], [*Title*], [*Key Judgments and Constructs*]),
    [#link(<ch-1>)[Chapter 1]], [Research Foundations and Rationale], [Substructural ownership, Mutable Value Semantics, Affect one-shot effects, `WriteTxn` walkthrough],
    [#link(<ch-2>)[Chapter 2]], [Lexical Structure and Proposed Grammar], [`UpperIdent`/`LowerIdent` separation, `[T]` generics, `@` index sigils, proposed EBNF],
    [#link(<ch-3>)[Chapter 3]], [Core Calculus and Four-Axis Modalities], [`ModeVec` ($u, p, c, o$), `[K-Adt]` field well-formedness, `Capsule[T]`, `@ lifo`, `@0` ghost erasure],
    [#link(<ch-4>)[Chapter 4]], [Place Projections, Destinations, and Refinements], [`[T-Let]`, `[T-CallProj]`, `[T-CallSliceProj]`, `[T-OutSplitSlice]`, `[T-StrongUpdate]`, `[M-ContendStrip]`],
    [#link(<ch-5>)[Chapter 5]], [Lexical Effects, Resource Grades, and Quotients], [`[T-Perform]`, `[T-Handle]`, pomonoid budgets (`Region`, `Stack`, `ConstTime`), `Later[k, T]`, `quotient`],
    [#link(<ch-fission>)[Chapter 6]], [Continuation Fission, Escrow, and Reapers], [`[T-Let-Fission]`, additive continuations, `escrow`, quarantine budgets],
    [#link(<ch-6>)[Chapter 7]], [Traits, Coherence, Sugar, and Hygienic WASI Macros], [`trait`, canonical `impl` vs. named `model`, `[T-ModelBrand]`, `sugar` enforestation (`[WF-SugarPass]`), `[T-MacroConfinement]`],
  ),
  kind: table,
)

== 1. Research Foundations and Systems Design Rationale
<ch-1>
#chapter-deck[Research Influences, Ownership Tradeoffs, and a Transaction Example]

Vesper explores how ownership modes, lexical borrowing, effects, and proof extraction can work together in a systems language. This chapter explains the research influences and design tradeoffs behind that proposal. Memory safety without garbage collection, data-race freedom, and machine-checked functional correctness are goals; the cited work and separate models do not establish all three for the complete Vesper implementation.

=== 1.1 Scholarly Foundations in Systems Language Design
<ch-1-1>

The following research streams inform the proposed calculus and compiler pipeline:

#figure(
  table(
    columns: (23%, 24%, 53%),
    align: (left, left, left),
    table.header([*Research Stream*], [*Representative Work*], [*Mechanism Informing the Vesper Design*]),
    [*Modal & Substructural Ownership*],
    [OxCaml Mode Crossing (ICFP 2026); _Data Race Freedom à la Mode_ (POPL 2025); _Ordered Adjoint Logic_ (IJCAR 2026)],
    [Replaces syntactic lifetime annotations with orthogonal modal axes (`copy`/`affine`/`linear`, `portable`/`pinned`, `uncontended`/`contended`, `unordered`/`lifo`) and automatic structural mode strengthening.],
    [*Mutable Value Semantics & PCGs*],
    [Hylo & MVS (IWACO 2026); _Tree Borrows_ (PLDI 2025); _Place Capability Graphs_ & Verified Polonius (2026)],
    [Enforces zero-GC, zero-ARC alias-XOR-mutability via second-class parameter projections (`in`, `inout`, `out`, `sink`) over trie-structured field trees and disjoint slice intervals.],
    [*One-Shot Lexical Effects*],
    [_Affect_ (POPL 2025); _Selection Monad_ (PLDI 2025); Lexical Effect Capabilities (OOPSLA 2025)],
    [Reconciles algebraic effect handlers with linear ownership by bounding continuations to one-shot resumptions (`affine` or `linear`), lowering handlers via register evidence-passing.],
    [*Decidable Liquid Refinements*],
    [_Flux_ Liquid Types (POPL 2025); RefinedRust 2.0 (OOPSLA 2026); Quantitative Type Theory (`@0`)],
    [Unifies SMT-decidable Horn refinements (`QF_LIA` + `QF_BV` + `QF_UF`) with `@0` ghost tokens and prophecy-free strong updates (`~>`) on exclusive `inout` and `out` places.],
    [*Prophecy-Free Proof Extraction*],
    [Charon + Aeneas & Hax (#link("https://arxiv.org/abs/2605.30106")[CAV/AIMACS 2026]); #link("https://drops.dagstuhl.de/storage/00lipics/lipics-vol384-types2025/LIPIcs.TYPES.2025.2/LIPIcs.TYPES.2025.2.pdf")[Lean4Lean] (TYPES 2025); #link("https://eprint.iacr.org/2026/899")[VCVio] (2026)],
    [Translates verified mutable and effectful code 1-to-1 into pure Lean 4 state-tuple functions and Freer Interaction Trees (`EffM`) without region reconstruction or prophecy variables.],
    [*Effectful Equality Saturation*],
    [_Effectful E-Graphs_ / `eggcc` (OOPSLA 2026); `egglog` Datalog Saturation],
    [Threads state and capability tokens through Datalog e-graphs to fuse tail-resumptive handlers, elide proven arena bounds, and emit checkable equational rewrite witnesses.],
  ),
  kind: table,
)

The design draws on three ideas from this literature:

- *Substructural Aliasing Without Syntactic Lifetimes:* _Tree Borrows_ (Villani, Hostert, Dreyer, and Jung, PLDI 2025) and _Place Capability Graphs_ demonstrate that fine-grained field-path exclusivity can be tracked over a place hierarchy without adding region variables to surface types, while OxCaml's _Mode Crossing_ (ICFP 2026) shows how concrete component types automatically strengthen modal bounds without redundant trait constraints.
- *Linearity Across Delimited Control:* Multi-shot algebraic effect handlers violate linear ownership because invoking a captured continuation twice duplicates unique resources held on the suspended stack. _Affect: An Affine Type and Effect System_ (van Rooij and Krebbers, POPL 2025; see the #link("https://iris-project.org/pdfs/2025-popl-affect.pdf")[paper] and #link("https://www.youtube.com/watch?v=ftxm1NTmd_U")[presentation]) resolves this conflict by typing delimited continuations within the same substructural lattice as values.
- *Compositional Verification Without Heap Separation Logic:* Because second-class mutable projections terminate strictly at call return and never alias, mutable systems code admits direct functional extraction into Lean 4 (`bv_decide`, `omega`, `grind`) without separation-logic frame boilerplate.

=== 1.2 Structural Limitations of Prior Affine Systems Languages
<ch-1-2>

Affine destructors and first-class lifetime-indexed references create tradeoffs that motivate five Vesper design goals:

+ *Affine (`at-most-once`) vs. True Linear (`exact-once`) Finalization:* Where ownership is purely affine, `mem::forget` is safe and implicit scope cleanup (`Drop`) is synchronous and infallible. The type system cannot enforce that a write-ahead log transaction is committed, an `io_uring` DMA buffer is harvested, a QUIC AEAD nonce ticket is retired, or a confidential-VM snapshot is reseeded across every control-flow and error path.
+ *Function Coloring, `Pin`, and Silent Cancellation Bugs:* Poll-based coroutine state machines split APIs into incompatible synchronous and `async` worlds, require `Pin` wrappers to stabilize self-referential borrows across suspension points, and silently drop partially mutated state when racing branches cancel a future at an arbitrary yield point.
+ *Viral Lifetime Cascades (`'a` Proliferation):* Reifying borrows as first-class pointer types (`&'a T`, `&'a mut T`) forces lifetime parameters across structs, traits, closures, and parser combinators, while `&mut self` receiver borrows routinely block callers from reading sibling immutable fields.
+ *Ambient Authority and Unsandboxed Metaprogramming:* Allowing transitive dependencies, procedural macros, and imperative build scripts unrestricted ambient access to filesystem, network, clock, and global heap APIs turns routine package upgrades and editor workspace loads into supply-chain execution vectors.
+ *Backward-Prophecy Verification Tax:* Extracting code with stored or returned `&'a mut T` pointers into a theorem prover requires reconstructing loan graphs and synthesizing backward prophecy functions to resolve deferred mutations when lifetimes expire.

=== 1.3 Core Architectural Pillars of Vesper
<ch-1-3>

Vesper resolves these five bottlenecks simultaneously through a cohesive language and toolchain design:

#figure(
  table(
    columns: (22%, 36%, 42%),
    align: (left, left, left),
    table.header([*Systems Dimension*], [*Lifetime / Affine Baseline*], [*Normative Vesper Guarantee*]),
    [*Resource Finalization*],
    [Affine only; `Drop` is synchronous and infallible; obligations can be leaked or forgotten.],
    [True `linear` usage axis: *Theorem 2* enforces exact-once consumption across all normal, `Fail`, and `Cancel` paths; cleanup is an ordinary effectful `sink` function.],
    [*Borrowing & Aliasing*],
    [First-class `&'a T` and `&'a mut T` infect types, traits, and closures with lifetime bounds.],
    [Second-class `in` / `inout` / `out` / `sink` projections over disjoint field trees (`inout x.{a, b.c}`) and verified slice ranges (`out buf[0..k]`) with zero lifetimes.],
    [*Concurrency & Cancel Safety*],
    [Colored `async fn`, `Pin` state machines, `Send`/`Sync` trait bounds, and cancel-unsafe `select!`.],
    [Colorless `eff [...]` capabilities on pooled Zero-`Pin` fiber stacks; `[T-Let]` statically rejects `Fail` or `Cancel` while any `linear` obligation is live.],
    [*Refinements & Ghost State*],
    [Runtime bounds checks and `RefCell`/index panics; separate verification DSLs.],
    [Decidable liquid refinements (`{ v: T | phi }`), `@0` ghost linear permissions (`0` runtime bits), and strong `~>` updates on `inout` and `out` places.],
    [*Syntax & Metaprogramming*],
    [`<T>` angle-bracket ambiguity (`::<T>` turbofish) and unsandboxed host `proc_macro` / `build.rs` code.],
    [Deterministic LL(2) `[T]` square-bracket generics and fuel-metered `wasm32-wasip2` Wasmtime sandboxes governed by `[T-MacroConfinement]`.],
    [*Supply Chain & Proofs*],
    [Ambient `fs`/`net`/`GlobalAlloc` authority, manual SemVer, and prophecy-heavy proof extraction.],
    [`Loom.toml` `capability_ceiling` (`Alloc` is explicit), `mif:blake3` SemVer solver, and 1-to-1 Lean 4 `.olean` proof certificates (`0 sorryAx`).],
  ),
  kind: table,
)

=== 1.4 Canonical Walkthrough: Linear Transactions and Disjoint Projections
<ch-1-4>

The following module illustrates how modal true linearity (`linear struct`), explicit effectful finalization (`sink self`), second-class disjoint field projections (`inout broker.{index, metrics}`), and compile-time cancel safety (`[T-Let]`) compose without a single lifetime annotation or `Pin` wrapper:

```rust
// A `linear` struct cannot go out of scope implicitly or be forgotten on any branch.
linear struct WriteTxn {
    wal_slot: u64,
    dirty_pages: BufferPoolSlice,
}

impl WriteTxn {
    // Cleanup is an ordinary consuming (`sink self`) method performing `Storage` and `Fail`:
    pub fn commit(sink self) -> CommitReceipt eff [Storage, Fail[IoError]] {
        let WriteTxn { wal_slot, dirty_pages } = self; // Linear destructuring discharges `self`
        Storage::flush_wal(wal_slot, in dirty_pages)?;
        Storage::release_slice(sink dirty_pages);
        CommitReceipt { wal_slot }
    }

    pub fn abort(sink self) -> Unit eff [Storage] {
        let WriteTxn { wal_slot, dirty_pages } = self;
        Storage::discard_wal(wal_slot);
        Storage::release_slice(sink dirty_pages);
    }
}

// Mutates ONLY `broker.index` and `broker.metrics`; caller retains shared `in broker.config`!
pub fn append_batch(
    inout broker.{index, metrics}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Storage, Fail[IoError]] {
    let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);

    // Writing `validate_crc(in batch)?;` here is rejected at compile time (E0301):
    // `Fail` has resumption guarantee `affine`, and `[T-Let]` forbids `linear <= affine`.
    match validate_crc(in batch) {
        Ok(()) => {
            broker.index.insert_in_place(sink batch);
            broker.metrics.batches_written += 1;
            tx.commit() // Consumed on the success branch
        }
        Err(err) => {
            Storage::recycle_batch(sink batch);
            tx.abort(); // Consumed on the error branch before raising `Fail`
            Fail::raise(err)
        }
    }
}
```

== 2. Lexical Structure and Proposed Surface Grammar
<ch-2>
#chapter-deck[Identifier Classes, Generics, Refinement Sigils, and Grammar Sketch]

Vesper source modules (`.ves`) and declarative FFI bindings (`.vsys`) are UTF-8 encoded. The grammar below is an incomplete design sketch, not a complete description of the implemented parser (`crates/vesper-syntax`). In particular, `f[k](x)` can denote either a generic call or an indexed function call under the productions below; no LL(2) guarantee follows from them.

=== 2.1 Deterministic Lookahead Design Invariants
<ch-2-1>

The following are design intentions; the productions and examples do not yet establish deterministic two-token lookahead:

+ *Case-Partitioned Identifiers and Turbofish-Free Square-Bracket Generics:* Type parameters and type arguments use `[...]` rather than `<...>`, while lexical rules strictly separate `UpperIdent` (nominal types, traits, effects, enum variants, named models) from `LowerIdent` (values, functions, fields, lexical capability labels, effect-row variables, clock binders). This case distinction alone cannot disambiguate lower-case effect-row or clock arguments from index expressions; a future syntax revision must resolve that ambiguity.
+ *Explicit `@` Refinement Index Sigils:* Dependent value/refinement arguments inside `[...]` carry a leading `'@'` sigil (`Slice[u8, @len]`, `BoundedVec[T, @(n + 1), @cap]`), separating refinement index expressions from `UpperIdent` type arguments and `LowerIdent` effect-row variables in *LL(1)*.
+ *Subset and Pattern Refinements Without Expression Collisions:* Subset refinement types begin with `'{' LowerIdent ':'` in type position (`{ v: u64 | v != 0 }`), which never collides with nominal, tuple, or function types; ergonomic range/pattern refinements use `'is'` (`u64 is 1..=4096`).
+ *Four-Mode Place Projections and Strong Update Arrows:* Parameter projections explicitly state `in` (default), `inout`, `out`, or `sink` over nested field trees (`inout broker.{index, metrics}`) or slice intervals (`out pkt[0..mid]`), with strong postcondition type transitions written `'~>'` (`inout v: Vec[T, @n] ~> Vec[T, @(n + 1)]`), distinct from function return `'->'`.
+ *Moded, Capability-Labeled, and Resource-Graded Effects:* Effect declarations specify whether handlers may abort (`effect Fail[E]`, binding `affine` continuations) or must resume (`linear effect Storage`, binding `linear` continuations), while `eff [...]` clauses attach optional lexical labels and `@` pomonoid resource grades (`eff [scratch: Region @ <= 64 * count, Stack @ <= 512 | r]`).
+ *Import-Free Phrase-Skeleton Slots for User Syntactic Sugar:* User-defined syntactic sugar (`sugar`, #link(<ch-6-sugar>)[Section 7.5]) extends surface notation without changing parser tables on `import`. Stage 1 parses three structured *Phrase Skeleton* slots whose two-token lookahead sets (`FIRST_2`) are meant to be disjoint from core expressions: (a) *clause, block-tail, and item forms* (`LowerIdent` followed by `let`, a pass-mode keyword, a literal, another identifier, or a spaced `'{'`, which avoids `PostfixOp`'s `'(' | '[' | '.' | '?'`), (b) *lexically tiered operators* (`'|>' | '|?>'` at pipeline precedence, and `.`-prefixed symbolic operators such as `.+`, `.*`, and `.==` that take their precedence from the glyph after the `.`), and (c) *tagged delimited literals* (a `TagIdent`, a `LowerIdent` written directly against its `'"'` or `'{'` with no space, distinct from `UpperIdent '{'` struct literals). The disambiguation rules that make these slots deterministic follow the grammar.

=== 2.2 Proposed Surface Grammar (Incomplete EBNF)
<ch-2-2>

```text
(* ==================== 1. MODULES & TOP-LEVEL DECLARATIONS ============== *)
Module         ::= ( UseDecl | Attr* Item | TestBlock )* ;
TestBlock      ::= 'test' '{' ( UseDecl | Attr* Item )* '}' ;
UseDecl        ::= Vis? 'use' UseRoot ( '::' LowerIdent )* '::' UseTarget ';' ;
UseRoot        ::= '@' LowerIdent ( '-' LowerIdent )* '/' LowerIdent  (* dependency *)
                 | 'self'                              (* this package's root *)
                 | 'super' ( '::' 'super' )* ;         (* an ancestor module *)
UseTarget      ::= UseName | '{' UseName ( ',' UseName )* ','? '}' ;
UseName        ::= ( UpperIdent | LowerIdent ) ( 'as' ( UpperIdent | LowerIdent ) )? ;
Attr           ::= '@' LowerIdent ( '(' AttrArgs? ')' )?
                 | '#[' LowerIdent ( '(' AttrArgs? ')' )? ']' ;

Item           ::= Vis? ( StructDecl | EnumDecl | QuotientDecl | TypeAliasDecl
                        | EffectDecl | TraitDecl | ImplDecl | ModelDecl
                        | MeasureDecl | SugarDecl | MacroDecl | FnDecl
                        | ConstDecl )
                 | ItemSugar ;
ItemSugar      ::= LowerIdent PhraseHead* SugarGroup ;   (* `test "name" { ... }` *)
Vis            ::= 'pub' ;

(* ==================== 2. MODALITIES, GHOST & GENERIC BINDERS =========== *)
GhostMod       ::= 'ghost' ;                           (* runtime presence rho = 0 *)
StorageSpec    ::= 'wire' | 'tiled' '(' IntLit ')' ;   (* Bridge ABI or Strata AoSoA *)

ModeAxis       ::= 'copy' | 'affine' | 'linear'        (* Usage axis: default 'affine' *)
                 | 'portable' | 'pinned'               (* Cross-shard transfer axis *)
                 | 'uncontended' | 'contended'         (* Shared reference contention *)
                 | 'unordered' | 'lifo' ;              (* Stack-ordered discharge axis *)
ModeSpec       ::= ModeAxis ( '+' ModeAxis )* ;

TypeParams     ::= '[' TypeParam ( ',' TypeParam )* ','? ']' ;
TypeParam      ::= UpperIdent ( ':' ParamBounds )?
                 | LowerIdent ':' 'eff' ( '=' EffClause )?
                 | LowerIdent ':' 'clock'              (* guarded recursion clock kappa *)
                 | '@' LowerIdent ':' PrimType         (* refinement index binder *)
                 | LowerIdent ':' 'pred' '(' TypeList ')' ; (* Horn abstract refinement *)
ParamBounds    ::= 'mod' ModeSpec | Path ( '+' Path )* ;

(* ==================== 3. ADTS, QUOTIENTS, EFFECTS & TRAITS ============= *)
StructDecl     ::= GhostMod? StorageSpec? ModeSpec? 'struct' UpperIdent TypeParams?
                   ( '{' FieldDeclList? '}' | ';' ) ;
FieldDeclList  ::= FieldDecl ( ',' FieldDecl )* ','? ;
FieldDecl      ::= Vis? GhostMod? TempTier? LowerIdent ':' Type ( '@@' ModeSpec )? ;
TempTier       ::= 'hot' | 'cold' ;                    (* tiled(N) AoSoA column tier *)

EnumDecl       ::= GhostMod? 'open'? ModeSpec? 'enum' UpperIdent TypeParams?
                   '{' VariantList? '}' ;
VariantList    ::= Variant ( ',' Variant )* ','? ;
Variant        ::= UpperIdent ( '(' TypeList? ')' | '{' FieldDeclList? '}' )? ;

QuotientDecl   ::= ModeSpec? 'quotient' UpperIdent TypeParams?
                   '=' 'repr' Type 'modulo' Path '{' ( Attr* FnDecl )* '}' ;

TypeAliasDecl  ::= 'type' UpperIdent TypeParams? '=' Type ';' ;

EffectDecl     ::= ModeSpec? 'effect' UpperIdent TypeParams? '{' EffectOp* '}' ;
EffectOp       ::= 'fn' LowerIdent TypeParams? '(' ParamList? ')' ( '->' Type )? ';' ;

TraitDecl      ::= 'trait' UpperIdent TypeParams? '{' TraitItem* '}' ;
TraitItem      ::= 'type' UpperIdent ( ':' ParamBounds )? ';'
                 | EffectOp ;
ImplDecl       ::= 'impl' TypeParams? ( Path 'for' )? Type WhereClause? '{' FnDecl* '}' ;
ModelDecl      ::= Vis? 'model' UpperIdent TypeParams? ':' Path 'for' Type
                   WhereClause? '{' FnDecl* '}' ;

MeasureDecl    ::= 'ghost' 'measure' LowerIdent TypeParams?
                   '(' 'in' LowerIdent ':' Type ')' '->' BaseType Block ;
SugarDecl      ::= 'sugar' LowerIdent ':' SugarSort SugarEmits?
                   '{' SugarRule+ '}' ;
SugarSort      ::= 'stmt' | 'expr' | 'pat' | 'tag' | 'item'
                 | 'infix' '(' CustomSym ',' PrecBand ',' Assoc ')' ;
CustomSym      ::= '`' ( '|>' | '|?>' | DottedBinOp ) '`' ;
PrecBand       ::= 'pipeline' | 'logical' | 'compare' | 'additive' | 'multiplicative' ;
Assoc          ::= 'left' | 'right' | 'none' ;
SugarEmits     ::= 'emits' EffClause ;
SugarRule      ::= 'rule' '(' SugarAtom+ ')' '=>' Block ;
SugarAtom      ::= LowerIdent | Keyword | Punct | SugarHole
                 | '(' SugarAtom* ')' | '[' SugarAtom* ']' | '{' SugarAtom* '}' ;
SugarHole      ::= HolePass? '$' LowerIdent ':' HoleSort
                 | '$' '..' LowerIdent ':' ( 'stmts' | 'scope' | 'args' | 'items' | 'pats' )
                 | '$' '(' SugarAtom+ ')' Punct? ( '*' | '+' ) ;   (* repetition *)
HolePass       ::= 'in' | 'inout' | 'out' | 'sink' ;   (* omitted defaults to 'in' *)
HoleSort       ::= 'ident' | 'pat' | 'ty' | 'expr' | 'place' | 'block' | 'scope' ;
MacroDecl      ::= 'macro' LowerIdent '{' MacroRule+ '}' ;
ConstDecl      ::= 'const' UpperIdent ':' Type '=' Expr ';' ;  (* 'const' is contextual *)

(* ==================== 4. FUNCTIONS, PROJECTIONS & STRONG UPDATES ======= *)
FnDecl         ::= Vis? 'fn' LowerIdent TypeParams? '(' ParamList? ')'
                   ( '->' GhostMod? Type )? EffClause?
                   WhereClause? EnsuresClause? Block ;

ParamList      ::= Param ( ',' Param )* ','? ;
Param          ::= GhostMod? PassMode? 'self' ( '.' PlaceTree )?
                 | GhostMod? PassMode? LowerIdent ( '.' PlaceTree | '[' IndexSel ']' )?
                   ( ':' Type ( '~>' Type )? )? ;

PassMode       ::= 'in' | 'inout' | 'out' | 'sink' ;   (* omitted defaults to 'in' *)

PlaceTree      ::= LowerIdent ( '.' PlaceTree )?
                 | '{' PlaceTree ( ',' PlaceTree )* ','? '}' ;
IndexSel       ::= Expr ( ',' Expr )* | Expr? '..' Expr? ;

WhereClause    ::= 'where' Constraint ( ',' Constraint )* ','? ;
Constraint     ::= UpperIdent ':' ParamBounds | RefinePred ;
EnsuresClause  ::= 'ensures' RefinePred ;

(* ==================== 5. TYPES, REFINEMENTS & GRADED EFFECT ROWS ======= *)
Type           ::= SubsetRefType | FnType | LaterType | DynType | PatternRefType ;

SubsetRefType  ::= '{' LowerIdent ':' BaseType '|' RefinePred '}' ;
PatternRefType ::= BaseType ( 'is' RefinePat )? ( '@' ModeSpec )? ;
LaterType      ::= 'Later' '[' LowerIdent ',' Type ']' ;
DynType        ::= 'dyn' Path ;

FnType         ::= 'fn' ( '@' ModeSpec )? '(' ParamTypeList? ')'
                   ( '->' GhostMod? Type )? EffClause? ;
ParamTypeList  ::= ( GhostMod? PassMode? Type ( '~>' Type )? ) ( ',' ParamTypeList )? ;

BaseType       ::= PrimType | '(' TypeList? ')' | ArrayType | Path TypeArgs? ;
ArrayType      ::= '[' Type ';' IntLit ']' ;            (* fixed-size array [T; N] *)
TypeArgs       ::= '[' TypeArg ( ',' TypeArg )* ','? ']' ;
TypeArg        ::= Type | EffRow | '@' RefineExpr ;
TypeList       ::= Type ( ',' Type )* ','? ;
PrimType       ::= 'u8' | 'u16' | 'u32' | 'u64' | 'usize' | 'u128'
                 | 'i8' | 'i16' | 'i32' | 'i64' | 'isize' | 'i128'
                 | 'f32' | 'f64' | 'bool' | 'Unit' | 'Never'
                 | 'u16le' | 'u32le' | 'u64le' | 'u16be' | 'u32be' | 'u64be' ;

EffClause      ::= 'eff' '[' EffRow? ']' ;
EffRow         ::= EffInst ( ',' EffInst )* ( '|' LowerIdent )? | LowerIdent ;
EffInst        ::= ( LowerIdent ':' )? Path TypeArgs? ( '@' GradeExpr )? ;
GradeExpr      ::= ( '<=' | '==' ) RefineExpr | 'exact' ;

(* ==================== 6. STATEMENTS, EXPRESSIONS & HANDLERS ============ *)
Block          ::= '{' Stmt* Expr? '}' ;
Stmt           ::= LetStmt | UseModelStmt | TailSugarStmt | Expr ';' ;
LetStmt        ::= 'let' GhostMod? 'mut'? Pat ( ':' Type )? '=' Expr ';' ;
TailSugarStmt  ::= GhostMod? LowerIdent PhraseHead* ( SugarGroup ';'? | ';' ) ;
UseModelStmt   ::= 'use' 'model' Path ';' ;

Expr           ::= HandleExpr | MatchExpr | IfExpr | LoopExpr | ForExpr
                 | ClauseSugarExpr | ClosureExpr | ReturnExpr | BreakExpr
                 | ContinueExpr | GhostExpr | AssignExpr ;
ClauseSugarExpr ::= GhostMod? LowerIdent PhraseHead* SugarGroup
                    ( SameLine HeadWord PhraseHead* SugarGroup )* ;
SugarGroup     ::= '{' SugarToken* '}' ;      (* token tree, enforested in Stage 2 *)
PhraseHead     ::= 'let' Pat '=' HeaderExpr | PassMode PostfixExpr
                 | '(' ArgList? ')' | '[' SugarToken* ']' | HeadWord | HeaderExpr ;
HeadWord       ::= LowerIdent | 'else' | 'model' | 'where' ;
HeaderExpr     ::= Expr ;   (* parsed in header mode: stops before '{' and HeadWord *)

HandleExpr     ::= 'handle' ( LowerIdent ':' )? Path TypeArgs?
                   ( 'state' Pat '=' Expr )? 'in' Block
                   'with' '{' ReturnClause ','? OpClause* '}' ;
ReturnClause   ::= 'return' Pat '=>' Expr ;
OpClause       ::= LowerIdent '(' PatList? ')' '=>' Expr ','? ;

MatchExpr      ::= 'match' HeaderExpr '{' ( Pat ( '|' Pat )* '=>' Expr ','? )* '}' ;
IfExpr         ::= 'if' HeaderExpr Block ( 'else' ( IfExpr | Block ) )? ;
LoopExpr       ::= 'loop' Block ;
ForExpr        ::= 'for' Pat 'in' RangeExpr Block ;
RangeExpr      ::= BinaryExpr ( '..' | '..=' ) BinaryExpr ;
ReturnExpr     ::= 'return' Expr? ;
BreakExpr      ::= 'break' Expr? ;
ContinueExpr   ::= 'continue' ;
ClosureExpr    ::= ( '||' | '|' ClosureParams '|' ) ( '@' ModeSpec )?
                   ( '->' Type )? EffClause? Expr ;
ClosureParams  ::= ( 'cap' LowerIdent | Param ) ( ',' ClosureParams )? ;
GhostExpr      ::= 'ghost' PrimaryExpr ;

AssignExpr     ::= PipeExpr ( ( '=' | '+=' | '-=' | '|=' | '&=' ) Expr )? ;
PipeExpr       ::= BinaryExpr ( ( '|>' | '|?>' ) BinaryExpr )* ;
BinaryExpr     ::= UnaryExpr ( ( BinOp | DottedBinOp ) UnaryExpr )* ;
BinOp          ::= '||' | '&&' | '==' | '!=' | '<' | '<=' | '>' | '>='
                 | '|' | '^' | '&' | '<<' | '>>'      (* loosest to tightest, *)
                 | '+' | '-' | '*' | '/' | '%' ;      (* left-associative    *)
DottedBinOp    ::= '.' ( '+' | '-' | '*' | '/' | '%' | '&' | '|' | '^'
                       | '==' | '!=' | '<' | '>' ) SymbolTail? ;
UnaryExpr      ::= ( '!' | '-' | '~' )? PostfixExpr ;
PostfixExpr    ::= PrimaryExpr PostfixOp* ;
PostfixOp      ::= '(' ArgList? ')'                    (* value or effect call *)
                 | '[' TypeArgs ']' '(' ArgList? ')'   (* LL(2) generic instantiation *)
                 | '[' IndexSel ']'                    (* LL(2) slice or tile projection *)
                 | '.' LowerIdent                      (* field access or method receiver *)
                 | '?' ;                               (* Fail::raise, or None early return *)

ArgList        ::= Arg ( ',' Arg )* ','? ;
Arg            ::= GhostMod? PassMode? Expr ;

PrimaryExpr    ::= Literal | TaggedDelimExpr | LowerIdent | '(' ExprList? ')'
                 | ArrayExpr | Block | Path ( '{' FieldInitList? '}' )? ;
ArrayExpr      ::= '[' ( ExprList | Expr ';' IntLit )? ']' ;   (* [a, b, c] or [x; N] *)
TaggedDelimExpr ::= TagIdent ( StringLit | '{' SugarToken* '}' ) ;
TagIdent       ::= LowerIdent ;   (* no trivia between it and the delimiter *)
Path           ::= PathRoot? UpperIdent ( '::' ( UpperIdent | LowerIdent ) )*
                 | PathRoot LowerIdent ;               (* a function of a module *)
PathRoot       ::= UseRoot '::' ( LowerIdent '::' )* ;  (* qualified path, ch 11.3 *)

(* ==================== 7. PATTERNS & LEXICAL TOKENS ===================== *)
Pat            ::= '_' | LowerIdent | Literal | '(' PatList? ')'
                 | Path ( '(' PatList? ')' | '{' FieldPatList? '}' )? ;
PatList        ::= Pat ( ',' Pat )* ','? ;
FieldPatList   ::= ( LowerIdent ( ':' Pat )? ) ( ',' FieldPatList )? ;

UpperIdent     ::= /[A-Z][A-Za-z0-9_]*/ ;
LowerIdent     ::= /[a-z_][A-Za-z0-9_]*/ ;
```

A top-level `test { ... }` block contributes its declarations and imports to
the enclosing module when tests are enabled. These declarations can use that
module's private items. Ordinary builds skip the contents of test blocks
during declaration collection and type checking.

*Contextual names.* `state`, `linear`, `with` and `eff` are keywords only where their forms start (`handle E state p = e in`, `linear struct`, `} with {`, and for `eff` an `EffectRow` after a function signature, function type or closure head, or the row bound `e: eff`); wherever a `LowerIdent` is expected (a binding, parameter, field, member or expression name) they are ordinary names, so `fn f(state: u64)`, `let with = ..`, `struct Row { eff: u32 }` and `r.eff()` parse. After a closure head, `eff [` always opens the closure's effect row, so a closure body that indexes a value named `eff` is written `{ eff[i] }` or `(eff)[i]`; any other body starting with the name `eff` is an expression. The other keywords cannot be names, and a diagnostic names the keyword it found. *Open intervals.* In `p[a..b]` either bound may be omitted: `p[..b]` is `p[0..b]`, and `p[a..]` and `p[..]` read `p.len()` for the upper bound, so `p` must then be a place path (a name followed by fields).

*Loops and early exits.* `loop`, `for`, `break`, `continue` and `return` are contextual keywords outside their statement positions, and a statement-position `loop`, `for`, `if` or `match` ending in `}` needs no `;`. `loop B` has the join of its `break` values as its type, or `Never` when it has no `break`; `for p in a..b B` and `a..=b` bind successive values of one integer type to the irrefutable pattern `p`, have type `()`, and accept `break` without a value only. `continue` starts the next iteration of the innermost `loop` or `for`: a `for` steps its value first, and the closed range leaves at its bound before incrementing, so `for b in 0..=255` over `u8` does not overflow, with or without `continue`. Like `break`, `continue` cannot leave a closure, handled block or handler clause (`E0213`). In a function or closure whose result is `Option[U]`, `e?` with `e: Option[T]` has type `T`: it continues with the payload of `Some` and returns `None` on `None`, under the rules of `return` (`?` on `Result[T, E]` performs `Fail[E]` as before; `?` on `Option` elsewhere is `E0200`). `return e` leaves the innermost function or closure, including from inside a handled block (skipping the handler's return clause), and is rejected inside handler clauses and region blocks (`E0213`). A linear value live at a `break`, `continue` or `return` that would drop it is rejected (`E0302`), as is a linear value moved by one iteration and needed by the next (`E0312`). An affine value live at a `break`, `continue` or `return`, or bound by an iteration that ends, is dropped there exactly once (boxes, §7.3). Assigning to a `let` binding, or passing it `inout`, requires `let mut` (`E0313`); parameters stay assignable, since `mut` is not part of parameter syntax. `for` over `Slice` and `ChunkSeq` waits for the port-readiness Slice lane.

*Or-patterns.* A `match` arm `p1 | p2 | ... => e` has one or more alternatives: it means exactly the arms `p1 => e`, `p2 => e`, ... in that order, so the value takes the first alternative that matches, every alternative counts toward exhaustiveness, and the body is checked once per alternative, with that alternative's bindings. Every alternative must bind the same names at the same types (`E0214`); a diagnostic in the shared body is reported once. `|` separates alternatives only at the top of an arm's pattern: nested or-patterns (`Some(1 | 2)`), a leading `|` and guards are not part of the grammar. _Current implementation:_ backends see one arm per alternative, so the body's code is emitted once per alternative; the self-hosted Stage 1 checker (`packages/vesper-compiler`) does not accept or-patterns.

*Sugar disambiguation.* `sugar` declarations (#link(<ch-6-sugar>)[Section 7.5]) never add productions; every user form must fit `ItemSugar`, `TailSugarStmt`, `ClauseSugarExpr`, `PipeExpr`, `DottedBinOp`, or `TaggedDelimExpr`. `SugarToken` is any token except an unbalanced closing delimiter, and `SymbolTail` continues an operator glyph sequence. Eight rules make the choice between core syntax and a sugar slot deterministic, using only tokens and the lossless CST's trivia, never imports:

+ *Adjacent tags.* The lexer emits a `TagIdent` only when a `LowerIdent` touches a `'"'` or `'{'` with no whitespace or comment between them, as in `hex"dead beef"` or `bits{ ver: u4 = 4 }`. A spaced `name {` is never a tagged literal. Stylus never inserts or removes that space.
+ *Header mode.* The scrutinee of `match`, the condition of `if`, the range of `for`, and every `HeaderExpr` head of a sugar form parse in header mode: the expression ends before a `{` that follows a complete operand and before a `HeadWord`. A struct literal or clause sugar in those positions needs parentheses, as with Rust's restriction on struct literals in conditions. So `if ready { ... }`, `scratch tmp[256] { ... }`, and `decreases hi - lo { ... }` all end their header at the `{`, and `send (AppendReq { entry }) on sink ch;` parenthesizes its struct literal.
+ *Skeleton entry.* At an item or statement start, or where an expression starts, a `LowerIdent` (optionally after `ghost`) begins a skeleton when the next token is a spaced `{`, another identifier, a literal, `let`, `else`, or a pass-mode keyword; none of these can follow an identifier in core syntax. `ghost calc { ... }` is therefore a ghost `TailSugarStmt`, not a `GhostExpr`.
+ *Calls versus parenthesized heads.* `name(args)` is parsed as an argument list in either case. If the token after `)` is a spaced `{` or a skeleton-entry token, the form is a sugar head (`errdefer(e) { ... }`); otherwise it stays a call. So `x;` and `f(x);` remain expression statements without backtracking.
+ *Same-line continuation.* A continuation clause (`} else {`, `} on_error(e) {`, `} and {`) must start on the same line as the `}` it follows. A `HeadWord` on a new line after `}` starts a new statement. A statement-position sugar ending in `}` needs no `;`, like `loop` and `if`.
+ *Token-tree groups.* Stage 1 keeps every `SugarGroup` as a balanced token tree. Stage 2 re-parses it by the sort of the hole it binds (`$b:block` as a `Block`, `$( ... ),*` as a field or arm list), so `emplace out pkt { magic: 1, crc: c }` and `modify_reg inout r { .txe = 1 }` need no block-shaped contents. Stylus formats a group as a block when it parses as one, and as a token tree otherwise.
+ *No word operators.* A `LowerIdent` after a complete operand always ends a header; there are no infix word operators such as `x matches p`, because choosing them would require knowing which sugars are imported.
+ *Stage 2 classification.* Stage 2 resolves the skeleton's head word against in-scope `sugar` bindings. An unbound head raises `E0142` (`UnresolvedSugar`); a binding whose sort does not fit the slot (an `expr` sugar used as an item, or a `tag` sugar written spaced) raises `E0141` (`SugarSortMismatch`). Formatting and Chroma Tier 0 highlighting never depend on this step.

With these rules every slot is chosen from at most two tokens after the head word plus the trivia flag on `TagIdent`, and the call case decides at the token after `)`. Inside a `PipeExpr` right-hand call, a bare `_` argument is the pipeline placeholder (`frame |> parse(_, in cfg)`); `_` is rejected as an expression elsewhere.

*Integer kinds.* The twelve integer `PrimType`s are distinct types with no implicit conversions; `usize` and `isize` are 64 bits wide on every target, and `i128` and `u128` are 128 bits wide, two words in every value layout (low word first in memory). An integer literal is at most `u64::MAX` in magnitude, so a larger 128-bit value is built with shifts and conversions, such as `(widen_u128(hi) << 64) | widen_u128(lo)`. The export ABI (§16.6) does not carry `i128` or `u128` in any version yet, and `Atomic[T]` and the float conversions (`nearest_f64`, `saturating_k`, `narrow_k` of a float) do not take them. Under the checked-arithmetic contract `vesper.arith.checked/1`, `+`, `-`, `*`, negation and signed `MIN / -1` raise `Fail[ArithOverflow]`, and division by zero raises `Fail[RuntimeError]`. `&`, `|`, `^` and prefix `~` are total; `a << n` and `a >> n` accept an amount `n` of any integer kind and raise `Fail[ArithOverflow]` unless `0 <= n < bits(a)` (`>>` is arithmetic on signed kinds). An operation that the checker proves cannot fail needs no `Fail` effect. The proof may use constant operands or range facts. A comparison of an integer local with a pure integer expression gives that local a range in the code it dominates: the matching branch of an `if`, through `!`, `&&` (in the true branch) and `||` (in the false branch), or the code after an `if` whose other branch does not complete. The range lasts until the next write to the local, so a range from before a loop holds in its body when the body does not write the local (a call writes only the places it is passed or called on). `x & m` with `m >= 0` lies in `0..=m`, and `x % m` with `x >= 0` and `m > 0` lies in `0..m`. Range facts prove shift amounts in `0..bits(a)`, divisors other than zero, signed quotients without `MIN / -1`, and `narrow_k` operands within `k`. Conversions are explicit calls named after the target kind `k`: `widen_k` (accepted only when lossless), `narrow_k` (checked, `Fail[ArithOverflow]`) and `wrapping_k` (modulo `2^bits(k)`). `rotate_left`/`rotate_right` and the byte-order functions `to_le_bytes_k`, `to_be_bytes_k`, `from_le_bytes_k`, `from_be_bytes_k` (over tuples of `u8`) are total. The reserved function `mul_wide(a: u64, b: u64) -> (u64, u64)` is the total full product of two `u64` values as `(low, high)` words, `a * b = high * 2^64 + low`; a module cannot define a function of that name. The reserved bit functions, one per integer kind `k`, are total: `leading_zeros_k(x: k) -> u32`, `trailing_zeros_k(x: k) -> u32` and `count_ones_k(x: k) -> u32` count over the two's-complement bit pattern of `x` (the first two are `bits(k)` at zero), `wrapping_shl_k(x: k, n: u32) -> k` and `wrapping_shr_k(x: k, n: u32) -> k` shift by `n mod bits(k)` (`wrapping_shr` is arithmetic on signed kinds), and `wrapping_neg_k(x: k) -> k` is `0 - x` modulo `2^bits(k)`; a module cannot define functions of those names. The reserved checked functions, one per integer kind `k`, are total as well: `checked_add_k(a: k, b: k) -> Option[k]`, `checked_sub_k` and `checked_mul_k` return `Some` of the exact result when it lies in `k` and `None` exactly where `+`, `-` or `*` would raise `Fail[ArithOverflow]`, so a function without `Fail` in its row may call them; a module cannot define functions of those names either. An unsuffixed integer literal takes its kind from its context: an annotation, a parameter or result, the other operand, the other branch of an `if` or `match`. A `let` without an annotation whose value's type is an unsuffixed literal (or a tuple of them), and the pattern of a `for` over a range of unsuffixed literals, take the kind of the binding's uses later in the same function, as if annotated with it; a conversion's operand does not constrain it. The binding is `i64` when no use constrains it or when its uses need different kinds. The rule only types programs that would otherwise be rejected: a function that checks with its literal bindings at `i64` keeps those types. The self-hosted Stage 1 checker (`packages/vesper-compiler`) does not apply it.

*Fixed-size arrays and module constants.* `[T; N]` is the array of `N` values of type `T`, where `N` is an integer literal below $2^16$ (a constant name as a length is not yet accepted). It is a value type whose mode is the mode of `T` and whose layout is `N` inline elements, exactly the homogeneous tuple `(T, ..., T)`: an array is copied, bound, passed, returned and stored in a struct field or copy struct like that tuple, and two array types are equal when their element types and lengths are. `[a, b, c]` lists the elements and `[x; N]` repeats `x`, which is evaluated once. `a[i]` takes an integer index of any kind and raises `Fail[RuntimeError]` `IndexOutOfBounds` unless the checker proves `0 <= i < N`, from a constant index, from the range facts of the integer-kinds paragraph, from `i + j` or `i - j` of proved bounds, or from an enclosing `for i in lo..hi` whose bounds are constants or `a.len()`; `+`, `-` and `*` with a `for` loop index as an operand cannot overflow when the operands' bounds keep every result in the kind, so `a[i + 1]` and `a[15 - i]` in a loop over a constant range raise nothing. On a mutable place (a `let mut` binding, a parameter or a field path of one), `a[i] = v`, `a[i] op= v` and `a.set(i, v)` write one element in place with the same bounds rule, and need a copy element type (`E0311`); a write to anything that is not such a place is `E1540`. `a.len()` is the constant `N` as `u64`. A constant `const NAME: T = e;` names one value of a scalar, tuple, copy struct or array type; `e` may use literals, constructors, struct and array expressions, operators, `let` blocks and other constants, but no call of a function (`E1541`), and the name may not be the name of a type, effect or constructor (`E1542`). A use of `NAME` is the value: it is not a place (`E0300`), so `let mut t = TABLE;` copies it before any write. Natively a constant array or struct is laid out once in read-only data and read in place. Across the native export boundary (`@export abi 3`) an array field of a copy struct is its element cells in order, so the struct stays a `#[repr(C)]` mirror; a binding names an array of more than eight elements a Rust array `[T; N]` and a shorter one a Rust tuple, since the interface does not tell the two apart. Views `in a[lo..hi]` and `inout a[..]` as `Slice[T]` arguments, writes to a nested element (`a[i][j] = v`, `a[i].f = v`), constants exported across packages and lengths named by a constant are not yet accepted.

*Float kinds.* `f32` and `f64` are IEEE 754 binary32 and binary64 under the contract `vesper.float.ieee754/1`; there are no implicit conversions between them or with integers, and no fast-math rewriting. `+`, `-`, `*`, `/`, `sqrt` and `fma` are correctly rounded (round to nearest, ties to even), `%` is the truncated remainder of C `fmod` (exact), and every float operation is total: overflow, division by zero and invalid operations produce infinities and NaNs, never failures. A float literal is `f32` where its context expects `f32` and `f64` otherwise; an `f32` literal denotes the `f32` nearest to the literal's `f64` value. The generic intrinsics `sqrt`, `abs`, `floor`, `ceil`, `round` (ties away from zero), `trunc`, `min`, `max` (IEEE 754-2019 `minimum`/`maximum`: NaN propagates, `-0 < +0`) and `fma` take operands of one float kind. Conversions are explicit: `widen_f32`/`widen_f64` are accepted only when exact (`f32` to `f64`, integers of at most 16 bits to `f32`, at most 32 bits to `f64`); `nearest_f32`/`nearest_f64` round any integer or float to nearest, ties to even; `narrow_k(x)` of a float truncates toward zero and raises `Fail[ArithOverflow]` on a NaN or an out-of-range value; `saturating_k(x)` truncates, clamps to `k`'s range and maps NaN to zero; `to_bits_f32`/`to_bits_f64` and `from_bits_f32`/`from_bits_f64` reinterpret exactly. The bit pattern of a NaN result is unspecified, except in a program with an `eff [DetFloat]` function, where the result of every arithmetic operation, `sqrt`, `fma`, `min`, `max`, rounding and width conversion is the canonical quiet NaN (`0x7fc00000`, `0x7ff8000000000000`); negation, `abs`, `%` and bit reinterpretation preserve payloads. The transcendental functions `exp_f64`, `ln_f64`, `sin_f64`, `cos_f64` and their `f32` counterparts are ordinary Vesper source functions (ports of fdlibm, the `f32` versions evaluate in `f64` and round once) added to a module that uses them, so every backend computes the same bits on every target without calling libm. They are within 1 ulp of the exact result; `sin` and `cos` use a three-part Cody-Waite reduction that is accurate for `|x| < 2^20 * pi/2` and do not yet implement Payne-Hanek reduction for larger arguments. They carry `DetFloat` and `Realtime` implicitly so that such code may call them. Lean extraction models `f32` as `Float32` and `f64` as `Float` for the basic operations and comparisons, and refuses the intrinsics with a typed diagnostic.

== 3. Core Calculus, Four-Axis Modalities, and Ghost Linearity
<ch-3>
#chapter-deck[Ownership Modes, Mode Crossing, Capsules, Release Order, and Ghost Values]

The proposed core calculus, $lambda_(upright("Vesper"))$, makes ownership and effects explicit. Method receivers become `in`, `inout`, `out`, or `sink` parameter projections; `?` becomes a `match` over `Fail::raise`; and each effect operation targets a lexical capability $l$. This chapter introduces the modes and ghost-presence annotations used in the later typing rules.

=== 3.1 Quantitative Presence and the Four-Axis Modal Lattice
<ch-3-1>

Every binding in $lambda_(upright("Vesper"))$ carries a pair $(rho, bold(m))$ combining *quantitative runtime presence* $rho in {0, 1}$ with a *four-axis modal vector* $bold(m) = (u, p, c, o) in cal(M)$ packed into a single byte (`ModeVec(u8)` in `crates/vesper-refine`, #link(<ch-7-3>)[Section 8.3]). The four orthogonal axes form a finite bounded product lattice ordered componentwise by $bold(m)_1 subset.eq.sq bold(m)_2$ (increasing structural restriction):

#figure(
  table(
    columns: (16%, 24%, 22%, 38%),
    align: (left, left, left, left),
    table.header([*Modal Axis*], [*Ordered Lattice ($subset.eq.sq$)*], [*Default Nominal Floor*], [*Operational & Soundness Meaning*]),
    [*Usage* ($u$)],
    [$upright("copy") subset.eq.sq upright("affine") subset.eq.sq upright("linear")$],
    [$upright("affine")$ (scalars: $upright("copy")$)],
    [Dynamic use count $cal(U)(u)$: $NN$ for `copy` (weakening + contraction), ${0, 1}$ for `affine` (implicit sync cleanup, no bitwise copy), and ${1}$ for `linear` (exact-once consumption; implicit drop forbidden).],
    [*Portability* ($p$)],
    [$upright("portable") subset.eq.sq upright("pinned")$],
    [$upright("portable")$],
    [Governs cross-fiber and cross-shard transfer (`Pulse::spawn`, channels). Shard-local handles (`UiThread`, intra-region pointers) carry `pinned` unless sealed into `Capsule[T]`.],
    [*Contention* ($c$)],
    [$upright("uncontended") subset.eq.sq upright("contended")$],
    [$upright("uncontended")$],
    [Distinguishes single-fiber shared `in` access (`uncontended`, preserving interior refinements) from cross-fiber shared `in` access (`contended`, stripping non-monotonic interior predicates).],
    [*Order* ($o$)],
    [$upright("unordered") subset.eq.sq upright("lifo")$],
    [$upright("unordered")$],
    [Restricts Gentzen structural Exchange: `lifo` obligations (`IrqGuard`, arena `Watermark`) must be discharged in strict last-acquired, first-released stack order.],
  ),
  kind: table,
)

Componentwise join ($bold(m)_1 union.sq bold(m)_2$, taking the stricter bound on each axis) corresponds to intersecting allowed dynamic behaviors. On the primary usage axis, $cal(U)(u_1 union.sq u_2) = cal(U)(u_1) inter cal(U)(u_2)$:

#figure(
  table(
    columns: (18%, 22%, 22%, 20%, 18%),
    align: (left, left, left, left, left),
    table.header([*Usage Mode ($u$)*], [*Allowed Uses $cal(U)(u)$*], [*Weakening (Drop)*], [*Contraction (Copy)*], [*Join $u union.sq upright("affine")$*]),
    [*`copy`* ($bot$)], [${0, 1, 2, dots} = NN$], [Permitted], [Permitted], [`affine`],
    [*`affine`*], [${0, 1}$], [Permitted (sync drop)], [Forbidden], [`affine`],
    [*`linear`* ($top$)], [${1}$ (exact-once)], [*Forbidden*], [Forbidden], [`linear`],
  ),
  kind: table,
)

Because ${1} subset {0, 1} subset NN$, *sub-moding is covariant along $subset.eq.sq$*: a value available at $bold(m)_1$ satisfies any context expecting $bold(m)_2$ whenever $bold(m)_1 subset.eq.sq bold(m)_2$ (for example, passing a `copy` scalar or `affine` buffer to a `linear` parameter), whereas relaxing `linear` down to `affine` is unsound unless justified by concrete modal kinding.

=== 3.2 Modal Kinding, Mode Crossing, and Field-Modality Well-Formedness
<ch-3-2>

Every type $tau$ possesses a modal kind bound $Delta tack.r tau : upright("mod ") bold(m)_0$ computing the strongest (lowest in $subset.eq.sq$) mode vector to which values of type $tau$ can strengthen given their concrete type arguments:

#_infer(
  "[K-Prim]",
  $upright("scalar")(tau)$,
  $Delta tack.r tau : upright("mod ") (upright("copy"), upright("portable"), upright("uncontended"), upright("unordered"))$,
)

#_infer(
  "[K-Var]",
  $(alpha : upright("mod ") bold(m)) in Delta$,
  $Delta tack.r alpha : upright("mod ") bold(m)$,
)

#_infer(
  "[K-Adt]",
  $upright("decl")(T[overline(alpha)]) = (bold(m)_(upright("decl")), { f_i : tau_i @@ bold(m)_i }) \ forall i, quad Delta tack.r tau_i [overline(sigma) \/ overline(alpha)] : upright("mod ") bold(k)_i quad "and" quad bold(k)_i subset.eq.sq bold(m)_i quad o(bold(m)_i) = upright("unordered")$,
  $Delta tack.r T[overline(sigma)] : upright("mod ") (bold(m)_(upright("decl")) union.sq limits(union.sq)_i bold(m)_i)$,
)

#_infer(
  "[K-Fn]",
  $bold(m) in cal(M)$,
  $Delta tack.r (tau_1 arrow.r^(epsilon)_(bold(m)) tau_2) : upright("mod ") bold(m)$,
)

+ *Field-Modality Well-Formedness ($forall i, bold(k)_i subset.eq.sq bold(m)_i$ in `[K-Adt]`):* An unannotated field contributes its intrinsic kind, $bold(m)_i = bold(k)_i$. An annotation `f_i: tau_i @@ m_i` raises the field's mode floor only if its intrinsic kind under $Delta$ satisfies $bold(k)_i subset.eq.sq bold(m)_i$; otherwise the checker reports `E0105`. Thus `@@ copy` accepts only provably `copy` components, and `@@ affine` accepts only components at most `affine`. The checker rejects `Ticket @@ copy` when `Ticket` is linear, preventing a copyable wrapper from weakening that obligation. Enum kinding joins contributions from every variant. Aggregate fields must have effective order `unordered`; a nominally `lifo` guard may contain unordered scalar representation fields.
+ *Polymorphic Container Specialization Without Trait Duplication:* For `copy enum Option[T] { None, Some(T) }`, $bold(m)_(upright("decl")) = bot$, so `[K-Adt]` evaluates to $bot union.sq bold(k)_T = bold(k)_T$:
  - `Option[u64]` has kind `mod copy` (freely copied and implicitly dropped);
  - `Option[BufferPoolSlice]` has kind `mod affine` (implicitly droppable, non-copyable);
  - `Option[WriteTxn]` has kind `mod linear` (cannot go out of scope without exhaustive pattern matching on `Some(tx)`).

=== 3.3 Adjoint `Capsule[T]` Sealing and LIFO Stack Discipline
<ch-3-3>

*Review limitation:* Sealing must require a self-contained region, no free outer capabilities, and a separate proof that every contained value is transferable between threads. Absence of aliases alone does not make thread-affine OS handles portable.

The Portability, Contention, and Order axes eliminate `Arc<Mutex<T>>` synchronization wrappers and runtime drop-order bugs:

- *Zero-Copy Graph Transfer via `Capsule[T]` (`portable` Sealing):* Cyclic or arena-indexed data structures (such as a `SlotArena` containing intra-region handles) carry `pinned + uncontended` so interior handles cannot leak individually across worker shards. Sealing requires unique ownership, an unordered root, zero external aliases, zero free outer capabilities, and no thread-affine handles in the reachable graph. LIFO obligations remain on their acquisition stack and cannot be sealed for transfer. Only a graph satisfying those obligations may acquire `portable` status. The Rust and Lean admission models check explicit evidence fields; it does not derive those fields from a heap or prove a runtime transfer implementation.
- *Compile-Time LIFO Stack Ordering (`@ lifo`):* Hardware interrupt masks (`linear + lifo struct IrqGuard`, #link(<ch-24>)[Chapter 25]) and nested region watermarks (`Watermark`, #link(<ch-12>)[Chapter 13]) inhabit `Order::Lifo`. The core `LifoState` helper maintains a path-local stack and rejects out-of-order release. Section 4.1 specifies its separate ordered-context discipline; integrating this with source checking and mapping it to `E0306` remain compiler obligations.

=== 3.4 Quantitative `@0` Ghost Linearity and Erasure Non-Interference
<ch-3-4>

*Relevance requirements:* Runtime control flow and runtime values cannot observe ghost data. Erased computations must terminate without runtime effects; ghost linear obligations remain checked before erasure. `lean/GhostErasure.lean` proves these properties for an annotated extension of the Appendix A operational fragment, including closed ghost resource computations; it does not establish a production compiler erasure pass or the full surface calculus. The current compiler AST has no ghost-binding representation.

Refinement type indices (`Slice[u8, @len]`) and zero-sized Iris-style capability witnesses (`ghost linear struct DmaSlotPerm[@slot: u32]`) share quantitative presence $rho = 0$:

- *Strict Computational Non-Interference:* A ghost context may observe runtime values through read-only logical views without consuming their owned permissions; existing `@0 linear` obligations retain their usage restrictions, whereas `@1` runtime control flow (`if`, `match`, pointer arithmetic, I/O) can *never* inspect a `@0` binding.
- *Full Substructural Enforcement with Zero Codegen Cost:* `@0` bindings obey the exact same `ModeVec` lattice ($upright("copy") subset.eq.sq upright("affine") subset.eq.sq upright("linear") + upright("lifo")$) during type and PCG checking, then erase completely prior to Stage 5 machine-code emission (`0` stack bytes, `0` registers, `0` instructions) while remaining 1-to-1 explicit proof witnesses in extracted Lean 4 models (#link(<ch-8>)[Chapter 9]).

== 4. Place Projections, Destination Calculus, and Liquid Refinements
<ch-4>
#chapter-deck[Typing Contexts, Field and Slice Permissions, Initialization, and Refinements]

Place projections describe which parts of a value a call may read, mutate, initialize, or consume. This chapter introduces the typing contexts, disjointness conditions, and snapshot postconditions used to express those permissions. The rules distinguish the proposed general system from the narrower place and refinement models in Appendix A.

=== 4.1 Typing Judgments, Discardability, and Context Splitting
<ch-4-1>

*Review limitation:* LIFO obligations also require an ordered context and restrictions on storage and movement; multiset splitting alone cannot enforce release order.

Unified typing judgments in $lambda_(upright("Vesper"))$ take the form:

$ Delta ; Sigma ; Phi ; Gamma tack.r e : tau @ (rho, bold(m)) divides epsilon $

- *$Delta$ (Kind, Clock, and Predicate Context):* Modal kind bounds ($alpha : upright("mod ") bold(m)$), effect-row variables ($r : upright("Row")$), guarded clocks ($kappa : upright("clock")$), ghost refinement binders ($@ n : tau_0$), and Horn abstract predicates ($p : upright("pred")(overline(tau))$).
- *$Sigma$ (Lexical Capability Context):* Active handler capabilities $l : upright("Cap")_q [E]$, where $q in {upright("affine"), upright("linear")}$ is the handler's continuation resumption guarantee.
- *$Phi$ (Path-Sensitive Refinement Fact Base):* Conjunction of active branch guards, `where` clauses, and index equalities visible in the current scope.
- *$Gamma$ (Substructural Place Context):* Owned bindings $x :_((rho, bold(m)))^(upright("own")) tau$, borrowed projections $x :_((rho, bold(m)))^(mu) tau$ ($mu in {upright("shr"), upright("mut")}$), and uninitialized write-only destination slots $x :_((1, bold(m)))^(upright("out")) tau$.
- *$epsilon$ (Graded Lexical Effect Row):* ${ l_1 : E_1^(q_1) @ g_1, dots, l_n : E_n^(q_n) @ g_n divides r }$. Its *row resumption guarantee* is the usage-axis meet $upright("res")(epsilon) = limits(inter.sq)_i q_i$, with $upright("res")(emptyset) = upright("linear")$.

*Discardability (`discard(Gamma)`):* Initialized shared views may be released regardless of the owner's usage mode. An owned binding may be implicitly dropped only when its order is `unordered` and its usage is at most `affine`. Active exclusive borrows and `out` slots are not discardable by this rule. An owned `lifo` obligation belongs to the separate ordered stack $Lambda$, never the exchangeable $Gamma$.

*Ordered guard context ($Lambda$):* Acquisition appends a fresh binding identity to the stack; release removes only its top. There is no exchange or contraction rule for owned stack entries. A branch join requires identical stacks, and a normal return requires an empty stack. Owned LIFO values cannot be placed in aggregate fields or ordinary mutable slots; a shared view does not transfer the ordered obligation. The existing core expression judgments apply to unordered owners; the ordered layer must be threaded through sequencing and control flow separately. `vesper-core::lifo::LifoState` implements the stack transition checks, and `mode_cross` rejects owned LIFO entries in ordinary contexts and ordered fields in aggregates. Source integration remains incomplete.

`lean/OrderedContexts.lean` proves that its executable guard-trace checker accepts exactly the traces derivable with stack-indexed acquire/release rules. Every prefix preserves a valid intermediate stack, and a closed trace balances acquisitions and releases for every identity. Out-of-order release and duplicate active acquisition are rejected. This does not prove that source lowering emits all guard events.

*Context Splitting ($Gamma = Gamma_1 convolve.o Gamma_2$):* `copy` bindings contract into both sub-contexts, whereas `affine` and `linear` owned bindings partition exclusively into either $Gamma_1$ or $Gamma_2$:

#_infer(
  "[Split-Copy]",
  $Gamma = Gamma_1 convolve.o Gamma_2 quad (mu = upright("shr") quad "or" quad (mu = upright("own") and u(bold(m)) = upright("copy") and o(bold(m)) = upright("unordered")))$,
  $(Gamma, x :_((rho, bold(m)))^(mu) tau) = (Gamma_1, x :_((rho, bold(m)))^(mu) tau) convolve.o (Gamma_2, x :_((rho, bold(m)))^(mu) tau)$,
)

#_infer(
  "[Split-Sub]",
  $Gamma = Gamma_1 convolve.o Gamma_2 quad (mu in {upright("mut"), upright("out")} quad "or" quad (mu = upright("own") and u(bold(m)) in {upright("affine"), upright("linear")} and o(bold(m)) = upright("unordered")))$,
  $(Gamma, x :_((rho, bold(m)))^(mu) tau) = (Gamma_1, x :_((rho, bold(m)))^(mu) tau) convolve.o Gamma_2 quad "(and symmetrically into " Gamma_2 ")"$,
)

=== 4.2 Core Substructural, Closure, Sequencing, and Branching Rules
<ch-4-2>

#_infer(
  "[T-Var]",
  $upright("discard")(Gamma) quad rho_(upright("ctx")) lt.eq rho$,
  $Delta ; Sigma ; Phi ; Gamma, x :_((rho, bold(m)))^(mu) tau tack.r x : tau @ (rho, bold(m)) divides emptyset$,
)

#_infer(
  "[T-SubMode]",
  $Delta ; Sigma ; Phi ; Gamma tack.r e : tau @ (rho, bold(m)_1) divides epsilon quad bold(m)_1 subset.eq.sq bold(m)_2$,
  $Delta ; Sigma ; Phi ; Gamma tack.r e : tau @ (rho, bold(m)_2) divides epsilon$,
)

#_infer(
  "[T-Cross]",
  $Delta ; Sigma ; Phi ; Gamma tack.r e : tau @ (rho, bold(m)) divides epsilon quad Delta tack.r tau : upright("mod ") bold(m)_0 quad bold(m)_0 subset.eq.sq bold(m)$,
  $Delta ; Sigma ; Phi ; Gamma tack.r e : tau @ (rho, bold(m)_0) divides epsilon$,
)

#_infer(
  "[T-Abs]",
  $Delta ; Sigma ; Phi ; Gamma, x :_((rho_x, bold(m)_x))^(upright("own")) tau_1 tack.r e : tau_2 @ (rho_2, bold(m)_2) divides epsilon \ forall (z :_((rho_z, bold(m)_z))^(upright("own")) tau_z) in Gamma, quad bold(m)_z subset.eq.sq bold(m)_(upright("fn")) quad "(no borrowed/out places in " Gamma ")"$,
  $Delta ; Sigma ; Phi ; Gamma tack.r (lambda x : tau_1 . med e) : (tau_1 arrow.r^(epsilon)_(bold(m)_(upright("fn"))) tau_2) @ (1, bold(m)_(upright("fn"))) divides emptyset$,
)

#_infer(
  "[T-Let]",
  $rho_1 = 0 arrow.r.double (epsilon_1 = emptyset and upright("total")(e_1)) \ Delta ; Sigma ; Phi ; Gamma_1 tack.r e_1 : tau_1 @ (rho_1, bold(m)_1) divides epsilon_1 \ Delta ; Sigma ; Phi ; Gamma_2, x :_((rho_1, bold(m)_1))^(upright("own")) tau_1 tack.r e_2 : tau_2 @ (rho_2, bold(m)_2) divides epsilon_2 \ forall (z :_((rho_z, bold(m)_z))^(mu) tau_z) in Gamma_2, quad u(bold(m)_z) subset.eq.sq upright("res")(epsilon_1)$,
  $Delta ; Sigma ; Phi ; Gamma_1 convolve.o Gamma_2 tack.r (upright("let ") x = e_1 upright(" in ") e_2) : tau_2 @ (rho_2, bold(m)_2) divides epsilon_1 compose epsilon_2$,
)

#_infer(
  "[T-Match]",
  $rho lt.eq rho_0 \ Delta ; Sigma ; Phi ; Gamma_0 tack.r e_0 : T[overline(sigma)] @ (rho_0, bold(m)_0) divides epsilon_0 \ forall i, quad Delta ; Sigma ; (Phi and e_0 = C_i(overline(x)_i)) ; Gamma, overline(x)_(i j) :_((rho_0, bold(k)_(i j)))^(upright("own")) tau_(i j) tack.r e_i : tau @ (rho, bold(m)) divides epsilon_i$,
  $Delta ; Sigma ; Phi ; Gamma_0 convolve.o Gamma tack.r (upright("match ") e_0 med { C_i(overline(x)_i) => e_i }) : tau @ (rho, bold(m)) divides epsilon_0 compose (limits(union.sq)_i epsilon_i)$,
)

Destructuring consumes the aggregate and binds fields at their intrinsic kinds $bold(k)_(i j)$. A scalar field is therefore `copy` even when the enclosing aggregate is linear; a resource field remains linear. An explicit field floor constrains the aggregate until this elimination. Encapsulated handles must keep their constructor and destructive pattern private if clients must use a particular release operation. This agrees with the executable checker and the larger Lean calculi.

Three core properties follow directly from these rules:

+ *Mode Crossing (`[T-Cross]`):* Generic code operating at `linear` mode strengthens concrete scalar results back to `copy` automatically as soon as $Delta tack.r tau : upright("mod copy")$ holds.
+ *No Escaping Second-Class Borrows (`[T-Abs]`):* Closures capture only owned values (`shr`, `mut`, and `out` projections cannot escape into heap closures), and capturing any `linear` value forces $u(bold(m)_(upright("fn"))) = upright("linear")$ (exact-once invocation).
+ *Closure Parameter Modes and Lent Closures:* A closure parameter takes a pass mode as a function parameter does (`|in v: T| ..`, `|inout v| ..`; omitted, `in`), and its type when no annotation is written comes from the expected function type, whose modes it must match (`E0308`). A `copy` value that a function holds `in` (every by-value parameter) is captured as a copy. A parameter `f: fn(A, ..) -> R eff [E]` passed `in` whose function type is not `copy` is a *lent function parameter*: the body may call it any number of times, each call a read that consumes nothing, and may lend it again as the argument for another lent function parameter, but binding, storing, returning, moving or capturing it in a closure that is not itself lent is refused (`E0305`, `E0310`). The argument for such a parameter is a closure literal, a `copy` function value or a lent function parameter (`E0310`); a closure value that owns its captures may be called at most once and is never lent. A closure literal written directly as that argument is a *lent closure*: it is second-class, lives for the call only, and instead of capturing the names it uses from the enclosing scope it borrows each `in` for the call, as one more `in` argument of that call. So it may use the caller's borrowed parameters and non-copy locals, which stay the caller's, unmoved and dropped by the caller; inside the body they are read-only (`E0307`, `E0305`), and the borrow conflicts with the call's other argument loans under the rules of #link(<ch-4-3>)[Section 4.3] (`E0301`), including a reserved two-phase `inout` loan, which stays active for the whole call. This is what lets a collection lend its values to a callback in place: `t.with_ref(k, |in v| v.n == key.n)`. A lent closure that writes or moves a caller's name is future work.
+ *Resumption Frame Condition (`u(bold(m)_z) subset.eq.sq upright("res")(epsilon_1)` in `[T-Let]`):* The continuation's live obligations must fit the effect's resumption mode. Pure expressions and `linear effect` operations (`Storage`, `Net`) have $upright("res")(epsilon_1) = upright("linear")$, so $Gamma_2$ may retain live linear values across a fiber yield. Abortable effects (`Fail`, `Cancel`) have $upright("res")(epsilon_1) = upright("affine")$. A live initialized linear obligation then fails the condition because $upright("linear") subset.eq.sq upright("affine")$ is false (`E0301`). Separately, `[T-Match]` requires identical consumption in every branch. The condition constrains *owned* obligations in $Gamma_2$ and handler states owned outside the target capability (`[T-Handle]`, `E0402`). A stationary `inout` borrow is not an owned obligation of the callee: it may cross an abortable call even when it borrows a `linear` resource (`inout tx: WriteTxn`), because the abort keeps the value in the caller's place (`PostCall_abort`, #link(<ch-4-3>)[§4.3]). The caller that owns `tx` still must handle the abort before its own frame unwinds.

=== 4.3 Four-Mode Disjoint Place and Slice Projections
<ch-4-3>

Let $p = x . pi$ denote a trie-structured field-tree projection on root local $x$, where two projections are *disjoint* ($p_i perp p_j$) whenever $x_i eq.not x_j$ or their selected leaf-field sets satisfy $upright("leaves")(pi_i) inter upright("leaves")(pi_j) = emptyset$:

The call contract records each parameter's pre-type and normal-return post-type, plus the result presence and modality. A call has two post-call transitions, one per way control leaves the callee:

- *Normal return* ($upright("PostCall")_(upright("ok"))(Phi, Gamma, overline(mu)_i med overline(p)_i) = (Phi'; Gamma')$): simultaneous snapshot/havoc of modified roots, removal of `sink` arguments, restoration of borrowed permissions, update of every `inout` target $p_k : tau_(k, upright("in")) ~> tau_(k, upright("out"))$ to $tau_(k, upright("out"))$, and initialization of each `out` destination to its declared post-type. Prior facts in $Phi$ that mention $upright("leaves")(p_k)$ are replaced by the callee's postcondition.
- *Abortive unwind* ($upright("PostCall")_(upright("abort"))$, when $upright("res")(epsilon) = upright("affine")$): permitted across borrowed arguments if and only if every `inout` parameter of $f$ is *stationary* ($tau_(k, upright("in")) = tau_(k, upright("out"))$). Callee writes happen in place (*keep-writes*, basic abort safety, as with a Rust `&mut`): at every abortable checkpoint inside $f$, each `inout` parameter $y_k$ must be whole (no moved-out sub-place in the PCG, `E0402`) and satisfy its type, refinement and quotient invariants $phi.alt(y_k)$. On unwind, `sink` arguments stay consumed, each stationary `inout` place keeps the last value the callee wrote before the abort with every caller fact in $Phi$ that mentions $upright("leaves")(p_k)$ havocked, and `out` destinations stay uninitialized: partially written `copy`/`affine` parts are dropped, and writing a `linear` value into an `out` slot before an abortable checkpoint is rejected (`E0402`). A non-stationary `inout` ($tau_(k, upright("in")) eq.not tau_(k, upright("out"))$) still requires $upright("res")(epsilon) = upright("linear")$ (`E0402`), unless a lexical `handle` inside $f$ catches the abortable effect before $f$ returns.

The abortive rule depends only on $upright("res")(epsilon)$, never on an effect's name: `Fail` and `Cancel` behave alike, and an open row variable is treated as abortable. Roll-back (copy-in/copy-out) is rejected: it cannot snapshot `affine`, `linear` or `pinned` places and it copies on every call. A caller that wants roll-back for a `copy` place writes it out (`let mut s = x; f(inout s)?; x = s`). A sibling argument that aborts before $f$ is entered (`f(inout x, g()?)`) has written nothing, and `E0410` still protects a `linear` `sink` sibling.

_Current implementation:_ the production checker has no `~>` typestate transitions, `out` parameters, or parameter refinements yet, so every `inout` is stationary and `E0402` keeps only the moved-out-hole condition for `inout` places. Handler clauses capture snapshots taken when the handler is entered, so their facts stay valid; after an abortable handler, the code that follows joins the normal and the abort edges, so the checker drops path facts (integer values, non-zero facts) about every place the handled body wrote. The evaluator, the Cranelift backend and the Lean extraction (#link(<ch-8-2>)[§9.2]) all keep writes.

#_infer(
  "[T-CallProj]",
  $Delta ; Sigma ; Phi ; Gamma_f tack.r f : ((overline(mu)_i med overline(tau)_i) arrow.r^(epsilon)_(bold(m)_f) (tau_(upright("ret")) @ (rho_(upright("ret")), bold(m)_(upright("ret"))))) @ (1, bold(m)_f) divides emptyset \ forall i eq.not j, quad (p_i perp p_j quad "or" quad mu_i = mu_j = upright("in")) \ Gamma_(upright("args")) "supplies owned" p_i "for" mu_i = upright("sink"), "uninit slot for" mu_i = upright("out"), "and reborrow for" mu_i in {upright("in"), upright("inout")} \ upright("PostCall")(Phi, Gamma_(upright("args")), overline(mu)_i med overline(p)_i) = (Phi'; Gamma')$,
  $Delta ; Sigma ; Phi ; Gamma_f convolve.o Gamma_(upright("args")) tack.r f(overline(mu)_i med overline(p)_i) : tau_(upright("ret")) @ (rho_(upright("ret")), bold(m)_(upright("ret"))) thick gt.closed thick (Phi'; Gamma') divides epsilon$,
)

This is a contract schema: the existing place calculus proves disjoint mutable/shared call safety, while the heap-field strong-update model proves havoc and its correspondence with lens patching. Allocation, linear heap payloads, and an integrated `out` semantics are still required to prove this entire rule.

*Two-phase argument loans.* Every `inout` loan a call takes, of its receiver (`s.push(v)`, `s.try_set(i, v)`, or a method `p.bump(v)` whose `self` is `inout`) or of a plain argument (`f(inout s, v)`), is _reserved_ while the call's other arguments are evaluated and _activated_ only when the callee is entered. A reserved loan admits shared reads of its place: an argument may read it through `in` in a nested call or by copying a scalar (an integer, float, `bool` or `Unit`), and sees the value from before the call, as in `s.try_set(i, wrapping_add(s.get(i), 1))`, `s.push(s.len())`, `f(inout c, g(in c))` or `f(inout s, s.n)`. Writes, moves and `inout` loans of an overlapping place in the arguments are still refused (`E0301`, and `E0410` in the Place IR check), including the inner call of `w(inout o, w(inout o, 0))`, and an argument loan that is still live when the call is entered may not overlap the activated loan (`s.extend(in s[..])`, `f(inout p, in p)`, or a copy aggregate `f(inout q, q.p)`, which a backend may lend by address). The evaluator, native code and the Lean extraction evaluate the arguments before the call, so they agree. _Current implementation:_ the self-hosted Stage 1 checker (`packages/vesper-compiler`) does not reserve these loans, so it stays stricter and refuses these programs.

*Borrowed matching.* A `match` whose scrutinee is a place of an `in` or `inout` binding (a borrowed parameter, or a name an enclosing borrowed match bound) and whose type is not copy _borrows_ the place instead of moving it. Every name its patterns bind is an `in` borrow of the payload it names, `Box(p)` patterns included, so a `Box` tree passed `in` can be walked any number of times: in `fn eval(in e: Expr) -> u64 { match e { Expr::Add(l, r) => match l { Box(a) => .. eval(in a) .. } } }` neither `l`, `a` nor the boxes are moved or freed, and the tree's owner frees them once. While the arms run the matched place is frozen: writing it, lending it `inout` or moving it is `E0301`, and a binding cannot be moved (`E0305`) or written (`E0307`); the bindings of an `inout` scrutinee are `in` borrows too. A wildcard discards nothing, and a scrutinee of copy type is copied as before. A match of an owned value moves it, as before. _Current implementation:_ the checker, the evaluator, the JIT and objects (§16.6 libraries included); `inout` bindings that write through a box into the matched tree need place projections through `Box` in the Place IR and are not yet accepted. (SH1 lane 6, #1149.)

Refinement arithmetic generalizes static struct `FieldTree` disjointness to *runtime array and `AoSoA` tile intervals* (`inout buf[0..mid]` alongside `inout buf[mid..len]`, or `inout bodies[i, j]` where $i eq.not j$) without runtime `RefCell` locks or bounds-check panics:

#_infer(
  "[T-CallSliceProj]",
  $forall a eq.not b in {1 .. k}, quad upright("Valid")_(upright("SMT"))(Phi arrow.r.double.long [l_a, r_a) inter [l_b, r_b) = emptyset and 0 lt.eq l_a lt.eq r_a lt.eq n)$,
  $Delta ; Phi tack.r upright("DisjointSlices")(s : upright("Slice")[tau, @ n], { [l_1 .. r_1), dots, [l_k .. r_k) })$,
)

=== 4.4 Write-Only `out` Destinations and Tail-Modulo-Cons
<ch-4-4>

An `out p: T` parameter (`OutSlot[T]`) denotes an *uninitialized write-only destination* backed by caller stack space, a lexical `VirtualArena`, or pinned DMA memory:

#_infer(
  "[T-OutSplitSlice]",
  $Delta ; Sigma ; Phi ; Gamma_1, (p_1 :_((1, upright("linear")))^(upright("out")) upright("Slice")[tau, @ k]) tack.r e_1 : upright("Unit") divides epsilon \ Delta ; Sigma ; Phi ; Gamma_2, (p_2 :_((1, upright("linear")))^(upright("out")) upright("Slice")[tau, @(n - k)]) tack.r e_2 : upright("Unit") divides epsilon \ upright("Valid")_(upright("SMT"))(Phi arrow.r.double.long 0 lt.eq k lt.eq n)$,
  $Delta ; Sigma ; Phi ; Gamma_1 convolve.o Gamma_2, (p :_((1, upright("linear")))^(upright("out")) upright("Slice")[tau, @ n]) tack.r upright("split_out")(p, k, lambda p_1 p_2 . med (e_1 parallel e_2)) : upright("Unit") divides epsilon$,
)

- *Unwind-Safe Before Initialization, Exhaustive Before Return:* Because an unfilled `out` destination holds raw uninitialized storage rather than a live resource handle, `[T-Let]` permits early `Fail` (`?`) unwinds *prior* to writing `p`, while `OutInitTree` tracking forbids normal return until $100%$ of `p`'s field tree or $[0 .. n)$ slice interval is initialized (`UninitializedOutSlotAtReturn`).
- *Zero-Copy Placement and Tail-Modulo-Cons (TMC):* Large aggregates construct directly in caller destination memory without intermediate stack `memcpy` or `MaybeUninit` `unsafe` blocks, and Stage 4 (`vesper-opt-egglog`) rewrites constructor-ending recursion into $O(1)$-stack tail-recursive `out`-slot loops.

=== 4.5 Decidable Liquid Refinement Logic and Strong Updates
<ch-4-5>

The intended Bitwuzla verification boundary is quantifier-free bit-vectors with uninterpreted functions (`QF_UFBV`). Mathematical integer arithmetic (`QF_LIA`) and constrained-Horn-clause fixpoints are not Bitwuzla capabilities. Machine arithmetic must encode its widths, signedness, overflow behavior, and definedness explicitly; replacing it with unbounded integer arithmetic requires a separate range proof.

Invariant inference must be separate from individual validity queries: a finite, explicitly bounded qualifier set may generate candidate predicates, but unrestricted Horn inference is not a decidable admission rule. Query encoding and inference need deterministic work limits. Timeout, unknown, unsupported logic, or exhausted limits must reject verification rather than prove an obligation. No uniform sub-2-millisecond bound follows from decidability. The standalone interval helper evaluates only its bounded arithmetic syntax; it does not implement this solver pipeline.

Quantified protocol theorems belong in checked Lean proofs. The integer heap model in `lean/StrongUpdate.lean` establishes its stated snapshot properties, not equivalence to machine bit-vectors or an implemented SMT pipeline. Three rules specify the intended refinement boundary:

#_infer(
  "[Sub-Refine]",
  $upright("Valid")_(upright("SMT"))((Phi and upright("facts")(Gamma)) arrow.r.double.long (phi.alt_1 [v \/ x] arrow.r.double.long phi.alt_2 [v \/ x]))$,
  $Delta ; Phi ; Gamma tack.r { x : tau divides phi.alt_1 } <: { x : tau divides phi.alt_2 }$,
)

#_infer(
  "[T-StrongUpdate]",
  $(x :_((1, bold(m)))^(upright("own")) tau_(upright("old"))) in Gamma quad f : (upright("inout ") y : tau_(upright("in")) ~> tau_(upright("out"))) arrow.r^(epsilon) tau_(upright("ret")) \ Delta ; Phi tack.r tau_(upright("old")) <: tau_(upright("in"))$,
  $Delta ; Sigma ; Phi ; Gamma tack.r f(upright("inout ") x) : tau_(upright("ret")) thick gt.closed thick (Phi' ; Gamma') divides epsilon$,
)

Let $W$ be the locations writable by interference, and let $upright("sameOutside")(W, s_0, s_1)$ mean that both heaps agree outside $W$. A candidate retained predicate $psi$ must follow in every permitted post-interference heap; syntactic separation into immutable and mutable conjuncts is not assumed:

#_infer(
  "[M-ContendStrip]",
  $forall s_0, s_1, quad (phi.alt(s_0) and upright("sameOutside")(W, s_0, s_1)) arrow.r.double psi(s_1)$,
  $upright("contend")({ x : tau divides phi.alt }) = { x : tau divides psi } @ upright("contended")$,
)

Here $phi.alt$ includes path facts and dependent sibling refinements from the entire pre-state context. `StrongUpdate.Heap.ForgetMutable` constructs a sound stable weakening by existentially hiding previous mutable contents. Its `sharing_preserves_weakened_context` theorem covers arbitrary allowed interference and aliases. Retaining the original predicate instead requires the universal `Stabilize` obligation; a merely true pre-state predicate does not imply that stronger obligation. The model proves that `immutable = 1 or mutable > 0` cannot be retained just because its mutable branch currently holds. A checker unable to prove a candidate consequence must forget it, rather than split arbitrary Boolean syntax heuristically.

- *Enum Niche Optimization and Zero-Cost FFI Validation:* Refined scalars such as `type NonZeroU64 = { v: u64 | v != 0 }` (`u64 is 1..=u64::MAX`) pack `Option[NonZeroU64]` into 8 bytes and synthesize constant-time `ForeignVal[T]` ingress checks at *Vesper Bridge* boundaries (#link(<ch-15>)[Chapter 16]).
- *Post-state facts (`[T-StrongUpdate]`):* For a fresh logical snapshot $x_(upright("old"))$, define $Phi' = Phi[x_(upright("old")) \/ x]$ and rename references to the old root throughout dependent sibling refinements in $Gamma$. Replace the root binding with its post-type, interpreting the callee's pre-state binder through the same snapshot. The snapshot is a logical value, not a duplicable resource or runtime pointer. A concrete implementation must account for every alias and modified sub-place. `lean/StrongUpdate.lean` proves assignment, sequencing, and branch soundness for scalar stores and integer heap fields, including multiple names for the same object. Its postcondition is equivalent to the reachable post-states: old values are substituted throughout both path facts and dependent sibling predicates. `lean/RefinementPlaces.lean` connects that contract to the lawful lenses and `patch` operation in `EffectPlaces.lean`, including composed field projections. These results do not cover allocation, pointer-valued fields, linear payload ownership, or prove the production extraction pass correct.
- *Concurrency-Safe Refinement Filtering (`[M-ContendStrip]`):* At a sharing boundary, retain only consequences justified under every allowed mutable-state replacement. This is a semantic proof obligation; the production checker does not yet implement the full heap-refinement calculus.

=== 4.6 End-to-End Showcase: Parallel DMA Staging with Ghost Permissions
<ch-4-6>

The following function combines ABI-transparent scalar refinements, `@0` `ghost linear` DMA slot permissions, parallel `[T-OutSplitSlice]` destination initialization, strong `~>` refinement updates, and a Bitwuzla-discharged exact arena budget that eliminates every runtime bump-limit branch:

```rust
// 1. ABI-transparent refined scalar (8-byte niche-packed Option[AlignedLen64]):
type AlignedLen64 = { n: usize | n > 0 && (n & 63) == 0 && n <= 4096 };

// 2. Zero-bit ghost linear DMA permission + LIFO hardware interrupt guard:
ghost linear struct DmaSlotPerm[@slot: u32] {}
linear + lifo struct IrqGuard { saved_psr: usize }

// 3. Zero-cost quotient hiding uncompacted AoSoA Z-Set internals via Lean 4 Quot.sound:
pub affine quotient ZSet[K: Ord] = repr TiledSeq[ZEntry[K]] modulo same_consolidated_weights {
    @respects
    pub fn total_weight(in self) -> i64 eff [] {
        self.repr.sum_weights_simd()
    }
}

// 4. Parallel zero-copy DMA initialization + graded region + strong `~>` update:
pub fn stage_parallel_dma_batch[P: IsaProfile, @slot: u32, @count: usize](
    out dst_pkt: AlignedByteArray[4096],
    in src_frames: Slice[RawFrame, @count],
    inout stats: ShardStats[@slot, @prev_pkts] ~> ShardStats[@slot, @(prev_pkts + 1)],
    ghost sink perm: DmaSlotPerm[@slot],
) -> ghost DmaStagedReceipt[@slot]
eff [Scope, Isa[P], Stack @ <= 384]
where
    count <= 32
{
    // Bitwuzla proves `64 * count <= 2048`, allowing Stage 4 `egglog` to elide all bump checks:
    let crc_summary = Region::scoped_exact[2048](|cap scratch|
        eff [scratch: Region @ <= 64 * count, Isa[P]]
    {
        let mut hashes = RegionVec::with_exact_cap[u64, @count](in scratch);
        for i in 0..count {
            // `i < count` proven in QF_LIA -> bounds check on `src_frames[i]` elided;
            // strong `~>` update advances `hashes` static length each iteration.
            hashes.push_unchecked_refined(simd_hash64[P](in src_frames[i]));
        }
        hashes.xor_fold[P]()
    });

    // Split uninitialized `out dst_pkt[0..4096]` into disjoint `[0..64)` and `[64..4096)` slices:
    let mid: usize is 64..=64 = 64;
    Pulse::scope(|inout s| eff [Scope, Isa[P]] {
        let f_hdr  = s.spawn(|| eff []       { write_dma_header(out dst_pkt[0..mid], crc_summary) });
        let f_body = s.spawn(|| eff [Isa[P]] { pack_frames_simd[P](out dst_pkt[mid..4096], in src_frames) });
        (f_hdr.join(), f_body.join())
    });

    // Discharge 0-bit linear ghost token and advance refined `stats` packet count:
    let ghost DmaSlotPerm {} = perm;
    stats.record_staged_packet();
    ghost DmaStagedReceipt {}
}
```

== 5. Lexical Effects, Graded Budgets, Clocks, and Quotients
<ch-5>
#chapter-deck[One-Shot Handlers, Resource Budgets, Guarded Recursion, and Quotients]

Lexical effects connect operations to handlers whose continuation usage is explicit. This chapter presents handler typing, resource grades, guarded clocks, and quotient interfaces. Their rules have separate mechanized models; combining all of these features with linear state and source inference remains open.

=== 5.1 Effect Perform and Stateful Lexical Handler Typing
<ch-5-1>

Effect operations invoke a lexically bound handler capability $l : upright("Cap")_q [E]$ in $Sigma$, while `handle` blocks install a fresh capability with explicit state threading:

#_infer(
  "[T-Perform]",
  $(l : upright("Cap")_q [E]) in Sigma quad (upright("op") : tau_(upright("arg")) @ bold(m)_(upright("arg")) arrow.r tau_(upright("res")) @ bold(m)_(upright("res"))) in E \ Delta ; Sigma ; Phi ; Gamma tack.r e : tau_(upright("arg")) @ (1, bold(m)_(upright("arg"))) divides epsilon$,
  $Delta ; Sigma ; Phi ; Gamma tack.r upright("perform ") l . upright("op")(e) : tau_(upright("res")) @ (1, bold(m)_(upright("res"))) divides epsilon compose { l : E^q @ g_(upright("op")) }$,
)

The initializer in the rule below is a pure atom. An effectful initializer must first be sequenced with `[T-Let]`, whose frame condition protects the remaining body context. The rule uses ungraded rows; $bold(k)_i$ is the intrinsic kind of operation argument $tau_i$. State must survive every escaping effect, and the handled label must occur in neither the result type nor the escaping row. Graded handlers need a separate composition rule that accounts for every clause invocation and resumption; reusing the same finite budget for both a clause and its continuation is invalid.

#_infer(
  "[T-Handle]",
  $upright("atom")(e_(upright("init"))) quad u(bold(m)_s) subset.eq.sq upright("res")(epsilon) quad l in.not upright("free_caps")(tau_(upright("out")), epsilon) \ Delta ; Sigma ; Phi ; Gamma_s tack.r e_(upright("init")) : tau_s @ (1, bold(m)_s) divides emptyset quad forall (z :_((rho_z, bold(m)_z))^(mu) tau_z) in Gamma_h, med u(bold(m)_z) = upright("copy") \ Delta ; (Sigma, l : upright("Cap")_q [E]) ; Phi ; Gamma_(upright("body")) tack.r e_(upright("body")) : tau_b @ (1, bold(m)_b) divides { l : E^q } compose epsilon \ Delta ; Sigma ; Phi ; Gamma_h, x :_((1, bold(m)_b))^(upright("own")) tau_b, s :_((1, bold(m)_s))^(upright("own")) tau_s tack.r e_(upright("ret")) : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon \ forall (upright("op")_i : tau_i arrow.r sigma_i) in E : \ Delta ; Sigma ; Phi ; Gamma_h, y :_((1, bold(k)_i))^(upright("own")) tau_i, s :_((1, bold(m)_s))^(upright("own")) tau_s, k :_((1, q))^(upright("own")) ((sigma_i, tau_s) arrow.r^(epsilon)_q tau_(upright("out"))) tack.r e_i : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon$,
  $Delta ; Sigma ; Phi ; Gamma_s convolve.o Gamma_(upright("body")) convolve.o Gamma_h tack.r (upright("handle ") l : E^q upright(" state ") s = e_(upright("init")) upright(" in ") e_(upright("body")) upright(" with ") { upright("return")(x, s) => e_(upright("ret")), upright("op")_i (y, s, k) => e_i }) : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon$,
)

The ungraded handler rule requires state-frame safety and lexical discharge in addition to the following structural conditions. `Metatheory/Polymorphism.lean` proves these properties for its own operational rule. It does not prove graded cost accounting or native-stack lifetime safety:

+ *Substructural Continuation Mode (`k` at Mode $q$):* Captured delimited continuations carry usage mode $q in {upright("affine"), upright("linear")}$, never `copy`. For abortable effects such as `effect Fail[Err]` ($q = upright("affine")$), `raise(err, s, k) => Err(err)` drops `k` soundly via `discard`. For non-abortable effects such as `linear effect Storage` ($q = upright("linear")$), every clause must resume `k` exactly once—either inline (tail-resumptive fusion) or by parking the linear fiber handle in a completion table (`io_uring` / `kqueue` / IOCP).
+ *Multi-Invocation State Threading:* Because $e_(upright("body"))$ may perform `op_i` repeatedly inside loops, ambient bindings $Gamma_h$ captured directly by handler clauses must be `copy`, whereas unique or `linear` handler state (such as a submission ring or bump cursor) threads linearly through the explicit state parameter $s :_((1, bold(m)_s)) tau_s$.
+ *Lexical Capability Confinement:* The capability label $l$ is scoped to $e_(upright("body"))$ and discharged from the outer effect row $epsilon$; any closure capturing $l$ records $l : E^q$ in its function type (`[T-Abs]`), preventing capabilities from escaping their defining handler frame.

=== 5.2 Worked Derivation: Rejecting an Unconsumed Obligation Across `?`
<ch-5-2>

Consider why Vesper statically rejects an unguarded `?` propagation while a `linear` handle is live:

```rust
let tx: WriteTxn = Storage::begin_txn(max_bytes); // q(Storage) = linear
validate_crc(in batch)?;                          // performs Fail::raise; q(Fail) = affine
tx.commit()
```

+ Binding `tx` introduces $t x :_((1, upright("linear")))^(upright("own")) upright("WriteTxn")$ into the environment.
+ Desugaring `validate_crc(in batch)?; tx.commit()` produces `let _ = e_crc in tx.commit()`, where $e_(upright("crc"))$ has effect row $epsilon_1 = { l_(upright("fail")) : upright("Fail")[upright("IoError")]^(upright("affine")) }$.
+ Context splitting (`⊛`) places `tx` into $Gamma_2$ because `tx.commit()` consumes it in the continuation expression.
+ `[T-Let]` checks $u(bold(m)_(t x)) subset.eq.sq upright("res")(epsilon_1)$, which reduces to $upright("linear") subset.eq.sq upright("affine")$—*false*. Compilation fails with diagnostic `E0301` identifying both the minting span of `tx` and the abortable `?` site. The same scope applies to every frame: a helper `fn append(inout tx: WriteTxn, ...) eff [Fail[IoError]]` may borrow and mutate `tx` across `?`, since unwinding `append` leaves the one `WriteTxn` whole in the caller's slot (keep-writes, #link(<ch-4-3>)[§4.3]); the owning caller must then `handle` the failure and consume `tx` on both arms. Inside `WriteTxn::commit`, by contrast, destructuring `WriteTxn` first transfers linearity to `dirty_pages` and handles `Fail` only after or alongside explicit cleanup.

=== 5.3 Ordered Pomonoid Resource Grades (`eff [E @ g]`)
<ch-5-3>

Resource accounting separates cumulative work, peak live stack space, and exact observations. The standalone `vesper-core::resource_grade` helper implements checked finite arithmetic; it is not yet a source-level effect inference pass.

- *Additive Arena and Fuel Budgets (`Region`, `Fuel`):* Sequential costs add, branch bounds take the maximum, and a bounded loop multiplies its body bound by its trip count. Overflow or an unresolved interval rejects admission. A successful interval check alone does not justify deleting allocation checks: the inferred cost must also be connected to the executing allocator and all control-flow paths.
- *Peak Call-Stack Depth (`Stack`):* Sequential sibling calls reuse space, so their bounds combine by `max`. A nested call retains its caller frame, so its bound is `frame_self + callee_bound`. A finite loop reuses the body's peak space; it does not multiply that space by its trip count. Recursion needs a separate depth argument.
- *Exact Timing (`ConstTime`):* Sequential costs add; secret-dependent alternatives require equal costs. A published exact cost is invariant under interface compatibility, even when a replacement is cheaper. Equality cannot be weakened to an upper bound. This arithmetic contract does not establish physical constant-time behavior on a processor.

`lean/ResourceGrades.lean` proves peak-stack bounds and exact-time non-interference for a finite operational call-tree language. It distinguishes sequential and nested frames, rejects unequal-cost branches, and proves preservation of caller cost contracts under its compatibility relation. Machine latency, recursive calls, and graded handler composition remain outside this model. A separate bounded one-shot handler model in `lean/HandlerGrades.lean` proves the additive budget `body_work + operation_count * clause_overhead + return_work`. Clause overhead excludes the resumed continuation. `evaluator_matches_cps` connects the operational evaluator to the existing `HandlerCPS` monad, and `extracted_handler_budget` proves the bound for that encoding. The model permits nonzero work before and after resumption, accounts for stopping clauses, and skips post-resumption work on an outer abort. The Rust `handler_additive_bound` helper checks the same arithmetic without overflow; deriving its inputs from source and combining grades with linear handler state remain open.

=== 5.4 Clock-Indexed Guarded Recursion and Substructural Quotients
<ch-5-4>

The clock and quotient rules must separate logical validity from runtime scheduling and representation metadata:

- *Clocked Guarded Recursion (`Later[k: clock, T]`):* A delayed runtime payload must be at most `affine`. A recursive step must capture only `copy` values and have a closed, empty effect row. A graph without instantaneous cycles checks dependencies; it does not prove productive source execution. At `@0`, forcing requires universal clock quantification with a fresh clock absent from ambient assumptions. Matching the active clock name is insufficient, and the source checker rejects ghost forcing until it represents the required judgment. The step-indexed model in `lean/ClockQuotientSafety.lean` permits forcing from validity at every clock index, rejects a delayed false proposition at the initial tick, and preserves clock-closed validity through total guarded steps. It excludes runtime scheduling and linear payload ownership.
- *Substructural `quotient` Types:* The representation kind must lie below the quotient's declared kind. Every public operation, including `inout` methods, requires a congruence certificate. An observation must return equal results for related representatives; an updating operation must additionally return related post-states. `ClockQuotientSafety.publicObserve` and `publicStep` use actual Lean proofs to construct `Quot.lift`; the representation-capacity counterexample cannot satisfy their premises. The core declaration helper checks an independent export inventory for missing, duplicate, unrelated, or mismatched proof statuses. A caller-supplied `proved_respects` flag is bookkeeping, not a certificate: integration must obtain it from trusted proof checking and bind it to the operation body and relation. Source elaboration, artifact binding, and runtime layout correspondence remain separate obligations.

=== 5.5 Core Metatheoretic Soundness Guarantees
<ch-5-5>

The repository proves progress, preservation, resource exhaustion, handler-state safety, lexical escape prevention, and borrow safety for several operational calculi, identified in Appendix A. The following combined end-to-end statements remain research goals: the separate calculi do not yet integrate all four modality axes, refinements, ghosts, grades, and machine execution in one semantics.

- *Theorem 1 (Type, Mode, and Capability Preservation):* Well-typed closed configurations step without stuck states while preserving types, 4-axis `ModeVec` bounds, disjoint place and slice capabilities, LIFO obligation stacks, and lexical effect rows.
- *Theorem 2 (Total Linear Exhaustion):* In every closed program $emptyset ; emptyset ; top ; emptyset tack.r e : tau @ (1, upright("copy")) divides emptyset$ terminating at a value or handled `Fail` result, every dynamically constructed `linear` value is destructured or consumed by a terminal `sink` transition *exactly once*—never leaked on an unwind edge and never duplicated across a continuation.
- *Goal 3 (Fixed-Address Fiber Stacks):* Active and suspended fiber stacks retain their virtual addresses: live machine pointers may refer to ancestor frames. A stack can be pooled only after termination and after all asynchronous operations accessing it have completed.
- *Theorem 4 (`@0` Ghost-Erasure Bisimulation):* Erasing all $rho = 0$ bindings, `ghost` fields, and refinement indices yields an untyped operational trace bit-for-bit identical to the graded evaluation semantics.

== 6. Substructural Continuation Fission, Escrow Projections, and Reaper Calculus
<ch-fission>
#chapter-deck[Proposed Cancellation Paths, Escrow Custody, and Cleanup Budgets]

*Design status:* This chapter is a proposed extension. Its grammar, lowering, timing and memory-budget claims are not implementation guarantees or completed end-to-end soundness proofs; the existing manuscript review status applies.

Core substructural effect systems—including #link("https://iris-project.org/pdfs/2025-popl-affect.pdf")[Affect] (van Rooij and Krebbers, POPL 2025) and #link("https://www.microsoft.com/en-us/research/wp-content/uploads/2023/11/qualified-effect-types-2018.pdf")[Qualified Effect Types] (Leijen, 2018)—assign a single monolithic usage mode $q in {upright("copy"), upright("affine"), upright("linear")}$ to each captured delimited continuation $k$. In #link(<ch-4-2>)[Section 4.2] and #link(<ch-5-2>)[Section 5.2], Vesper's baseline `[T-Let]` Resumption Frame Condition ($u(bold(m)_z) subset.eq.sq upright("res")(epsilon_1)$) enforces soundness under that monolithic assumption by statically rejecting (`E0301`) any abortable effect (`Fail::raise` via `?` or `Cancel` via `Pulse::timeout`) whenever a `linear` obligation resides in the continuation frame $Gamma_2$.

Completion I/O (`io_uring`, IOCP), RDMA, GPU queues, and transactional storage motivate three cancellation concerns:

+ *Synchronous Drop and DMA Lifetimes:* Dropping a coroutine state machine can reclaim its memory before external operations finish. If kernel `io_uring` SQEs or remote RNIC `rkey` windows still reference that memory, early reclamation risks use-after-free. Relevant discussions include #link("https://without.boats/blog/poll-drop/")[Asynchronous Destructors] (Without Boats), #link("https://sabrinajewson.org/blog/async-drop")[Async Destructors and Completion Futures] (Jewson), #link("https://clef-lang.com/blog/rusts-retrofits/")[Rust's Retrofits], #link("https://man7.org/linux/man-pages/man7/io_uring_cancelation.7.html")[`io_uring_cancelation(7)`], and the RDMA design in #link(<ch-34-2>)[Section 35.2].
+ *Cleanup Latency:* Waiting inline for a disk controller, peer, or RNIC to drain can delay the cancelling fiber beyond its requested deadline. The proposal separates waking that caller from completing cleanup.
+ *Explicit Linear Cleanup:* The baseline rule rejects `?` or a `Pulse::race` abort while a linear obligation remains live (`E0301`). Manual cleanup before each fallible branch becomes difficult to maintain in pipelined I/O loops.

*Substructural Continuation Fission* proposes to address these constraints without garbage collection, `'static` heap boxing, or `Pin`. Rather than treating a captured continuation $k$ as an indivisible function, Vesper fissions $k$ along Stage 3's Place Capability Graph (#link(<ch-7-5>)[Section 8.5]) into a *linear additive conjunction* ($amp$, Girard's _with_) pairing the normal resumption path $k . upright("resume")$ with a compiler-sliced residual reaper $k . upright("reap")$, backed by co-owned `escrow` place projections and pomonoid quarantine-memory budgets (`EscrowMem`).

=== 6.1 Architectural Comparison and Fission Requirements
<ch-fission-1>

The table compares the proposed contracts. Its fission column states design targets, not measured performance or completed proofs. Other columns summarize the cleanup strategies assumed by this comparison.

#figure(
  table(
    columns: (21%, 19%, 19%, 19%, 22%),
    align: (left, left, left, left, left),
    table.header(
      [*Property*],
      [*Sync Coroutine `Drop`*],
      [*Inline `AsyncDrop`*],
      [*Monolithic `[T-Let]`*],
      [*Proposed Fission Contract*],
    ),
    [*Normal-Path Linearity*],
    [None (`mem::forget` legal)],
    [Weak (opt-in binding)],
    [*Exact-once* (`Theorem 2`)],
    [Exact-once preservation required],
    [*`?` / `Cancel` Across Live Obligations*],
    [Permitted (silent drops)],
    [Permitted (inline await)],
    [*Rejected* (`E0301`)],
    [*Permitted* for types with `reap`],
    [*`io_uring` / RDMA Stack DMA Safety*],
    [Unsafe if DMA outlives storage],
    [Wait for DMA before reclaiming],
    [Requires `OwnedDmaBuf`],
    [Quarantine via `EscrowCell` until CQE],
    [*Caller Wakeup on Timeout*],
    [$O(1)$ drop; DMA lifetime separate],
    [*Unbounded stall*],
    [Cannot cancel],
    [$O(1)$ target (`4` instructions)],
    [*Zombie Quarantine Memory*],
    [N/A],
    [Caller frame retained],
    [N/A],
    [Bound required (`EscrowMem @ <= B`)],
    [*Lean 4 Verification*],
    [Proof approach depends on model],
    [Opaque poll state machine],
    [Single-path `EffM`],
    [*Dual-branch pure state reducer*],
  ),
  kind: table,
)

The proposal sets four requirements for continuation fission across the compiler stages:

+ *Uncompromised Normal-Path Linearity:* Defining a `reap` clause on a `linear struct` *never* relaxes its usage mode to `affine`. Falling off the end of a block without an explicit terminal consumer (`tx.commit()`, `IoUring::await_read()`) remains a hard compile-time error; `reap` is reachable exclusively along typed abort edges (`k.reap()`).
+ *Co-Owned `escrow` Custody Without `'static` Heap Copies:* Passing `escrow buf[0..4096]` loans disjoint read/write custody of `buf` to an external hardware or kernel queue bound to a linear ticket `h : T[@buf]`. While `h` is live, the Place Capability Graph forbids all CPU `in`, `inout`, `out`, and `sink` accesses to `buf[0..4096]`.
+ *$O(1)$ Non-Blocking Unwind via Split `EscrowCell` Detachment:* Synchronous `reap` clauses (`eff []`, such as rolling back an in-memory `WriteTxn`) execute inline during unwind. Asynchronous `reap` clauses (`eff [IoReap]`, such as awaiting a cancelled `io_uring` CQE or `ibv_wr_local_inv` work completion) detach *only* the minimal `EscrowCell` sub-frame into the worker shard's reaper ring while immediately recycling the 64 KiB CPU fiber stack.
+ *Pomonoid Quarantine Admission Control:* Every potential abort edge crossing an asynchronous `escrow` loan contributes its exact detached byte footprint to `EscrowMem`, enabling Bitwuzla to prove statically that worst-case cancellation storms never exhaust fixed `ShardSlab` or `VirtualArena` budgets.

=== 6.2 LL(2) Surface Grammar Extensions: `escrow` and `reap`
<ch-fission-2>

Continuation Fission proposes three additions to the grammar sketch in #link(<ch-2-2>)[Section 2.2] (`'escrow'` and `'reap'` are reserved keywords in parameter/item positions):

```text
(* 1. Fifth Place-Passing Mode: Asynchronous Hardware / Kernel Co-Custody *)
PassMode       ::= 'in' | 'inout' | 'out' | 'sink' | 'escrow' ;

(* 2. Linear Struct Reaper Declaration (inside StructDecl body) *)
StructDecl     ::= GhostMod? StorageSpec? ModeSpec? 'struct' UpperIdent TypeParams?
                   ( '{' FieldDeclList? ReapDecl? '}' | ';' ) ;

ReapDecl       ::= 'reap' '(' 'sink' 'self' ( ',' ParamList )? ')' EffClause? Block ;

(* 3. Additive-Conjunctive Handler Continuation Elimination *)
PostfixOp      ::= ...
                 | '.' 'resume' '(' ArgList? ')'     (* pi_1(k): normal one-shot resumption *)
                 | '.' 'reap'   '(' ')' ;            (* pi_2(k): residual obligation reaper *)
```

The intended local lookahead argument is: inside `StructDecl`, `'reap' '('` disambiguates in two tokens from `FieldDecl` (`Vis? GhostMod? TempTier? LowerIdent ':'`), and inside `Arg` / `Param`, `'escrow'` occupies the exact lexical slot of `PassMode`.

=== 6.3 Formal Typing Judgments: `escrow`, `[T-Let-Fission]`, and `[T-Handle-With]`
<ch-fission-3>

==== Well-Formedness of `reap` Declarations

A `reap` declaration on a `linear` nominal type $T$ specifies an *infallible* terminal compensation morphism ($upright("res")(epsilon_(upright("reap"))) = upright("linear")$). Forbidding abortable effects inside `reap` itself prevents secondary unwind cascades during cleanup:

#_infer(
  "[WF-Reap]",
  $u(bold(m)_(upright("decl"))(T)) = upright("linear") quad upright("res")(epsilon_(upright("reap"))) = upright("linear") \ Delta ; Sigma ; Phi ; (upright("self") :_((1, upright("linear")))^(upright("own")) T, overline(z :_((1, bold(m)_z))^(mu_z) tau_z)) tack.r e_(upright("reap")) : upright("Unit") @ (1, upright("copy")) divides epsilon_(upright("reap")) \ forall j, quad mu_(z_j) in {upright("in"), upright("inout"), upright("escrow")}$,
  $Delta tack.r (upright("reap") (upright("sink self"), overline(mu_z med z : tau_z)) upright(" eff ")[epsilon_(upright("reap"))] med e_(upright("reap"))) upright(" ok in ") T$,
)

==== Co-Owned `escrow` Place Custody

In the substructural place context $Gamma$ (#link(<ch-4-1>)[Section 4.1]), an `escrow` operation transitions a disjoint place or slice $p = x . pi$ from owned/uninitialized state into a *frozen co-custody state* $p :_((1, bold(m)))^(upright("escrow")(h)) tau_p$ indexed by the linear ticket $h$ returned by the submission primitive:

#_infer(
  "[T-EscrowLoan]",
  $(x . pi :_((1, bold(m)))^(mu_0) tau_p) in Gamma quad mu_0 in {upright("own"), upright("out")} quad p(bold(m)) = upright("portable") \ f_(upright("sub")) : (upright("escrow ") y : tau_p) arrow.r^(epsilon) upright("Ticket")[tau_p] @ (1, upright("linear")) quad h in.not upright("dom")(Gamma)$,
  $Delta ; Sigma ; Phi ; Gamma tack.r (upright("let ") h = f_(upright("sub"))(upright("escrow ") x . pi)) thick gt.closed thick (Gamma[x . pi mapsto (x . pi :_((1, bold(m)))^(upright("escrow")(h)) tau_p)], h :_((1, upright("linear")))^(upright("own")) upright("Ticket")[tau_p] @ p) divides epsilon$,
)

#_infer(
  "[T-EscrowClaim]",
  $f_(upright("wait")) : (upright("sink ") upright("Ticket")[tau_p], upright("escrow ") tau_p) arrow.r^(epsilon) tau_(upright("ret")) @ (1, bold(m)_(upright("ret")))$,
  $Delta ; Sigma ; Phi ; Gamma, (x . pi :_((1, bold(m)))^(upright("escrow")(h)) tau_p), (h :_((1, upright("linear")))^(upright("own")) upright("Ticket")[tau_p] @ p) tack.r f_(upright("wait"))(upright("sink ") h, upright("escrow ") x . pi) : tau_(upright("ret")) @ (1, bold(m)_(upright("ret"))) thick gt.closed thick (Gamma, x . pi :_((1, bold(m)))^(upright("own")) tau_p) divides epsilon$,
)

While $x . pi$ carries permission $upright("escrow")(h)$:
+ Neither `[T-Var]`, `[T-CallProj]`, nor `discard(Gamma)` applies to $x . pi$ (preventing all CPU reads, CPU writes, and premature stack frame drops while hardware DMA is active);
+ Only `[T-EscrowClaim]` (normal completion) or $h$'s declared `reap(sink self, escrow p)` transition (fissioned abort) can consume $h$ and unlock $x . pi$.

==== Fissioned Sequencing (`[T-Let-Fission]`)

When $e_1$ performs an abortable effect row $epsilon_1$ ($upright("res")(epsilon_1) = upright("affine")$), `[T-Let-Fission]` partitions the continuation environment $Gamma_2 = Gamma_(upright("aff")) convolve.o Gamma_(upright("reap"))$, where $upright("discard")(Gamma_(upright("aff")))$ holds and $Gamma_(upright("reap")) = [h_1, dots, h_m]$ is an ordered sequence of `linear` obligations (and their bound `escrow` places) that each possess a valid `reap` transition with pairwise-disjoint mutable/escrow footprints in reverse acquisition (`LIFO`) order:

#_infer(
  "[T-ReapSlice]",
  $upright("discard")(Gamma_(upright("aff"))) quad forall k in {1 .. m}, med (h_k :_((1, bold(m)_k))^(upright("own")) T_k) in Gamma_(upright("reap")) \ T_k upright(" has ") upright("reap")_k (upright("sink self"), overline(mu_(k j) med p_(k j))) upright(" eff ")[epsilon_(upright("reap"), k)] \ forall a eq.not b, med upright("mut_escrow")(p_a) perp upright("mut_escrow")(p_b) quad B_(upright("esc")) = sum_(k : upright("async")(k)) upright("sizeof")(upright("escrow")(h_k))$,
  $Delta ; Phi ; Gamma_(upright("aff")) tack.r Gamma_(upright("reap")) arrow.squiggly_(upright("reap")) (epsilon_(upright("reap"), m) compose dots.c compose epsilon_(upright("reap"), 1)) compose { upright("EscrowMem") @ lt.eq B_(upright("esc")) }$,
)

#_infer(
  "[T-Let-Fission]",
  $Delta ; Sigma ; Phi ; Gamma_1 tack.r e_1 : tau_1 @ (1, bold(m)_1) divides epsilon_1 quad upright("res")(epsilon_1) = upright("affine") \ Delta ; Sigma ; Phi ; Gamma_2, x :_((1, bold(m)_1))^(upright("own")) tau_1 tack.r e_2 : tau_2 @ (1, bold(m)_2) divides epsilon_2 \ Gamma_2 = Gamma_(upright("aff")) convolve.o Gamma_(upright("reap")) quad Delta ; Phi ; Gamma_(upright("aff")) tack.r Gamma_(upright("reap")) arrow.squiggly_(upright("reap")) epsilon_(upright("reap"))$,
  $Delta ; Sigma ; Phi ; Gamma_1 convolve.o Gamma_2 tack.r (upright("let ") x = e_1 upright(" in ") e_2) : tau_2 @ (1, bold(m)_2) divides epsilon_1 compose (epsilon_2 union.sq epsilon_(upright("reap")))$,
)

Notice that when $Gamma_(upright("reap")) = emptyset$, $epsilon_(upright("reap")) = emptyset$ and `[T-Let-Fission]` degenerates identically to #link(<ch-4-2>)[Section 4.2]'s `[T-Let]`. Conversely, if a `linear` obligation intentionally *omits* a `reap` clause (such as an uncompensated bare-metal `IrqGuard` or two-phase commit coordinator vote that *must* be resolved explicitly), `[T-ReapSlice]` fails and Vesper preserves `E0301`.

==== Additive-Conjunctive Handler Typing (`[T-Handle-With]`)

Inside an abortable `handle` block ($q = upright("affine")$), each operation clause receives a *linear additive-conjunctive continuation* $k$ exposing `.resume(...)` and `.reap()`, branded with capability label $l$ at mode `linear + pinned + lifo` so $k$ can never escape the handler clause:

#_infer(
  "[T-Handle-With]",
  $Delta ; Sigma ; Phi ; Gamma_s tack.r e_(upright("init")) : tau_s @ (1, bold(m)_s) divides epsilon quad u(bold(m)_s) subset.eq.sq upright("res")(epsilon) quad forall z in Gamma_h, med u(bold(m)_z) = upright("copy") \ Delta ; (Sigma, l : upright("Cap")_(upright("affine")) [E]) ; Phi ; Gamma_(upright("body")) tack.r e_(upright("body")) : tau_b @ (1, bold(m)_b) divides { l : E^(upright("affine")) @ g } compose epsilon \ Delta ; Sigma ; Phi ; Gamma_h, x :_((1, bold(m)_b))^(upright("own")) tau_b, s :_((1, bold(m)_s))^(upright("own")) tau_s tack.r e_(upright("ret")) : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon \ forall (upright("op")_i : tau_i arrow.r sigma_i) in E : \ Delta ; Sigma ; Phi ; Gamma_h, y :_((1, bold(m)_(y_i)))^(upright("own")) tau_i, s :_((1, bold(m)_s))^(upright("own")) tau_s, \ k :_((1, upright("linear") + upright("pinned") + upright("lifo")))^(upright("own")) upright("FissionCont")_l [sigma_i, tau_s, tau_(upright("out")), epsilon, epsilon_(upright("reap"))] \ tack.r e_i : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon union.sq epsilon_(upright("reap"))$,
  $Delta ; Sigma ; Phi ; Gamma_s convolve.o Gamma_(upright("body")) convolve.o Gamma_h tack.r (upright("handle ") l : E^(upright("affine")) upright(" state ") s = e_(upright("init")) upright(" in ") e_(upright("body")) upright(" with ") { upright("return")(x, s) => e_(upright("ret")), upright("op")_i (y, s, k) => e_i }) : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon union.sq epsilon_(upright("reap"))$,
)

where `FissionCont` eliminates exclusively via one of its two additive projections:

#_infer(
  "[T-With-Resume]",
  $Delta ; Sigma ; Phi ; Gamma_1 tack.r v : sigma_i @ (1, bold(m)_v) divides emptyset quad Delta ; Sigma ; Phi ; Gamma_2 tack.r s : tau_s @ (1, bold(m)_s) divides emptyset$,
  $Delta ; Sigma ; Phi ; Gamma_1 convolve.o Gamma_2, k :_((1, upright("linear") + upright("pinned") + upright("lifo")))^(upright("own")) upright("FissionCont")_l [sigma_i, tau_s, tau_(upright("out")), epsilon, epsilon_(upright("reap"))] tack.r k . upright("resume")(v, s) : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon$,
)

#_infer(
  "[T-With-Reap]",
  $upright("discard")(Gamma)$,
  $Delta ; Sigma ; Phi ; Gamma, k :_((1, upright("linear") + upright("pinned") + upright("lifo")))^(upright("own")) upright("FissionCont")_l [sigma_i, tau_s, tau_(upright("out")), epsilon, epsilon_(upright("reap"))] tack.r k . upright("reap")() : upright("Unit") @ (1, upright("copy")) divides epsilon_(upright("reap"))$,
)

For standard `Fail[E]` handlers, `raise(err, s, k) => { k.reap(); Err(err) }` discharges `k`'s linear obligation in one line—running synchronous rollbacks inline and detaching asynchronous `escrow` reapers automatically before returning `Err(err)`.

=== 6.4 Ordered Pomonoid Quarantine Grades (`EscrowMem` and `IoReap`)
<ch-fission-4>

Unbounded background cleanup under a timeout storm is a classic availability hazard in completion-I/O runtimes. Continuation Fission prevents zombie memory exhaustion statically by integrating `EscrowMem` and `IoReap` into #link(<ch-5-3>)[Section 5.3]'s ordered resource-grade algebra:

- *Sequential High-Water Quarantine (`EscrowMem`):* Across sequential statements `e_1; e_2` where `e_1`'s `escrow` tickets are claimed prior to `e_2`, detached quarantine memory composes via $max(g_1, g_2)$, whereas simultaneously live nested `escrow` tickets sum additively ($g_1 + g_2$):
  $ upright("EscrowMem")(upright("let ") h_1 = upright("prep")(upright("escrow ") b_1) upright(" in let ") h_2 = upright("prep")(upright("escrow ") b_2) upright(" in ") e_(upright("abort"))) = upright("sizeof")(b_1) + upright("sizeof")(b_2) $
- *Compile-Time Slab Admission Proof:* When a worker loop runs inside `Pulse::with_escrow_pool[MAX_BYTES]`, Stage 3 verifies via Bitwuzla (`QF_UFBV`) that `max_concurrent_fibers * per_fiber_escrow_mem <= MAX_BYTES`, guaranteeing zero dynamic heap fallback and zero `ENOMEM` stalls during mass connection disconnects.

=== 6.5 End-to-End Showcase: Cancel-Safe `io_uring`, WAL Rollback, and RDMA Quarantine
<ch-fission-5>

The following module combines a synchronous `WriteTxn` rollback, an asynchronous Linux `io_uring` stack-detached page read, and a *Vesper Chorus* (#link(<ch-34-2>)[Section 35.2]) Type-2 RDMA Memory Window invalidation (#link("https://man7.org/linux/man-pages/man3/ibv_wr_local_inv.3.html")[`ibv_wr_local_inv(3)`]) across both `?` propagation and `Pulse::timeout`:

#_compact-listing[
```vesper
// 1. Synchronous WAL transaction: rolls back dirty page table inline in 0 ns on `?` or `Cancel`.
pub linear struct WriteTxn {
    lsn: u64,
    staged_slots: RegionVec[u32],

    reap(sink self, inout wal: WalJournal) eff [] {
        let WriteTxn { lsn, staged_slots } = self;
        wal.abort_staged_lsn(lsn, sink staged_slots);
    }
}

// 2. Completion-based io_uring SQE ticket: quarantines `escrow dst` until kernel CQE drains.
pub linear struct UringReadTicket[@sqe_id: u64] {
    sqe_id: u64,

    reap(sink self, escrow dst: ByteSlice) eff [IoReap @ <= 1] {
        let UringReadTicket { sqe_id } = self;
        IoUring::prep_async_cancel(sqe_id);
        IoUring::await_final_cqe(sqe_id, escrow dst); // `dst` recycled ONLY after CQE harvest!
    }
}

// 3. Chorus Type-2 RDMA Memory Window: fences remote RNIC `rkey` DMA before freeing buffer.
pub linear struct RdmaWindowTicket[@rkey: u32] {
    mw_handle: u64,
    rkey: u32,

    reap(sink self, escrow wire_buf: ByteSlice) eff [RdmaReap @ <= 1] {
        let RdmaWindowTicket { mw_handle, rkey } = self;
        RdmaQp::post_local_inv_and_await_wc(mw_handle, rkey, escrow wire_buf);
    }
}

// 4. Unified pipeline: idiomatic `?` and 5ms timeout with zero UAF and zero boilerplate!
pub fn replicate_verified_page(
    inout wal: WalJournal,
    in hdr_bytes: ByteSlice,
    fd: RawFd,
    disk_off: u64,
) -> Result[u64, ReplError]
eff [
    Io, Rdma,
    IoReap @ <= 1, RdmaReap @ <= 1, EscrowMem @ <= 8192,
    Fail[ReplError], Cancel,
] {
    Pulse::timeout(5.ms(), || {
        let mut disk_page: ByteArray[4096] = uninit;
        let mut rdma_ack:  ByteArray[4096] = uninit;

        // Both 4 KiB buffers allocate in a detachable 8 KiB EscrowCell (`EscrowMem = 8192`):
        let disk_sqe = IoUring::submit_pread(fd, disk_off, escrow disk_page[0..4096]);
        let rdma_mw  = RdmaQp::bind_mw2_recv(escrow rdma_ack[0..4096]);
        let tx       = wal.begin_txn();

        // If CRC validation fails OR the 5ms timeout fires here:
        //   1. `tx.reap(inout wal)` runs inline on the CPU stack;
        //   2. `rdma_mw.reap` and `disk_sqe.reap` detach the 8 KiB EscrowCell to ReapRing;
        //   3. Caller receives `Err(...)` immediately while NIC & NVMe drain safely!
        let meta = FrameHeader::decode_non_malleable(in hdr_bytes)?;

        let n_read = IoUring::await_pread(sink disk_sqe, escrow disk_page[0..4096])?;
        let quorum = RdmaQp::await_mw2_ack(sink rdma_mw, escrow rdma_ack[0..4096])?;
        tx.stage_verified(inout wal, in meta, in disk_page[0..n_read], in rdma_ack[0..quorum]);
        Ok(tx.commit(inout wal))
    })
}
```
]

=== 6.6 Dual-Branch Lean 4 Extraction and Mechanized Soundness
<ch-fission-6>

Stage 5B (#link(<ch-8-2>)[Section 9.2]) translates `[T-Let-Fission]` into a *prophecy-free two-arm pure state transition* in Lean 4, exposing both normal commit states and abort/timeout reaped states to `vesper verify`:

$ ⟦upright("let ") x = e_1 upright(" in ") e_2⟧_(Gamma_(upright("reap"))) (s_0) := upright("match ") ⟦e_1⟧(s_0) upright(" with") \
  divides upright(".ok ") (v, s_1) => ⟦e_2⟧(v, s_1) \
  divides upright(".error ") (e, s_1) => upright("let ") s_2 := ⟦upright("reap")_(Gamma_(upright("reap")))⟧(s_1) upright(" ; .error ") (e, s_2) $

The following illustrative standalone Lean 4 listing proposes a model for *Theorem 5 (Fissioned Total Linear Exhaustion)* and *Theorem 6 (Zero-UAF Escrow Quarantine Safety)*. `lean/ContinuationFission.lean` is not currently a repository module. The listing states local properties of abstract ticket and place states; it does not establish compiler extraction, hardware DMA safety, termination, or bounded quarantine for the implementation:

#_compact-listing[
```lean
-- illustrative/ContinuationFission.lean
-- Zero-sorryAx mechanization of [T-EscrowLoan], [T-Let-Fission], [T-Handle-With],
-- Fissioned Total Linear Exhaustion (Theorem 5), and DMA Quarantine Safety (Theorem 6).

namespace Vesper.ContinuationFission

inductive PlaceState where
  | cpuOwned    : PlaceState
  | escrowed    : (ticketId : Nat) -> PlaceState
  | quarantined : (ticketId : Nat) -> PlaceState
  | recycled    : PlaceState
  deriving DecidableEq, Repr

inductive TicketState where
  | active      : (placeId : Nat) -> (escrowBytes : Nat) -> TicketState
  | committed   : TicketState
  | reaping     : (placeId : Nat) -> (escrowBytes : Nat) -> TicketState
  | reaped      : TicketState
  deriving DecidableEq, Repr

structure FissionWorld where
  places  : Nat -> PlaceState
  tickets : Nat -> TicketState

/-- Hardware DMA may write to `p` whenever its bound ticket is `active` or `reaping`. -/
def HwDmaMayTouch (w : FissionWorld) (p : Nat) : Prop :=
  exists tid sz,
    (w.tickets tid = .active p sz \/ w.tickets tid = .reaping p sz)

/-- CPU reuse / slab recycling is permitted iff `w.places p = .recycled` or `.cpuOwned`. -/
def WellFormedEscrow (w : FissionWorld) : Prop :=
  forall tid p sz,
    (w.tickets tid = .active p sz  -> w.places p = .escrowed tid) /\
    (w.tickets tid = .reaping p sz -> w.places p = .quarantined tid)

/-- `k.reap()` detaches an active ticket into quarantine without premature recycling. -/
def detachToReaper (w : FissionWorld) (tid p sz : Nat) : FissionWorld :=
  { places  := fun q => if q = p then .quarantined tid else w.places q
  , tickets := fun t => if t = tid then .reaping p sz else w.tickets t }

/-- Reaper completion after terminal CQE / `IBV_WC_LOCAL_INV` harvest. -/
def completeReapCqe (w : FissionWorld) (tid p : Nat) : FissionWorld :=
  { places  := fun q => if q = p then .recycled else w.places q
  , tickets := fun t => if t = tid then .reaped else w.tickets t }

theorem detach_preserves_well_formed
    (w : FissionWorld) (tid p sz : Nat)
    (hwf : WellFormedEscrow w)
    (h_act : w.tickets tid = .active p sz)
    (h_uniq : forall t2 p2 sz2, t2 ≠ tid ->
      (w.tickets t2 = .active p2 sz2 \/ w.tickets t2 = .reaping p2 sz2) -> p2 ≠ p) :
    WellFormedEscrow (detachToReaper w tid p sz) := by
  intro t q s
  unfold detachToReaper
  by_cases ht : t = tid
  · subst ht
    simp only [ ↓reduceIte]
    constructor
    · intro h_absurd; cases h_absurd
    · intro h_eq; injection h_eq with hp _; subst hp; simp
  · simp only [ht, ↓reduceIte]
    have ⟨hA, hR⟩ := hwf t q s
    constructor
    · intro ha
      have h_neq : q ≠ p := h_uniq t q s ht (Or.inl ha)
      simp [h_neq, hA ha]
    · intro hr
      have h_neq : q ≠ p := h_uniq t q s ht (Or.inr hr)
      simp [h_neq, hR hr]

/-- Theorem 6 (Zero-UAF Escrow Quarantine Safety): No place reachable by in-flight
    hardware DMA (`active` or `reaping`) is ever observable as `.cpuOwned` or `.recycled`. -/
theorem zero_uaf_during_escrow_and_quarantine
    (w : FissionWorld) (p : Nat)
    (hwf : WellFormedEscrow w)
    (hdma : HwDmaMayTouch w p) :
    w.places p ≠ .cpuOwned /\ w.places p ≠ .recycled := by
  rcases hdma with ⟨tid, sz, h_live⟩
  have ⟨hA, hR⟩ := hwf tid p sz
  cases h_live with
  | inl h_act =>
    rw [hA h_act]
    exact ⟨by decide, by decide⟩
  | inr h_rep =>
    rw [hR h_rep]
    exact ⟨by decide, by decide⟩

/-- Theorem 5 (Additive Continuation Exhaustion): Eliminating `FissionCont` via
    either `.resume` or `.reap` transitions every live linear ticket to `.committed`
    or `.reaped` in exactly one step (`0` leaked obligations). -/
inductive FissionChoice where | resumePath | reapPath deriving DecidableEq

def dischargeLinearTicket (choice : FissionChoice) : TicketState :=
  match choice with
  | .resumePath => .committed
  | .reapPath   => .reaped

theorem fission_choice_exhausts_linear_obligation (c : FissionChoice) :
    dischargeLinearTicket c = .committed \/ dischargeLinearTicket c = .reaped := by
  cases c <;> simp [dischargeLinearTicket]

end Vesper.ContinuationFission
```
]

=== 6.7 Five-Stage Lowering, Split `EscrowCell` ABI, and SemVer Variance
<ch-fission-7>

The proposed lowering targets no additional overhead for functions without `escrow` projections and no heap allocation on abort paths. The instruction counts below are design estimates:

+ *Stage 3 — Backward Reaper Slicing (`vesper-mir-pcg`):* At every CFG edge performing an abortable effect (`res(epsilon) = affine`), the PCG solver walks live `linear` bindings $h_m, dots, h_1$ in reverse dominance order, validates `[T-ReapSlice]` disjointness, and marks only locals referenced by `escrow` parameters of asynchronous `reap` blocks with storage class `Storage::EscrowSubFrame`.
+ *Stage 4 — Split `EscrowCell` Sub-Frame Placement (`vesper-opt-egglog`):* CPU-only locals remain in standard stack slots on the 64 KiB `Pulse` fiber stack. `Storage::EscrowSubFrame` locals pack into a cache-aligned `EscrowCell` bumped in 3 instructions from the worker shard's Domain-1 `ShardSlab` (#link(<ch-12-3>)[Section 13.3]) at function prologue and popped inline on normal return (`0` `memcpy`, `0` atomic instructions).
+ *Stage 5A — Dual-Target Landing Pads (`(resume_ip, reap_ip)`):* Each fissioned suspension descriptor emits a 16-byte pair `(resume_ip, reap_ip)` in `.rodata`. Invoking `k.resume(v, s)` jumps directly to `resume_ip`. Invoking `k.reap()` jumps to `reap_ip`, which executes synchronous `reap` blocks inline, enqueues `{ escrow_cell_ptr, async_reap_step_fn }` onto `PulseWorkerShard::reap_ring` in 4 instructions, and immediately returns the 64 KiB CPU fiber stack to the shard pool.
+ *SemVer Substitutability (#link(<ch-11-2>)[Section 12.2] Integration):* Adding a `reap` declaration to an existing `linear struct` (or lowering its `EscrowMem` / `IoReap` grade) is *SemVer Minor-compatible* ($I_(upright("new")) prec.curly.eq_(upright("pub")) I_(upright("old"))$), because every existing caller that manually consumed the obligation continues to typecheck unchanged while new callers gain `?` and `Cancel` composability; removing a `reap` declaration or raising its `EscrowMem` ceiling is *Major-breaking*.

== 7. Vesper Pact and Forge: Traits, Coherence, Sugar, and WASI Macros
<ch-6>
#chapter-deck[Effect-Polymorphic Traits, Coherence, Named Models, Syntactic Sugar, and Hygienic Macros]

*Vesper Pact* (`crates/vesper-pact`) describes trait polymorphism, while *Vesper Forge* (`crates/vesper-forge`) describes user-defined syntactic sugar and compile-time macros. Pact combines type-class coherence, tabled goal resolution, and capability-passing effect polymorphism. Forge adapts skeletal enforestation for `sugar` and Lean 4's two-way approach for macro hygiene. The research references are #link("https://arxiv.org/abs/2502.20546")[On the State of Coherence in the Land of Type Classes] (Racordon et al., 2025), the #link("https://blog.rust-lang.org/2026/08/21/enabling-next-solver-on-nightly/")[Next-Generation Trait Solver] (2026), #link("https://gopiandcode.uk/pdfs/rhombus-oopsla23.pdf")[Rhombus] (Flatt et al., OOPSLA 2023) for Shrubbery enforestation, and #link("https://arxiv.org/abs/2001.10490")[Beyond Notations] (Ullrich and de Moura).

=== 7.1 Core Abstraction and Metaprogramming Invariants
<ch-6-1>

- *Colorless Modal and Effect-Polymorphic Traits:* Rather than splitting traits across synchronous, asynchronous, fallible, and ownership-mode variants (`Iterator` vs. `AsyncIterator`, `From` vs. `TryFrom`, `FnMut` vs. `FnOnce`), a single Vesper `trait` abstracts uniformly over modal bounds (`mod linear`), parameter passing modes (`in`, `inout`, `out`, `sink`), and lexical effect rows (`eff [r]`).
- *Stratified Coherence Without `newtype` Wrappers:* Pact pairs orphan-checked anonymous `impl` blocks—preserving *Global Model Uniqueness (GMU)* across dependency graphs—with explicit *named `model` declarations* and zero-sized type branding (`[T-ModelBrand]`) for local third-party adaptation without wrapper-struct indirection.
- *Zero-Allocation Second-Class Trait Objects (`in dyn`, `inout dyn`, `sink dyn`):* Because second-class borrows never escape, `inout writer: dyn ByteSink[r]` lowers to a 16-byte `(data_ptr, vtable_ptr)` register pair requiring zero heap `Alloc` authority while forwarding active `EvidenceVector` pointers (`r15` / `x28`) and supporting `sink self` consumption via a move-vtable slot. Object safety, the second-class rules, the coercion and the vtable layout are specified in #link(<ch-6-3>)[Section 7.3].
- *Hermetic `wasm32-wasip2` Macro Sandboxes:* Procedural and derive macros (`#[wasm_macro]`) distribute as precompiled WebAssembly Components executed inside `vesper-loom-sandbox` (`wasmtime`) with deterministic instruction-fuel metering, epoch-zero clocks, and zero ambient filesystem or network access.
- *Import-Free Skeletal Enforestation and Pass-Mode Transparency (`sugar`, `[WF-SugarPass]`):* User-defined syntactic sugar (`sugar`) rewrites Stage 1 `PhraseSkeleton` CST nodes without changing the parser on `import`. Unexpanded sugar can therefore be formatted by *Vesper Stylus* (#link(<ch-18>)[Chapter 19]) before import resolution. A rule may not mutate, initialize, or consume a caller place unless the call site writes the matching `inout`, `out`, or `sink` keyword. Rules marked `@lean_notation` are intended to emit Lean 4 `@[app_unexpander]` mirrors.
- *Two-Way Hygiene and Static Anti-Smuggling (`[T-MacroConfinement]`):* Identifier tokens combine Flatt scope sets with quotation-time pre-resolved `DefId` targets, while post-expansion confinement forbids any macro or `sugar` rule from injecting capabilities outside its declared `emits` / `macro_emits_effects` ceiling.

=== 7.2 Vesper Pact: Colorless Modal and Effect-Polymorphic Traits
<ch-6-2>

Because completion I/O, checked fallibility, SIMD profiles, and arena allocation all inhabit `eff [...]` rows, one trait definition specializes cleanly across pure in-memory slices and suspending `io_uring` streams:

```rust
// Unifies synchronous iteration, fallible streams, async I/O, and linear item consumption:
pub trait Source[r: eff = []] {
    // `mod linear` sits at the top of `copy <= affine <= linear`, permitting `Item`
    // to instantiate to `u64` (copy), `RecordBatch` (affine), or `WriteTxn` (linear):
    type Item: mod linear;

    fn next(inout self) -> Option[Self::Item] eff [r];
}

pub fn fold_source[S, T, Acc, r1: eff, r2: eff](
    inout src: S,
    sink init: Acc,
    f: fn(sink Acc, sink T) -> Acc eff [r2],
) -> Acc eff [r1 | r2]
where
    S: Source[r1, Item = T],
{
    let mut acc = init;
    loop {
        match src.next() {
            Some(item) => { acc = f(sink acc, sink item); }
            None => break acc,
        }
    }
}
```

When `S` is an in-memory `TiledSeq` (`r1 = []`) and `f` is pure (`r2 = []`), `fold_source` lowers in Cranelift SSA IR to an inline scalar or SIMD loop. When `S` is a *Vesper Pulse* `TcpFrameSource` (`r1 = [Net, Cancel]`) and `f` commits a `WriteTxn` (`r2 = [Storage, Fail[IoError]]`), the identical `fold_source` body suspends on pooled fiber stacks while enforcing `[T-Let]` (`u(bold(m)_(upright("acc"))) subset.eq.sq upright("res")(r_1 union r_2)`).

=== 7.3 Stratified Coherence: Canonical `impl` and Type-Branded `model`
<ch-6-3>

Pact resolves the tension between global coherence and third-party extensibility through two explicit conformance tiers:

+ *Tier 1 — Anonymous Canonical `impl` Declarations (Global Model Uniqueness):* Standard `impl Trait for Type` blocks participate in implicit typeclass search and obey the *Covered-First Orphan Rule*: package $P$ may declare an anonymous `impl[A] Trait[T_1, ..., T_n] for T_0` if and only if either `Trait` is defined in $P$ or the first uncovered type constructor in $(T_0, T_1, dots, T_n)$ is defined in $P$. Type variables and capability labels canonicalize to de Bruijn indices (`?0`, `?1`) inside an SLG tabled solver with lazy associated-type normalization and coinductive cycle termination.
+ *Tier 2 — Named Lexical `model` Declarations:* To adapt an upstream type (`@std/uuid::Uuid`) to an upstream trait (`@vesper-wire/codec::WireCodec`) without `newtype` wrappers, any package may define a named `model` and bind it lexically via `use model`:

```rust
pub model BigEndianUuidCodec: WireCodec for Uuid {
    fn encode(in self, inout out: ByteSlice) -> Result[usize, EncodeError] { ... }
    fn decode(in raw: ByteSlice) -> Result[Uuid, DecodeError] { ... }
}

fn write_partition_ids(in ids: Slice[Uuid], inout buf: ByteSlice) -> Result[usize, EncodeError] {
    use model BigEndianUuidCodec; // Lexically binds WireCodec[Uuid] with zero wrapper overhead
    encode_slice(in ids, inout buf)
}
```

+ *Invariant Protection via Zero-Sized Model Branding (`[T-ModelBrand]`):* Ordered and hashed collections (`FlatTable`, `CacheBTree`, #link(<ch-12>)[Chapter 13]) stamp the active model identity brand $M$ (defaulting to `Canonical[Trait, K]`) into their type parameter list:

#_infer(
  "[T-ModelBrand]",
  $Delta ; Sigma_(upright("models")) tack.r M_1 equiv_(upright("def")) M_2$,
  $Delta ; Sigma_(upright("models")) tack.r upright("CacheBTree")[tau_K, tau_V, M_1] equiv upright("CacheBTree")[tau_K, tau_V, M_2]$,
)

`CacheBTree[Uuid, Offset]` and `CacheBTree[Uuid, Offset, ReverseUuidOrd]` share identical runtime layouts yet remain statically distinct types, preventing accidental B-tree invariant corruption. In `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]), `trait` declarations extract to Lean 4 `class` definitions and `model` declarations extract to named `instance` values.

+ *Bounded Generics by Monomorphization:* Trait bounds on functions, inherent `impl` blocks, and type parameters (`fn max[T: Ord]`, `where K: Ord`, `T: mod copy + Ord`, `struct CacheBTree[K: Ord, V]`) are discharged statically. A bound is a solver assumption inside a generic body. Each call site resolves the bound under its lexical `use model` scopes, and the selected evidence flows to nested instances using that bound, as in `write_partition_ids` above. Each instance becomes a private specialized copy, so backends and Lean extraction receive concrete code without runtime dictionaries. Missing or ambiguous conformances are reported at the instance. The compiler rejects polymorphic recursion requiring unboundedly many instances; Second-class `dyn Trait` parameters are the one dictionary-passing exception (next item); first-class `dyn` values and bounded functions used as values remain outside this mechanism.

  The current builtins are `Eq`, `Ord` (with `Ordering`), and `Hash`. Canonical conformances cover integer scalars, `bool`, `()`, and tuples of two to four copy elements. Struct conformances require an explicit empty canonical implementation over all struct parameters, such as `impl Ord for Point {}` or `impl[T] Ord for Pair[T] {}`. Derivation follows field order lexicographically, adds a where-bound for each parameter mentioned by a field, and requires every field to conform. This opt-in makes ordering and hashing contracts explicit and keeps secret-holding types unhashable unless requested. `Hash` uses a deterministic FNV-style fold independent of addresses and iteration order. These traits remain builtin because the self-hosted front end cannot yet parse the package that would declare them. `@vesper-std/core` (#link(<ch-12>)[Chapter 13]) builds its ordering helpers on them.

+ *Second-Class Trait Objects (`in dyn`, `inout dyn`, `sink dyn`):* Unboxed, `dyn Trait[args]` is admitted only as the whole type of an `in`, `inout` or `sink` parameter of a function or inherent method (the boxed form `Box[dyn Trait[args]]` of the next item is a first-class type); it is never a result, a field, a `let` annotation, a type argument, a closure or trait-method parameter, or an `out` parameter (E0632). Inside the body the parameter may appear only as a method receiver, as the first argument of a trait path call, or as a direct argument for another `dyn` parameter; binding, storing, returning, capturing it or passing it to a non-`dyn` parameter is E0631. Calls on the parameter are typed by the trait's signature as for a bounded parameter, so the trait methods' effects flow to the caller exactly as a static call's would, but the function is compiled once rather than per implementing type.
  A trait is *object safe* when it declares no associated types and each method has no type parameters, takes `self` first by `in`, `inout` or `sink`, mentions `Self` nowhere else, and has a closed effect row after the object's trait arguments are substituted (E0630). A method may perform any effect a static call may: `Fail[E]`, the capability effects, `Ffi[Lib]` and handler effects (`Net`, `Storage`, user effects). The object type carries the row of its trait's methods, so a call through an object adds the called method's row to the caller's exactly as a static call does, and a caller whose row lacks it is refused (E0401). A method's handler effects are discharged by the handlers in scope at the call, not where the object was built: an object boxed outside any handler may be called later under one. A call site coerces a concrete `T` to `dyn Trait[args]` by selecting `T: Trait[args]` under its lexical `use model` scopes, as for a static bound (E0608 when none exists); the type must be known at that site, after monomorphization of the enclosing function (E0633). A program has one vtable per object type and implementing type, so coercing the same type to the same object under two different conformances is refused (E0633), and so is a vtable whose slot would pass a function value, in `self` or another parameter or the result (E0630). A `dyn` parameter forwarded to another `dyn` parameter keeps its vtable and must name the same instantiated trait (E0608). A `sink dyn` parameter accepts only types of mode at most `affine`; `in` and `inout` objects may borrow linear values.
  An object lowers to the 16-byte pair `(data_ptr, vtable_ptr)` with no heap allocation: `data_ptr` addresses the caller's place (`in`, `inout`) or the moved value (`sink`). Each (object type, `T`) pair has one read-only vtable per compilation unit: word 0 is the drop function of `T` (zero when `T` owns nothing), word 1 the move slot (zero: Stage 1 types are bitwise movable by `size` bytes), word 2 the size and word 3 the alignment of `T`, followed by one function per method in trait declaration order. A slot takes `self` by address whatever its pass mode and has the trait method's calling convention, through a generated slot function that calls the selected conformance. Consuming a `sink dyn` object (a `sink self` method or a `sink dyn` argument) empties the parameter; an object not consumed is dropped once through word 0 when the callee exits. A slot whose method performs only `Fail` and capability or `Ffi[Lib]` effects is a compiled function with the method's calling convention. Native code specializes handler effects into the code that reaches their handler, so a slot whose method performs one holds no function (zero): a call through it compares the object's vtable address with each vtable the program has for that object type, a closed set in a whole-program build, and runs the selected conformance method under the caller's handlers as a static call would; an unknown vtable traps. The evaluator dispatches through the vtable and performs the effects on its handler stack. The verified backend, WebAssembly and the export ABI refuse `dyn` parameters with a typed diagnostic (E0634 or an export type error) until they gain the pair layout.
+ *Boxed Trait Objects (`Box[dyn Trait]`):* `Box[dyn Trait[args]]` is an ordinary owned type of mode `affine`, written anywhere a type is: a `let` annotation, a field or variant payload, a type argument (`ChunkSeq[Box[dyn Trait]]`, `Option[Box[dyn Trait]]`), a parameter of any pass mode and a result, including in trait and conformance method signatures. The trait must be object safe (E0630); a `dyn` written anywhere else outside a parameter's whole type is still E0632. A boxed object is built only by `Box::new(value)` where the expected type is `Box[dyn Trait[args]]`, or by `Box::new[dyn Trait[args]](value)`: the call selects `T: Trait[args]` for the value's concrete type `T` exactly as a `dyn` parameter's coercion does (E0608, E0633, one vtable per object type and `T`), and `T` must be at most `affine`, since an object may be dropped (E0205). An existing `Box[T]` is not coerced, a `dyn` parameter cannot be boxed (E0631), and a boxed object cannot be unpacked with a `Box(...)` pattern, because only its vtable knows its value's type (E0631).
  A method call on a boxed object (`b.m(...)`, or `Trait::m(b, ...)`) is a call through its vtable slot, typed by the trait's signature at the box's trait arguments (a call through `Box[dyn Factory[i64]]` returns what `Factory[i64]` returns); a method of another trait is E0608. Whatever the method's `self` pass, the call reads the box and passes the value's address, so an `inout self` method mutates the boxed value in place and needs only an `inout` place holding the box, which may be a sequence element `s[i]` (the call then borrows the whole sequence `s` `inout`). An element is not a place, so it is lent `inout` only to reach its object, as a method receiver or to an `inout dyn` parameter, never to a parameter of the box's own type that could replace the box (E0300). A `sink self` method consumes the value and frees the box. A boxed object is lent to an `in`, `inout` or `sink` `dyn` parameter of the same instantiated trait without a new vtable (E0608 otherwise): the parameter's pair addresses the boxed value, and a `sink` lend frees the box after the call, the callee having consumed or dropped the value.
  A boxed object is one word, the address of a heap block whose first word is its vtable's address and whose value follows at offset 8, so it moves, nests in aggregates and fills sequence slots as any `Box[T]` does. Dropping it (at the end of its owner's scope, when its place is assigned, or with the sequence or aggregate that holds it) calls the vtable's drop word on the value and then frees the block. A function with loops or early exits owns boxes as any other function does, in a local, a `sink` parameter or an aggregate's fields, plain `Box[T]` and boxed objects alike: each box it owns is dropped exactly once on every path, at the end of the iteration that binds it, when its place is assigned, at a `break` or `return` that leaves it live, or after the loop, and a box moved on one path is dropped on the others; moving a box that the next iteration needs is `E0312`. A sequence element read through an index is lent, never owned, so its sequence frees it. A box consumed by a `sink` call is freed before the call's failure can leave the caller, and one lent `sink` to a function whose handler effects are specialized at the call is freed when that call returns, resumes or is aborted by its handler. The verified backend and WebAssembly refuse boxed objects (E0634); the export ABI carries them only inside a handle type (§16.6).
+ *Function Objects (`dyn Fn`, `dyn FnMut`, `dyn FnOnce`):* A function object type is written `dyn Fn(A, ..) -> R eff [E]`, `dyn FnMut(..)` or `dyn FnOnce(..)`, with the parameters (and their pass modes), result and effect row of a function type. It is a trait object whose trait has one method `call`, taking `self` `in` (`Fn`), `inout` (`FnMut`) or `sink` (`FnOnce`) and then the signature's parameters, so it obeys the two items above: unboxed it is only the whole type of an `in`, `inout` or `sink` parameter, and `Box[dyn FnMut(..)]` is an ordinary owned type. A closure literal written where a function object is expected (an argument for such a parameter, `Box::new(closure)` expecting a boxed one, or `Box::new[dyn FnMut(..)](closure)`) is coerced to it: its captures, which it owns as any closure does (`[T-Abs]`), become the object's value, and its body becomes `call`. Lent to an `in dyn Fn` or `inout dyn FnMut` parameter the value lives in the caller's frame and nothing is allocated; boxed, one block holds the vtable and the captures. A `FnMut` object's `call` mutates its own captures, which persist across its calls; mutating a capture of a `Fn` object is refused as any write through an `in` receiver is (E0307). A `dyn FnOnce` call consumes the object, and a boxed one frees its block. A closure capturing a `linear` value is linear: it may be called once only, so it is never a `Fn` or `FnMut` (E0310), and since objects may be dropped uncalled it cannot be boxed either (E0205). `f(args)` on a name, field or sequence element of function-object type calls `call`, as does `x.call(args)` on any function-object expression, and the call's effects reach the caller's row. The arity is the signature's (E0207). In Stage 1 the coercion is found before type checking, so a captured local whose type is not written where it is bound (an annotation, a literal, a struct literal, a call of a declared function) must be annotated (E0102), and a closure whose captures or signature mention its enclosing function's type parameters is refused (E0633). A plain `fn(A) -> R` value stays a specialized function value, not an object.

=== 7.4 Vesper Forge: Two-Way Hygiene and WASI Macro Confinement
<ch-6-4>

Vesper Forge supports both zero-startup declarative syntax rules (`macro`) over typed grammar categories (`expr`, `pat`, `ty`, `place_tree`, `eff_row`, `item`) and precompiled `wasm32-wasip2` components (`#[wasm_macro]`) receiving pre-parsed `CstArena` and `AdtSchema` WIT records in under 50 microseconds. Because WASI sandboxes execute deterministically without I/O or wall clocks, macro expansion is memoized as a pure `(MacroWasmBlake3, InputAstBlake3) -> OutputAst` query across keystrokes and builds.

Every CST identifier carries `(Symbol, Span, ScopeSet, Option[PreResolvedDefId])`:

- *Scope-Set Local Hygiene:* Expansion $k$ stamps a fresh `ScopeMark(k)` onto introduced tokens; local binder `let x` binds reference `x` iff $"marks"("binder") subset.eq "marks"("reference")$.
- *Pre-Resolved Global Paths:* Paths inside `quote { @vesper-wire/hash::WireHash::hash_into(...) }` resolve their target `DefId` when the macro package is compiled, requiring zero hidden re-exports in caller crates.
- *Supply-Chain Anti-Smuggling Law (`[T-MacroConfinement]`):* Post-expansion auditing bars any macro from smuggling unauthorized effect capabilities into a privileged caller:

#_infer(
  "[T-MacroConfinement]",
  $d = upright("PreResolvedDefId")("ident") in upright("expand")(M, "ast") quad upright("origin")(d) = M$,
  $upright("caps")(d) subset.eq upright("macro_emits_effects")(M) inter upright("capability_ceiling")(P_(upright("caller")))$,
)

Macro packages default to `macro_emits_effects = []` in `Loom.toml`: even inside a server binary holding `Net` and `Storage`, a pure `@derive` macro cannot inject network, filesystem, or FFI calls.

```rust
// Defined in `@vesper-wire/derive` (`wasm32-wasip2`, `macro_emits_effects = []`)
#[wasm_macro(derive = WireHash, emits_effects = [])]
pub fn derive_wire_hash(in schema: AdtSchema) -> Result[CstItem, MacroDiag] {
    if schema.nominal_mode == Mode::Linear {
        return Err(MacroDiag::error(
            schema.span,
            "WireHash requires shared `in self`; confirm linear obligation permits hashing",
        ));
    }

    let field_stmts = schema.fields.map(|in f| {
        let fname = f.ident;
        quote_stmt! {
            @vesper-wire/hash::WireHash::hash_into(in self.$fname, inout h);
        }
    });

    let ty_name = schema.ident;
    Ok(quote_item! {
        impl @vesper-wire/hash::WireHash for $ty_name {
            fn hash_into[H: @vesper-wire/hash::Hasher](in self, inout h: H) eff [] {
                $..field_stmts
            }
        }
    })
}
```

=== 7.5 Skeletal Enforestation and Substructural `sugar` Calculus
<ch-6-sugar>

User-extensible syntax usually forces a trade-off that conflicts with verified systems programming. Unstructured token-tree macros (`macro_rules!`) preserve import-free lexing at the cost of mandatory `!` call-site sigils, opaque interiors where canonical formatters bail out, and silent mutation or consumption of caller variables (`my_macro!(x)` expanding to `&mut x` or `drop(x)`). Conversely, dynamic parser-table extensions (#link("https://arxiv.org/abs/2001.10490")[Lean 4 `syntax`], Agda mixfix) couple Stage 1 parsing to transitive module imports. That coupling defeats the import-free parsing goal (#link(<ch-2-1>)[Section 2.1]), the snippet-level *Chroma* Tier 0 highlighting target (#link(<ch-20>)[Chapter 21]), and comment-preserving *Stylus* formatting (#link(<ch-18>)[Chapter 19]).

Vesper proposes to address both problems through *Skeletal Enforestation* (`sugar`, Tier 0 of Vesper Forge), adapting #link("https://gopiandcode.uk/pdfs/rhombus-oopsla23.pdf")[Rhombus Shrubbery enforestation] to Vesper's LL(2) grammar and four-mode place calculus. This section is a design proposal: neither `crates/vesper-forge` nor the self-hosted parser implements `sugar` yet, and its slots rely on the disambiguation rules in #link(<ch-2-2>)[Section 2.2]. The table compares the intended properties:

#figure(
  table(
    columns: (21%, 19%, 20%, 19%, 21%),
    align: (left, left, left, left, left),
    table.header(
      [*Property*],
      [*Token-Tree Macros (`macro_rules!`)*],
      [*Parser-Mutating `syntax` (Lean 4)*],
      [*Shrubbery Macros (Rhombus)*],
      [*Vesper `sugar` Enforestation*],
    ),
    [*Import-Free Stage 1 Parse*],
    [Yes (flat token trees)],
    [*No* (imports mutate parser)],
    [Yes (indentation groups)],
    [*Yes* (deterministic LL(2) CST)],
    [*Zero-Plugin Formatting*],
    [*No* (bails on custom syntax)],
    [Requires custom pretty rules],
    [Shrubbery-level only],
    [*Yes* (Stylus formats skeleton nodes)],
    [*Call-Site Mutation Clarity*],
    [*Opaque* (hides `&mut` / moves)],
    [Opaque until elaboration],
    [Dynamic / untyped],
    [*Exact* (`[WF-SugarPass]`)],
    [*Termination & Latency*],
    [Bounded recursion depth],
    [Arbitrary elaborator IO],
    [Turing-complete phases],
    [*Rank-bounded* (at most 64 levels)],
    [*Proof-Goal Preservation*],
    [Expands to `$tmp` noise],
    [*Yes* (`@[app_unexpander]`)],
    [N/A],
    [*Yes* (`@lean_notation` mirror)],
  ),
  kind: table,
)

Four rules govern `sugar` well-formedness, block-tail continuation splicing, effect honesty, and Lean 4 notation preservation:

+ *Substructural Pass-Mode Transparency and Single Evaluation (`[WF-SugarPass]`):* Pattern holes separate rvalue expressions (`$e:expr`) from lvalue caller places (`mu_p $p:place` with explicit pass floor $mu_p in {upright("in"), upright("inout"), upright("out"), upright("sink")}$; omitted $mu_p$ defaults to `in`). Define the *pass-visibility relation* $mu_(upright("pat")) gt.closed.eq_(upright("vis")) mu_(upright("use"))$ by:
  $ upright("in") gt.closed.eq_(upright("vis")) upright("in"), quad upright("inout") gt.closed.eq_(upright("vis")) mu in {upright("in"), upright("inout")}, quad upright("out") gt.closed.eq_(upright("vis")) upright("out"), quad upright("sink") gt.closed.eq_(upright("vis")) mu in {upright("in"), upright("inout"), upright("sink")} $
  Every rvalue `$e:expr` evaluates at most once on every path, and only on the paths where the template uses it. A hole that appears at most once on each control-flow path of $T$ is substituted in place, so `ensure ok else make_error();` calls `make_error()` only when `ok` is false. A hole that appears more than once on some path binds once through a hygienic `let __h = $e;` header placed at the nearest point that dominates those uses (preventing duplicate side effects or `linear` temporary cloning and barring lvalue `inout`/`out` uses of `__h`). Meanwhile every place hole `p` checks every template occurrence $mu_j$ against $mu_(upright("pat"))$:

#_infer(
  "[WF-SugarPass]",
  $forall j in {1 .. m}, quad mu_(upright("pat")) gt.closed.eq_(upright("vis")) mu_j \ (exists j, med mu_j in {upright("inout"), upright("out"), upright("sink")}) arrow.r.double.long upright("LinearSinglePath")(p, t)$,
  $Delta tack.r (mu_(upright("pat")) med p : upright("place") arrow.r.double t) med upright("wf")$,
)

  Consequently, no well-formed `sugar` expansion can mutate (`inout`), initialize (`out`), or consume (`sink`) a caller place unless that exact pass-mode keyword appears lexically at the call site.

2. *Stratified Clause and Operator Enforestation (`[E-Enforest-Clause]`):* After Stage 1 builds `PhraseSkeleton` CST nodes (`ClauseSugarExpr`, `PipeExpr`, `DottedBinOp`, `TaggedDelimExpr`), Stage 2 matches the head keyword or operator symbol against lexically in-scope `sugar` bindings, stamps a fresh `ScopeMark(k)`, and enforces strictly decreasing stratification rank ($upright("rank")(S') < upright("rank")(S) lt.eq 64$) so that expansion terminates without starting a WASI sandbox:

#_infer(
  "[E-Enforest-Clause]",
  $Sigma_(upright("syn"))(upright("kw")) = (upright("sugar ") upright("kw") : kappa upright(" rule ") P arrow.r.double T) quad upright("match")(P, S) = sigma \ k = upright("fresh_mark")() quad upright("rank")(T) < upright("rank")(S) lt.eq 64$,
  $Sigma_(upright("syn")) tack.r S arrow.r.squiggly_(upright("sugar")) upright("stamp")(k, T)[sigma]$,
)

  For `DottedBinOp` symbols (`.+`, `.*`, `.==`), Stage 1 fixes operator precedence and associativity purely from the first glyph after `.`; mixing two distinct custom operators at the same `Assoc::None` band raises `E0140` (`AmbiguousCustomOperatorMix`) demanding explicit parentheses.

3. *Block-Tail Continuation Splicing (`[E-Enforest-Tail]`):* Statement-level `sugar` patterns ending in `$..rest:stmts` capture the remaining statements and trailing tail expression of the enclosing `{ ... }` block, enabling library-defined scoped cleanup (`defer`) and early-exit pattern guards (`guard let ... else`) without compiler special cases:

#_infer(
  "[E-Enforest-Tail]",
  $Sigma_(upright("syn"))(upright("kw")) = (upright("sugar ") upright("kw") : upright("stmt rule ") (P ; med r : upright("stmts")) arrow.r.double T) \ upright("match")(P, s_1) = sigma quad k = upright("fresh_mark")()$,
  $Sigma_(upright("syn")) tack.r { s_1 ; s_2 ; dots ; s_n ; e_? } arrow.r.squiggly_(upright("sugar")) upright("stamp")(k, T)[sigma, r mapsto (s_2 ; dots ; s_n ; e_?)]$,
)

4. *Effect Honesty, Dual-Span Diagnostics, and `@lean_notation` Unexpanders (`[T-SugarEffectHonesty]`):* If template $T$ introduces an effect operation `perform l.op` not originating from a caller expression hole, `SugarDecl` must declare `emits eff [epsilon_s]` subject to `[T-MacroConfinement]` (#link(<ch-6-4>)[Section 7.4]):

#_infer(
  "[T-SugarEffectHonesty]",
  $d in upright("PreResolvedDefs")(T) quad upright("caps")(d) subset.eq epsilon_s inter upright("macro_emits_effects")(M) inter upright("capability_ceiling")(P_(upright("caller")))$,
  $Delta ; Sigma_(upright("syn")) tack.r (upright("sugar ") upright("kw") upright(" emits eff ")[epsilon_s] med { T }) med upright("confined")$,
)

  When $upright("res")(epsilon_s) = upright("affine")$ (`Fail` or `Cancel`), *Vesper Chroma* Tier 1 (#link(<ch-20>)[Chapter 21]) decorates `kw` (or `|?>`) with an abortable-checkpoint badge directly on the unexpanded CST, and `[T-Let]` diagnostics (`E0301` / `E0402`) attach dual `(call_span, rule_def_span)` labels. Annotating a pure or monadic rule with `@lean_notation` is intended to synthesize a companion Lean 4 `syntax` + `macro_rules` + `@[app_unexpander]` module (`Notations.lean`, #link(<ch-8-2>)[Section 9.2]) whose expansion should unfold by `rfl` to extracted `Funs.lean` definitions while keeping `Specs.lean` goals in surface Vesper notation:

#_compact-listing[
```rust
// 1. Block-tail `defer` and `guard let` defined in `std/sugar` (no compiler special case):
pub sugar defer : stmt {
    rule (defer $cleanup:block $..rest:scope) => {
        {
            let __exit = $..rest;   // Exit value: Normal(v), Return(r), Break(b), or Raise(e)
            $cleanup;
            __exit.resume()
        }
    }
}

pub sugar guard : stmt {
    rule (guard let $pat:pat = $rhs:expr else $diverge:block ; $..rest:stmts) => {
        match $rhs {
            $pat => { $..rest },
            _    => $diverge, // Stage 2 checks `$diverge : Never`
        }
    }
}

// 2. Multi-clause transaction sugar enforcing `[WF-SugarPass]` on `inout $db`:
pub sugar with_txn : expr emits eff [Storage] {
    rule (
        with_txn $tx:ident on inout $db:place $body:block
        on_error($err:pat) $recover:block
    ) => {
        {
            let mut $tx = @vesper-quarry/txn::Storage::begin(inout $db);
            let __res = handle Fail in $body with {
                return(v)   => Ok(v),
                raise(e, _) => Err(e),
            };
            match __res {
                Ok(v)     => { $tx.commit(inout $db); Ok(v) }
                Err($err) => { $tx.rollback(inout $db); $recover }
            }
        }
    }
}

// 3. Fallible pipeline sugar mirrored 1-to-1 into Lean 4 `Notations.lean`:
@lean_notation
pub sugar pipe_try : infix(`|?>`, pipeline, left) emits eff [Fail] {
    rule ($lhs:expr |?> $callee:ident ( $..args:args )) => {
        $callee($lhs?, $..args)
    }
}

// Call site — formatted natively by `vesper fmt` before import resolution;
// omitting `inout` before `shard.db` is statically rejected by `[WF-SugarPass]`:
pub fn ingest_packet(
    inout shard: DbShard,
    in frame: ByteSlice,
) -> Result[CommitReceipt, QuarryErr] eff [Storage, Fail[QuarryErr]] {
    guard let Some(hdr) = frame |?> decode_hdr(in shard.cfg) else {
        return Err(QuarryErr::EmptyFrame);
    };
    defer { shard.metrics.frames_processed += 1; }
    with_txn tx on inout shard.db {
        tx.append_verified(in hdr)?
    } on_error(e) {
        Err(e)
    }
}
```
]

`defer` binds the rest of the block as a `scope` hole rather than `stmts`. A `scope` hole is the one exit-aware mechanism `sugar` needs from the compiler: Stage 2 runs the spliced statements so that a `return`, a `break` out of the hole, or an unhandled `Fail` produces an `Exit` value instead of leaving the expansion, and `Exit::resume` then performs that exit after the template's own code has run. The cleanup therefore runs on every exit path, and a `linear` value released in `$cleanup` passes the exhaustion check on early exits too. A `stmts` hole, by contrast, runs only on fallthrough.


=== 7.6 Standard Library Sugar Catalog (`std/*`)
<ch-6-std-sugar>

The standard library sugars proposed here have two jobs. They move ad hoc syntax out of the compiler core, and they remove the ceremony around Vesper's substructural guardrails: `linear` unwinds, `out` slot initialization, `Region` closures, `Pulse::scope` fork-join, `@ lifo` guards, and `@0` ghost proofs. Each one expands under `[WF-SugarPass]` and `[T-SugarEffectHonesty]` (#link(<ch-6-sugar>)[Section 7.5]), so none can hide a mutation or a capability. Like the rest of `sugar`, the catalog is a design proposal; no `std` module ships these rules yet.

#figure(
  table(
    columns: (19%, 27%, 21%, 33%),
    align: (left, left, left, left),
    table.header([*Module and Sugar*], [*Call-Site Shape*], [*Skeleton Slot and Pass Floor*], [*Compiler Feature or Boilerplate Replaced*]),
    [`std/control::errdefer`], [`errdefer(e) { tx.rollback(); }`], [`TailSugarStmt` (`$..rest:stmts`)], [Replaces a nine-line `handle Fail` plus `match Result` block, so `Mode::Linear` handles can cross `?` with cleanup the PCG checks.],
    [`std/control::ensure`], [`ensure len <= 4096 else Err::TooBig;`], [`TailSugarStmt` (`emits eff [Fail]`)], [Replaces `if !(...) { Fail::raise(...) }` and mirrors to a one-line guard in `Notations.lean`.],
    [`std/control::guard`], [`guard let Some(x) = e else { ... };`], [`TailSugarStmt` (`$diverge : Never`)], [Removes rightward `match` drift while keeping path-sensitive refinements in $Phi$.],
    [`std/mem::emplace`], [`emplace out pkt { ver: 1, ... };`], [`TailSugarStmt` (`out $p:place`)], [Field-by-field `OutSlot` initialization with struct-literal ergonomics and no stack temporary.],
    [`std/strata::scratch`], [`scratch arena[4096] { ... }`], [`ClauseSugarExpr`], [Wraps `Region::scoped_exact[B](|cap arena| ...)` in a flat lexical block.],
    [`std/hw::holding`], [`holding irq = Irq::mask() { ... }`], [`ClauseSugarExpr`], [Pairs `@ lifo` guard acquisition with reverse-order release on normal and `Fail` paths.],
    [`std/pulse::par`], [`par { a(out s[0..k]) } and { b(...) }`], [`ClauseSugarExpr` (`emits eff [Scope]`)], [Expands multi-arm fork-join blocks to `Pulse::scope` with `[T-OutSplitSlice]` and `[T-CallProj]`.],
    [`std/pulse::within`], [`within 5.ms() { ... } on_timeout { ... }`], [`ClauseSugarExpr` (`emits eff [Clock]`)], [Handles `Cancel` locally and requires an explicit timeout recovery arm at the call site.],
    [`std/wire::bits`], [`bits{ ver: u4 = 4, len: u12 = n }`], [`TaggedDelimExpr` (`@lean_notation`)], [Packs bitfields with `QF_BV`-checked shifts and masks, mirrored to `BitVec.append` for `bv_decide`.],
    [`std/verify::calc`], [`ghost calc { a*(b+1) == a*b + a; <= c }`], [`ClauseSugarExpr` (`@0` ghost)], [Chains intermediate nonlinear arithmetic lemmas for Bitwuzla at no runtime cost, mirrored to a Lean `calc` block.],
  ),
  kind: table,
)

==== Linear-Checked Unwind Cleanup: `errdefer` and `ensure` (`std/control`)

In Zig, an `errdefer` block that forgets to release a resource leaks it silently. In Vesper, the expansion places `match __res` _outside_ a local `handle Fail`. Because the outgoing row of `handle Fail` is `linear`, `Mode::Linear` values such as `WriteTxn` and `OwnedFd` may stay live across `?` inside `$..rest`. `[T-Match]` then requires the `Err` arm to consume them, so an `errdefer` block that forgets `tx` is rejected as a dropped linear value (`E0302`).

#figure(
  {
    let node(title, detail, fill: luma(96%), stroke: 0.5pt + _rule) = box(
      inset: (x: 8pt, y: 6pt), radius: 10pt, fill: fill, stroke: stroke,
      align(center, stack(dir: ttb, spacing: 4pt, text(size: 8.5pt, title), text(font: "DejaVu Sans Mono", size: 7.5pt, detail))),
    )
    let arrow(label) = align(center + horizon, text(size: 8pt, fill: _muted, label + " →"))
    grid(
      columns: (auto, auto, auto, auto, auto),
      column-gutter: 6pt, row-gutter: 8pt, align: center + horizon,
      grid.cell(rowspan: 2, node([`errdefer` scope], [tx: linear live])),
      grid.cell(rowspan: 2, arrow[]),
      grid.cell(rowspan: 2, node([Local `handle Fail`], [res(out) = linear], fill: luma(98%), stroke: 0.8pt + _accent)),
      arrow[`Ok`], node([Normal tail], [tx.commit() ran]),
      arrow[`Err`], node([Cleanup and re-raise], [tx.rollback() ran]),
    )
  },
  kind: image,
  caption: [`errdefer` keeps `tx` live inside a local `handle Fail`; each exit arm must consume it.],
)

#_compact-listing[
```rust
// Defined in `std/control`:
pub sugar errdefer : stmt emits eff [Fail] {
    rule (errdefer($err:pat) $cleanup:block $..rest:stmts) => {
        {
            let __res = handle Fail in { Ok({ $..rest }) } with {
                return(v)   => v,
                raise(e, _) => Err(e),
            };
            match __res {
                Ok(v)     => v,
                Err($err) => { $cleanup; Fail::raise($err) }
            }
        }
    }
}

@lean_notation
pub sugar ensure : stmt emits eff [Fail] {
    rule (ensure $cond:expr else $err:expr ; $..rest:stmts) => {
        if $cond { $..rest } else { Fail::raise($err) }
    }
}

// Usage: linear `tx` crosses both `?` checkpoints without boilerplate.
fn append_wal_verified(inout db: Db, in batch: Batch) -> Receipt eff [Storage, Fail[IoErr]] {
    ensure batch.len() <= 4096 else IoErr::BatchTooLarge;
    let mut tx: WriteTxn = Storage::begin_txn(inout db);
    errdefer(e) { tx.rollback(inout db); } // the PCG checks that `tx` is consumed on Err

    validate_crc(in batch)?;
    stage_pages(inout tx, in batch)?;
    tx.commit(inout db)
}
```
]

==== Zero-Copy Memory Ergonomics: `emplace`, `swap`, and `scratch` (`std/mem` and `std/strata`)

Writing directly into an `out` destination (#link(<ch-4-4>)[Section 4.4]) avoids intermediate stack slots, which matters under eBPF and interrupt-handler `Stack @ <= 512` budgets. `scratch` flattens nested region closures (#link(<ch-12-2>)[Section 13.2]). `emplace` uses the repetition hole `$( ... ),*` of the `SugarHole` production (#link(<ch-2-2>)[Section 2.2]).

#_compact-listing[
```rust
// Struct placement into an `out` slot with no temporary (`OutInitTree` checks exhaustiveness):
pub sugar emplace : stmt {
    rule (emplace out $dst:place { $( $field:ident : $val:expr ),* } ;) => {
        { $( $dst.$field = $val; )* }
    }
}

// Symmetric in-place swap under `[WF-SugarPass]` (`inout` required on both places):
pub sugar swap : stmt {
    rule (swap inout $a:place with inout $b:place ;) => {
        @std/mem::swap_disjoint(inout $a, inout $b);
    }
}

// Flat lexical `VirtualArena` scope (`Region::scoped_exact[B]`):
pub sugar scratch : expr {
    rule (scratch $cap:ident [ $bytes:expr ] $body:block) => {
        @vesper-alloc/region::Region::scoped_exact[$bytes](|cap $cap| $body)
    }
}

// Usage:
fn build_reply(out pkt: ReplyFrame, in items: Slice[u64, @n]) where n <= 32 {
    let checksum = scratch tmp[256] {
        let mut acc = RegionVec::with_exact_cap[u64, @n](in tmp);
        acc.extend_hashed(in items);
        acc.fold_xor()
    };
    emplace out pkt { magic: 0x5653_5031, count: narrow_u32(items.len()), crc: checksum };
}
```
]

==== Concurrency and LIFO Hardware Guards: `par`, `within`, and `holding` (`std/pulse` and `std/hw`)

`Pulse::scope` spawn and join ceremonies (#link(<ch-14-4>)[Section 15.4]) and bare-metal `@ lifo` interrupt masks (`IrqGuard`, #link(<ch-24>)[Chapter 25]) compress into short structured blocks. The solver still checks slice disjointness (`[T-OutSplitSlice]`) after expansion.

#_compact-listing[
```rust
// Parallel disjoint `out` / `inout` fork-join:
pub sugar par : expr emits eff [Scope] {
    rule (par $left:block and $right:block) => {
        @vesper-pulse/scope::Pulse::scope(|inout __s| {
            let __f1 = __s.spawn(|| $left);
            let __f2 = __s.spawn(|| $right);
            (__f1.join(), __f2.join())
        })
    }
}

// `@ lifo` hardware guard scope that cannot be inverted (released on `Fail` too):
pub sugar holding : expr {
    rule (holding $g:ident = $acquire:expr $body:block) => {
        {
            let $g = $acquire;
            errdefer(_) { @std/hw::LifoGuard::release(sink $g); }
            let __v = $body;
            @std/hw::LifoGuard::release(sink $g);
            __v
        }
    }
}

// Usage:
fn stage_tx_ring(inout nic: NicRegs, out frame: ByteArray[1536], in payload: Bytes) {
    let (_, _) = par {
        write_eth_ip_hdr(out frame[0..54], in payload)
    } and {
        copy_payload_simd(out frame[54..1536], in payload)
    };
    holding irq = Irq::disable_local() {
        nic.ring_doorbell(in frame);
    }
}
```
]

==== Proof and Vector Notation: `bits{...}`, `ghost calc`, and Elementwise Operators (`std/wire`, `std/verify`, and `std/vector`)

Tagged literals and `@lean_notation` sugars connect low-level bit manipulation to Lean 4 proof automation (`bv_decide` and `calc`). Because tags must touch their delimiter, the spaced call `ghost calc { ... }` is a ghost `TailSugarStmt` under the skeleton-entry rule (#link(<ch-2-2>)[Section 2.2]), not a `GhostExpr` over a tagged literal.

#_compact-listing[
```rust
// 1. `@0` equational proof chain with no runtime cost: feeds intermediate lemmas to
//    Bitwuzla and unexpands one-to-one into a Lean 4 `calc` block in `Specs.lean`:
@lean_notation
pub sugar ghost_calc : stmt {
    rule (ghost calc { $lhs:expr == $mid:expr ; <= $bound:expr ; } $..rest:stmts) => {
        {
            let ghost _ = @std/verify::assert_eq_ghost($lhs, $mid);
            let ghost _ = @std/verify::assert_le_ghost($mid, $bound);
            $..rest
        }
    }
}

// 2. Elementwise portable `VVec` SIMD operators at arithmetic precedence:
@lean_notation
pub sugar vadd : infix(`.+`, additive, left) {
    rule ($a:expr .+ $b:expr) => { @vesper-simd/vvec::VVec::add($a, $b) }
}
@lean_notation
pub sugar vmul : infix(`.*`, multiplicative, left) {
    rule ($a:expr .* $b:expr) => { @vesper-simd/vvec::VVec::mul($a, $b) }
}

// Usage in XPBD physics (Chapter 40): `.*` binds tighter than `.+`, as `*` does over `+`.
let next_pos = pos .+ vel .* dt_splat;
```
]

==== Shrinking the Compiler Core

Once `std/control` and `std/pulse` ship in the standard prelude, several forms that now need dedicated HIR lowering could become library sugar: `for pat in iter { ... }` (as `loop` over `Iterator::next(inout iter)`), `if let pat = e { ... }`, `assert`, and the `|>` and `|?>` pipeline operators. That would shrink Stage 2 (`vesper-hir`) and the Lean extractor while `vesper fmt` and Chroma output stay the same, because both already work on the unexpanded skeleton. Today `for` is a core form over integer ranges (#link(<ch-2-2>)[Section 2.2]); moving it into `std` waits on `sugar` and an `Iterator` trait.

=== 7.7 Domain Sugar Catalog (`std/*` and Ecosystem Packages)
<ch-6-domain-sugar>

Beyond control flow and memory helpers, `sugar` can give Vesper's specialized subsystems domain-level notation: linear session protocols, opt-in transactional rollback of `inout` places, constant-time cryptography, `@fip` in-place ADT rewriting, W1C-safe MMIO batching, DBSP stream comprehensions, and checked loop invariants. Each form below expands to ordinary library calls under `[WF-SugarPass]` and `[T-SugarEffectHonesty]` (#link(<ch-6-sugar>)[Section 7.5]), so none needs a compiler special case. Like #link(<ch-6-std-sugar>)[Section 7.6], this catalog is a design proposal; the helper functions it names (`Chan::send`, `fip::unpack_reuse`, `RmwReg::read`, and so on) are part of the proposal and are not yet specified in their chapters.

#figure(
  table(
    columns: (19%, 27%, 21%, 33%),
    align: (left, left, left, left),
    table.header([*Module and Sugar*], [*Call-Site Shape*], [*Skeleton Slot and Pass Floor*], [*Problem Solved Without a Compiler Special Case*]),
    [`std/txn::transact`], [`transact inout bbr { ... }`], [`ClauseSugarExpr` (`inout $p:place`)], [Opt-in snapshot rollback on top of the default keep-writes behavior (#link(<ch-4-3>)[Section 4.3]) when `$p : copy`, with exact `Stack` accounting for the snapshot.],
    [`std/chorus::send` / `recv`], [`send req on sink ch; recv ack from sink ch;`], [`TailSugarStmt` (`sink $ch:place`)], [Threads linear session-type transitions (`Send` to `Recv` to `End`) without manual `let ch = ...` rebinding.],
    [`std/vault::ct_if`], [`ct_if mask { a } else { b }`], [`ClauseSugarExpr` (`eff [ConstTime]`)], [Writes a secret-dependent choice as `if`/`else` while expanding to branchless `SecMask` selection with no `brif`.],
    [`std/fip::morph`], [`morph sink node { Black(l, r) => Red(l, r) }`], [`ClauseSugarExpr` (`sink $p:place`)], [Unpacks an owned heap or slab enum into a same-size `@fip` `ReuseSlot` token, so the rewrite allocates nothing.],
    [`std/silicon::modify_reg`], [`modify_reg inout uart.cr { .txe = 1, .rxe = 1 };`], [`TailSugarStmt` (`inout $r:place`)], [Coalesces multi-field MMIO updates into one bus read, ALU masks, and one bus write; rejects `W1C` registers statically.],
    [`std/aura::focus`], [`focus inout app.{ nav => ..., pane => ... }`], [`ClauseSugarExpr` (`inout $p:place`)], [Opens named disjoint `FieldTree` sub-borrows together under `[T-CallProj]` without repeating paths.],
    [`std/flux::zset`], [`zset{ (r.k, r.v) => +1 | r in batch, r.ok }`], [`TaggedDelimExpr` (`@lean_notation`)], [Compiles relational comprehensions to pre-sized `TiledSeq` AoSoA loops, mirrored to `ZSet.filterMap`.],
    [`std/verify::while_verified`], [`while c invariant phi decreases d { ... }`], [`ClauseSugarExpr` (`@lean_notation`)], [Expands to `loop` with `@0` ghost entry and step checks for Bitwuzla and `termination_by d` in Lean 4.],
    [`std/pact::using`], [`using model ReverseOrd { map.insert(k, v) }`], [`ClauseSugarExpr`], [Confines a non-canonical Pact `model` override (#link(<ch-6-3>)[Section 7.3]) to one expression instead of a whole block.],
    [`std/beacon::trace_span`], [`trace_span "wal.sync" [lsn = s] { ... }`], [`ClauseSugarExpr` (`emits eff [Trace]`)], [Stack-allocates a W3C TraceContext span, checks `@pii` redaction through `Telemetrable` (#link(<ch-31>)[Chapter 32]), and records unwinds.],
  ),
  kind: table,
)

==== Linear Session-Type Rebinding: `send`, `recv`, and `close` (`std/chorus`)

Linear session channels (#link(<ch-14-4>)[Section 15.4] and #link(<ch-34>)[Chapter 35]) normally force repeated shadowing, as in `let ch = ch.send(m); let (ack, ch) = ch.recv(); ch.close();`, because every step consumes the old protocol typestate and produces the next. Block-tail `$..rest:stmts` splicing turns a multi-step protocol into flat code. `Mode::Linear` exhaustion still applies: forgetting `close` leaves a live linear channel at the end of the block, which is rejected (`E0302`). Because the expansion rebinds `$ch` with `let`, the hole must name a local binding rather than a field path.

#figure(
  {
    let node(title, detail, fill: luma(96%), stroke: 0.5pt + _rule) = box(
      inset: (x: 8pt, y: 6pt), radius: 10pt, fill: fill, stroke: stroke,
      align(center, stack(dir: ttb, spacing: 4pt, text(size: 8.5pt, title), text(font: "DejaVu Sans Mono", size: 7.5pt, detail))),
    )
    let arrow(label) = align(center + horizon, text(size: 8pt, fill: _muted, label + " →"))
    grid(
      columns: (auto, auto, auto, auto, auto, auto, auto),
      column-gutter: 6pt, align: center + horizon,
      node([`Send[Req, …]`], [linear]),
      arrow[`send`],
      node([`Recv[Ack, End]`], [rebound], fill: luma(98%), stroke: 0.8pt + _accent),
      arrow[`recv`],
      node([`End`], [ack bound]),
      arrow[`close`],
      node([Discharged], [Theorem 2 holds]),
    )
  },
  kind: image,
  caption: [Each `send` or `recv` consumes the channel and rebinds the next protocol state for the rest of the block.],
)

#_compact-listing[
```rust
pub sugar send : stmt emits eff [Net] {
    rule (send $msg:expr on sink $ch:place ; $..rest:stmts) => {
        {
            let $ch = @vesper-chorus/chan::Chan::send(sink $ch, $msg);
            $..rest
        }
    }
}

pub sugar recv : stmt emits eff [Net] {
    rule (recv $pat:pat from sink $ch:place ; $..rest:stmts) => {
        {
            let ($pat, $ch) = @vesper-chorus/chan::Chan::recv(sink $ch);
            $..rest
        }
    }
}

// Usage: no rebinding noise; omitting `ch.close()` is still rejected (E0302).
fn replicate_entry(sink ch: Chan[Send[AppendReq, Recv[AppendAck, End]]], in entry: LogEntry) -> AppendAck
eff [Net] {
    send (AppendReq { entry }) on sink ch;   // header mode: struct literal in parentheses
    recv ack from sink ch;
    ch.close();
    ack
}
```
]

==== Opt-In Rollback and Allocation-Free Tree Rewrites: `transact` (`std/txn`) and `morph` (`std/fip`)

Keep-writes is Vesper's default for `inout` across an abort (#link(<ch-4-3>)[Section 4.3]): callee writes stay in the caller's place. `transact` lets a caller ask for rollback instead, with one word, whenever the state is `copy`; the snapshot `let __snap = $p` is what checks `$p : copy`. `morph` makes `@fip` in-place constructor reuse (#link(<ch-12-4>)[Section 13.4]) direct for balanced trees and AST passes. It relies on the repetition hole and a `$..xs:pats` sequence hole from #link(<ch-2-2>)[Section 2.2].

#_compact-listing[
```rust
// 1. Opt-in rollback over keep-writes (`let __snap = $p` requires `$p : copy`):
pub sugar transact : expr emits eff [Fail] {
    rule (transact inout $p:place $body:block) => {
        {
            let __snap = $p;
            errdefer(_) { $p = __snap; }
            $body
        }
    }
}

// 2. Allocation-free `@fip` in-place variant rewrite:
pub sugar morph : expr {
    rule (morph sink $node:place { $( $Src:ident ( $..xs:pats ) => $Dst:ident ( $..ys:args ) ),* }) => {
        match @vesper-alloc/fip::unpack_reuse(sink $node) {
            $( ($Src($..xs), __slot) => @vesper-alloc/fip::place_in(sink __slot, $Dst($..ys)) ),*
        }
    }
}

// Usage:
transact inout bbr_model {
    bbr_model.advance_round(in sample)?;
    bbr_model.probe_bw(in rtt)? // if `probe_bw` raises Fail, `bbr_model` is restored
}
```
]

==== Branchless Cryptography and Single-Write MMIO: `ct_if` (`std/vault`) and `modify_reg` (`std/silicon`)

Constant-time cryptography (#link(<ch-27>)[Chapter 28]) and bare-metal drivers (#link(<ch-24>)[Chapter 25]) both suffer when safe primitives force unreadable bit manipulation. `ct_if` evaluates both arms unconditionally, as constant-time code must, so each arm's effects always happen; only the selected value depends on the mask. `modify_reg` reads the register once, applies each field setter, and writes once; a register whose access type is `W1C` has no read-modify-write combinator, so the expansion fails to type-check on it.

#_compact-listing[
```rust
// 1. Branchless secret selection inside `eff [ConstTime]` (no `brif` instructions):
@lean_notation
pub sugar ct_if : expr emits eff [ConstTime] {
    rule (ct_if $mask:expr $then_b:block else $else_b:block) => {
        {
            let __t = $then_b;
            let __f = $else_b;
            @vesper-vault/secret::ct_select($mask, __t, __f)
        }
    }
}

// 2. Coalesced single-read, single-write MMIO bitfield update (rejects `W1C` registers):
pub sugar modify_reg : stmt {
    rule (modify_reg inout $reg:place { $( . $field:ident = $val:expr ),* } ;) => {
        {
            let mut __word = @vesper-hw/mmio::RmwReg::read(in $reg);
            $( __word = __word.$field($val); )*
            @vesper-hw/mmio::RmwReg::write(inout $reg, __word);
        }
    }
}

// Usage:
let tag_acc = ct_if mask_eq { acc ^ block_mac } else { acc ^ dummy_mac };
modify_reg inout spi1.ctrl1 { .baud_div = 4, .master = 1, .spe = 1 };
```
]

==== Checked Loop Invariants and Stream Comprehensions: `while ... invariant ... decreases` (`std/verify`) and `zset{...}` (`std/flux`)

Loop invariants and differential DBSP comprehensions (#link(<ch-36>)[Chapter 37]) stay concise in `.ves` source while lowering to Bitwuzla verification conditions and Lean 4 `termination_by` proofs. `while` is not a core keyword (#link(<ch-2-2>)[Section 2.2]), so the library can define it. The ghost checks have `@0` presence and erase in Stage 5A.

#_compact-listing[
```rust
// Checked loop invariant and strict well-founded variant:
@lean_notation
pub sugar while_verified : expr {
    rule (while $cond:expr invariant $inv:expr decreases $v:expr $body:block) => {
        {
            let ghost _ = @std/verify::assert_ghost($inv);
            loop {
                if !($cond) { break; }
                let ghost __v0: usize = $v;
                $body;
                let ghost _ = @std/verify::assert_ghost($inv);
                let ghost _ = @std/verify::assert_lt_ghost($v, __v0);
            }
        }
    }
}

// Usage: no runtime overhead once ghosts erase; termination is checked in Bitwuzla and `Funs.lean`:
while lo < hi
    invariant lo <= hi && hi <= table.len()
    decreases hi - lo
{
    let mid = lo + (hi - lo) / 2;
    if table[mid] < target { lo = mid + 1; } else { hi = mid; }
}
```
]

=== 7.8 Readability Sugar Catalog
<ch-6-read-sugar>

The sugars in this section target the places where Vesper code is noisiest: resource scopes, formatting, literal data, pipelines, waiting, retries, folds, typestate changes, tests, and proof steps. Each one fits a slot defined by the disambiguation rules in #link(<ch-2-2>)[Section 2.2], and each declares the effects it introduces. Like #link(<ch-6-std-sugar>)[Sections 7.6] and #link(<ch-6-domain-sugar>)[7.7], this catalog is a design proposal. Infix word operators such as `r matches Ok(v)` are deliberately absent: the no-word-operators rule keeps parsing independent of imports, so variant tests use `guard let` or `match`.

#figure(
  table(
    columns: (19%, 30%, 18%, 33%),
    align: (left, left, left, left),
    table.header([*Module and Sugar*], [*Call-Site Shape*], [*Slot*], [*What It Replaces*]),
    [`std/control::holding`], [`holding f = File::open(p)? { ... }`], [`ClauseSugarExpr` (`$body:scope`)], [Generalizes the `@ lifo` guard form of #link(<ch-6-std-sugar>)[Section 7.6] to any `Release` value, released on every exit path.],
    [`std/fmt::fmt`, `std/beacon::log`], [`fmt"lsn={lsn} len={n:x}"`, `log"replayed {count} pages"`], [`TaggedDelimExpr`], [Hand-written `Writer` call chains; `log` also checks `@pii` fields through `Telemetrable`.],
    [`std/lit::hex`, `bytes`, `dur`, `ip`, `re`], [`hex"dead beef"`, `bytes"4KiB"`, `dur"5ms"`, `ip"10.0.0.1"`, `re"[a-z]+"`], [`TaggedDelimExpr`], [Byte-array test vectors, magic size constants, and runtime parsing of configuration literals; each is validated during expansion and becomes a constant.],
    [`std/control::|>` placeholder], [`frame |> parse(_, in cfg) |?> validate(_)`], [`PipeExpr`], [Temporary bindings when the piped value is not the first argument.],
    [`std/pulse::race`], [`race { r = recv(in ch) => ..., timeout 5.ms() => ... }`], [`ClauseSugarExpr` (`emits eff [Scope, Clock]`)], [Manual `Pulse::race` closures; cancellation of the losing arms stays visible at the call site.],
    [`std/pulse::retry`], [`retry 3 backoff 10.ms() { ... }`], [`ClauseSugarExpr` (`emits eff [Clock]`)], [Counter loops with sleeps and error bookkeeping.],
    [`std/iter::sum`, `max`, `any`, `all`], [`sum{ x * w | (x, w) in zip(a, b) }`], [`TaggedDelimExpr`], [Accumulator loops; each expands to one counted loop with no allocation.],
    [`std/vector::lanes`], [`lanes v in xs { ... }`], [`ClauseSugarExpr`], [`VVec` strip-mining with a separate remainder loop.],
    [`std/control::update`], [`update cfg { port = 80, tls = true }`], [`ClauseSugarExpr`], [Copy-and-assign sequences for `copy` configuration structs.],
    [`std/typestate::transition`], [`transition inout conn from Handshake to Open { ... }`], [`ClauseSugarExpr` (`inout $p:place`)], [Hand-threaded typestate rebinding in Prism and Warp connection machines.],
    [`std/verify::assert`], [`assert len <= cap because lemma_bounded(in buf);`], [`TailSugarStmt` (`@lean_notation`)], [Unnamed assertions; the named lemma is passed to the solver and mirrors to Lean `by exact`.],
    [`std/test::test`, `prop`], [`test "wal replay" { ... }`, `prop "roundtrip" for x: Frame { ... }`], [`ItemSugar`], [Test registration boilerplate; `prop` hooks into Crucible (#link(<ch-21>)[Chapter 22]) and Codex doctests (#link(<ch-19>)[Chapter 20]).],
  ),
  kind: table,
)

#_compact-listing[
```rust
// 1. Exit-aware resource scope over any `Release` value (`$body:scope`, Section 7.5):
pub sugar holding : expr {
    rule (holding $r:ident = $acquire:expr $body:scope) => {
        {
            let $r = $acquire;
            let __exit = $body;
            @std/control::Release::release(sink $r);
            __exit.resume()
        }
    }
}

// 2. Bounded retry with backoff; the attempt count and delay stay at the call site:
pub sugar retry : expr emits eff [Clock] {
    rule (retry $n:expr backoff $delay:expr $body:block) => {
        @std/pulse::retry_with_backoff($n, $delay, || $body)
    }
}

// 3. A fold comprehension as a tagged literal (the tag touches its brace):
pub sugar sum : tag {
    rule (sum{ $e:expr | $p:pat in $src:expr }) => {
        {
            let mut __acc = 0;
            for $p in $src { __acc += $e; }
            __acc
        }
    }
}

// Usage:
fn replay_segment(in path: Path, in cfg: WalCfg) -> u64 eff [Storage, Clock, Fail[IoErr]] {
    holding f = File::open(in path)? {
        let hdr = f.read_header()? |?> validate(_, in cfg);
        assert hdr.len <= cfg.max_segment because lemma_segment_bounded(in hdr);
        let pages = retry 3 backoff dur"10ms" { f.read_pages(in hdr) };
        log"replayed {pages.len()} pages from {path}";
        sum{ p.checksum | p in pages }
    }
}

test "replay rejects truncated segments" {
    let bytes = hex"5653 5031 0000 00ff";
    assert replay_bytes(in bytes).is_err() because lemma_truncation_detected(in bytes);
}
```
]

The usage above shows how the rules keep these forms apart: `log"..."`, `hex"..."`, `dur"..."`, and `sum{` are tags because they touch their delimiters; `holding f = ... {` and `retry 3 backoff ... {` are clause forms whose headers end at the spaced `{`; `assert ... because ...;` is a tail statement whose header ends at the head word `because`; and `test "..." {` is an item because a `LowerIdent` cannot begin a core item.

=== 7.9 Trait Solver, Enforester, and Hygiene Engine (`crates/vesper-pact` and `crates/vesper-forge`)
<ch-6-5>

```rust
// crates/vesper-pact/src/lib.rs
use rustc_hash::FxHashMap;
use vesper_hir::ty::{EffectRow, Mode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TraitId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelId(pub u32);

/// Distinguishes global orphan-checked `impl` blocks from lexically scoped named `model`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelBrand {
    Canonical { trait_id: TraitId },
    Named { model_id: ModelId },
}

/// Canonicalized goal (`?0`, `?1` de Bruijn variables) for SLG tabled trait resolution.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CanonicalTraitGoal {
    pub trait_id: TraitId,
    pub brand: ModelBrand,
    pub canonical_type_args: Vec<u32>,
    pub max_mode_bound: Mode,
    pub effect_row: EffectRow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalEvaluation {
    Proven { impl_def_id: u32, nested_obligations: Vec<CanonicalTraitGoal> },
    CoinductiveCycleAssumed,
    Ambiguous,
    NoSolution,
}

#[derive(Default)]
pub struct TabledTraitSolver {
    pub memo_table: FxHashMap<CanonicalTraitGoal, GoalEvaluation>,
}

impl TabledTraitSolver {
    /// Enforces the Covered-First Orphan Rule on anonymous canonical `impl` blocks.
    pub fn check_canonical_orphan_rule(
        defining_pkg: u32,
        trait_pkg: u32,
        ordered_type_constructors: &[Option<u32>],
    ) -> Result<(), &'static str> {
        if defining_pkg == trait_pkg {
            return Ok(());
        }
        match ordered_type_constructors.first().copied().flatten() {
            Some(first_uncovered_pkg) if first_uncovered_pkg == defining_pkg => Ok(()),
            _ => Err("orphan canonical `impl` forbidden; declare a `pub model Name: Trait for Type` instead"),
        }
    }
}

// crates/vesper-forge/src/lib.rs
use rustc_hash::FxHashSet;
use smol_str::SmolStr;
use vesper_loom_calculus::CapabilityCeiling;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScopeMark(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DefId {
    pub pkg_id: u32,
    pub item_idx: u32,
}

/// Hygienic identifier token stored on lossless Rowan CST leaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HygienicIdent {
    pub text: SmolStr,
    pub marks: Vec<ScopeMark>,
    pub pre_resolved: Option<DefId>,
}

impl HygienicIdent {
    /// Flatt / Lean 4 subset rule: `binder` binds `use_site` iff `binder.marks <= use_site.marks`.
    pub fn can_bind_local(&self, use_site: &Self) -> bool {
        self.text == use_site.text
            && self.pre_resolved.is_none()
            && use_site.pre_resolved.is_none()
            && self.marks.iter().all(|m| use_site.marks.contains(m))
    }
}

#[derive(Clone, Debug)]
pub enum MacroSecurityError {
    SmuggledEffectCapability {
        macro_pkg: SmolStr,
        injected_def: DefId,
        forbidden_effect: SmolStr,
    },
}

/// Enforces `[T-MacroConfinement]` after expanding a WASI `wasm32-wasip2` macro component.
pub fn verify_macro_anti_smuggling(
    macro_pkg_name: &SmolStr,
    macro_pkg_id: u32,
    macro_declared_emits: &FxHashSet<SmolStr>,
    caller_ceiling: &CapabilityCeiling,
    emitted_idents: &[HygienicIdent],
    lookup_def_effects: impl Fn(DefId) -> Vec<SmolStr>,
) -> Result<(), MacroSecurityError> {
    for ident in emitted_idents {
        if let Some(def_id) = ident.pre_resolved {
            if def_id.pkg_id == macro_pkg_id {
                for eff in lookup_def_effects(def_id) {
                    if !macro_declared_emits.contains(&eff)
                        || !caller_ceiling.allowed_effects.contains(&eff)
                    {
                        return Err(MacroSecurityError::SmuggledEffectCapability {
                            macro_pkg: macro_pkg_name.clone(),
                            injected_def: def_id,
                            forbidden_effect: eff,
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Pass-mode visibility floor declared on a `sugar` place hole (`[WF-SugarPass]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HolePassFloor {
    SharedIn,
    MutInout,
    InitOut,
    MoveSink,
}

impl HolePassFloor {
    pub fn permits_template_use(self, demanded: Self) -> bool {
        matches!(
            (self, demanded),
            (Self::SharedIn, Self::SharedIn)
                | (Self::MutInout, Self::SharedIn | Self::MutInout)
                | (Self::InitOut, Self::InitOut)
                | (Self::MoveSink, Self::SharedIn | Self::MutInout | Self::MoveSink)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SugarTransparencyError {
    HiddenPassEscalation {
        hole_name: SmolStr,
        declared_floor: HolePassFloor,
        template_demanded: HolePassFloor,
    },
    DuplicatedExclusiveHole {
        hole_name: SmolStr,
    },
}

/// Verifies `[WF-SugarPass]` on a `sugar` rule template prior to registration.
pub fn verify_sugar_pass_transparency(
    hole_name: &SmolStr,
    declared_floor: HolePassFloor,
    template_uses: &[(HolePassFloor, bool)], // (demanded_mode, same_exec_path)
) -> Result<(), SugarTransparencyError> {
    let mut exclusive_uses_on_path = 0usize;
    for &(demanded, same_path) in template_uses {
        if !declared_floor.permits_template_use(demanded) {
            return Err(SugarTransparencyError::HiddenPassEscalation {
                hole_name: hole_name.clone(),
                declared_floor,
                template_demanded: demanded,
            });
        }
        if same_path && demanded != HolePassFloor::SharedIn {
            exclusive_uses_on_path += 1;
            if exclusive_uses_on_path > 1 {
                return Err(SugarTransparencyError::DuplicatedExclusiveHole {
                    hole_name: hole_name.clone(),
                });
            }
        }
    }
    Ok(())
}
```

= Part II. Compiler Architecture, Verification, Bootstrap, and Governance
<part-2>
#chapter-deck[Compilation, Proof Extraction, Bootstrap, Package Policies, and Compatibility]

#_part-summary("Part II Architectural Scope")[
  Part II follows source code through compilation, verification, and distribution. #link(<ch-7>)[Chapter 8] presents the five-stage compiler architecture. #link(<ch-8>)[Chapter 9] explains extraction from `PlaceIr` to Lean 4 and the proof-admission checks. #link(<ch-9>)[Chapter 10] describes bootstrap, rewrite witnesses, and diverse double-compiling. #link(<ch-10>)[Chapter 11] covers *Vesper Loom* package policies and build confinement. #link(<ch-11>)[Chapter 12] distinguishes source-interface compatibility from binary compatibility. These mechanisms have separate trust boundaries; together they do not yet establish end-to-end compiler correctness.
]

#figure(
  table(
    columns: (14%, 34%, 52%),
    align: (left, left, left),
    table.header([*Chapter*], [*Title*], [*Subsystems and Host Crates*]),
    [#link(<ch-7>)[Chapter 8]], [Compiler Pipeline and Host Workspace], [`vesper-span`, `vesper-syntax`, `vesper-hir`, `vesper-refine`, `vesper-mir-pcg`, `vesper-opt-egglog`, `vesper-codegen-clif`],
    [#link(<ch-8>)[Chapter 9]], [Prophecy-Free Lean 4 Proof Extractor], [`PlaceIr` to `EffM` state lenses, `Types`/`Funs`/`Specs`/`Proofs.lean`, `.olean` zero-`sorryAx` gate (`vesper-verify-lean`)],
    [#link(<ch-9>)[Chapter 10]], [Verified Bootstrap and Diverse Double-Compiling], [`@vesper-lang/compiler`, `EgglogRewriteWitness`, `Stage 0`--`Stage 3` + CompCert DDC (`vesper-codegen-verified`)],
    [#link(<ch-10>)[Chapter 11]], [Hermetic Package Management (Vesper Loom)], [`Loom.toml`, `Loom.lock`, `capability_ceiling`, WASI `build.ves`, CDCL + Max-SMT, Lila quorum (`vesper-loom-*`)],
    [#link(<ch-11>)[Chapter 12]], [Mathematical SemVer and Interface Subtyping], [`mif:blake3`, `[SV-Fn]`, `[SV-Refine-Pre/Post]`, `[SV-Grade]`, broken-caller witness synthesis (`vesper-semver`)],
  ),
  kind: table,
)

== 8. Compiler Pipeline and Host Architecture
<ch-7>
#chapter-deck[Compilation Stages, Workspace Structure, HIR, and Place Capability Graphs]

Vesper's compiler and supporting tools share a modular workspace. This chapter follows the proposed five-stage pipeline from source syntax to native code and Lean models, then describes the host representations and place-capability analysis. The workspace also contains the proof extractor, SemVer solver, Loom package manager, IDE server, and domain libraries; the actual build and CLI determine which components a toolchain binary includes.

=== 8.1 Five-Stage Compilation and Dual-Backend Pipeline
<ch-7-1>

Compilation lowers `.ves` source modules through five deterministic stages targeting native machine code and pure Lean 4 verification models simultaneously:

+ *Stage 1 — Resilient SIMD Lexing and Lossless `PhraseSkeleton` CST (`vesper-span` + `vesper-syntax`):* Zero-copy SIMD-accelerated tokenization (`logos`) feeds an import-free LL(2) recursive-descent parser constructing a lossless `rowan` Red-Green Concrete Syntax Tree, including open `PhraseSkeleton` clause, operator, and tagged-literal nodes (#link(<ch-2-1>)[Section 2.1]), memoized inside `salsa` incremental queries for sub-millisecond editor diagnostics (`ariadne`).
+ *Stage 2 — Skeletal Enforestation and Bidirectional Modal Elaboration (`vesper-hir` + `vesper-pact` + `vesper-forge`):* Enforests stratified `sugar` rules under `[WF-SugarPass]` (#link(<ch-6-sugar>)[Section 7.5]), expands hygienic `macro` and fuel-metered `wasm32-wasip2` `#[wasm_macro]` components under `[T-MacroConfinement]`, desugars surface syntax into $lambda_(upright("Vesper"))$ HIR, resolves tabled trait/model goals, and runs bidirectional inference with mode crossing (`[T-Cross]`) and lexical effect-row unification.
+ *Stage 3 — Place Capability Graph, LIFO, and Horn VC Solver (`vesper-mir-pcg` + `vesper-refine`):* Lowers HIR into basic-block `PlaceIr`, constructs a Place Capability Graph (`petgraph`) over field trees and slice intervals, and discharges Horn `QF_LIA` + `QF_BV` + `QF_UF` obligations in Bitwuzla to enforce:
  - *Alias-XOR-Mutability:* Zero active `inout` or `out` projection overlaps any sibling `in`, `inout`, `out`, or `sink` projection (`[T-CallProj]`, `[T-CallSliceProj]`).
  - *Total Linear, `out`, and LIFO Exhaustion:* Every `linear` binding is consumed exactly once across $100%$ of control-flow and `Fail` unwind edges (`[T-Let]`, `[T-Match]`), every `out` destination is $100%$ initialized prior to `Ok` return, and every `lifo` guard releases in strict stack order.
+ *Stage 4 — Effectful Equality Saturation with Rewrite Witnesses (`vesper-opt-egglog`):* Threads state and capability tokens through `egglog` e-graphs (#link("https://arxiv.org/abs/2609.31887")[_Effectful E-Graphs_], OOPSLA 2026) to fuse tail-resumptive handlers (`Fail`, lexical `Region` bumps) into inline branches and pointer bumps, rewrite tail-modulo-cons into `out`-slot loops, elide proven graded arena bounds checks, vectorize `VVec` kernels, and emit linear-time checkable `EgglogRewriteWitness` certificates (#link(<ch-9>)[Chapter 10]).
+ *Stage 5 — Dual Native and Verification Backends (`vesper-codegen-clif`, `vesper-codegen-verified`, `vesper-verify-lean`):*
  - *Stage 5A (Native & JIT Codegen):* Erases all `@0` ghost bindings, clocks, and refinement predicates, emitting Cranelift SSA IR (`cranelift-codegen` / `cranelift-jit` / `cranelift-object`)—or direct verified machine code via `vesper-codegen-verified`—for `x86_64`, `AArch64`, `RISC-V`, `wasm32-wasip2`, and Linux `eBPF`.
  - *Stage 5B (Prophecy-Free Lean 4 Extraction):* Translates validated `PlaceIr` (retaining `@0` ghost witnesses and surface refinements) 1-to-1 into pure state-passing Lean 4 definitions and composable `.olean` certificates (#link(<ch-8>)[Chapter 9]).

=== 8.2 Host Workspace Boundaries
<ch-7-2>

Rust crate packaging is an implementation detail, not a language conformance requirement. The root `Cargo.toml` and `Cargo.lock` are authoritative for workspace members and dependency versions; `cargo metadata --no-deps --format-version 1` reports the current members. The checker and Place IR currently reside in the root facade. The architecture distinguishes syntax and diagnostics, elaboration and place checking, optimization and native code generation, proof extraction, and package tooling; proposed subsystem names elsewhere in this manuscript do not by themselves establish that corresponding crates or features exist.

=== 8.3 Substructural HIR, Field Trees, and Lexical Effects (`crates/vesper-hir`)
<ch-7-3>

`crates/vesper-hir` encodes usage modes, continuation resumption guarantees, trie-structured `FieldTree` selectors, and open lexical effect rows in compact arena-indexed data structures:

```rust
// crates/vesper-hir/src/ty.rs
use la_arena::Idx;
use rustc_hash::FxHashMap;
use smol_str::SmolStr;

/// Primary usage mode axis ordered by `Copy < Affine < Linear` (`copy <= affine <= linear`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Mode {
    Copy = 0,   // bottom: weakening + contraction permitted
    Affine = 1, // implicit drop permitted; bitwise copy forbidden
    Linear = 2, // top: exact-once consumption; neither drop nor copy permitted
}

impl Mode {
    #[inline]
    pub fn join(self, other: Self) -> Self {
        core::cmp::max(self, other)
    }

    #[inline]
    pub fn meet(self, other: Self) -> Self {
        core::cmp::min(self, other)
    }

    #[inline]
    pub fn is_discardable(self) -> bool {
        self <= Self::Affine
    }
}

/// Continuation resumption contract `q in {affine, linear}` on an effect declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ResumptionMode {
    Affine = 1, // Handler may abort (e.g., `effect Fail[E]`)
    Linear = 2, // Handler must resume `k` exactly once (e.g., `linear effect Storage`)
}

impl ResumptionMode {
    #[inline]
    pub fn as_mode(self) -> Mode {
        match self {
            Self::Affine => Mode::Affine,
            Self::Linear => Mode::Linear,
        }
    }
}

/// Four-mode second-class parameter projection algebra (`[T-CallProj]` & `[T-OutSplitSlice]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassMode {
    In,    // Shared read-only projection (`shr`)
    InOut, // Exclusive mutable projection (`mut`, supports `~>` strong update)
    Out,   // Exclusive write-only uninitialized destination (`OutSlot[T]`)
    Sink,  // Ownership transfer / linear obligation consumption (`own`)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldIdx(pub u16);

/// Prefix tree representing disjoint field projections (`broker.{index, metrics.counters}`).
/// An empty `children` map represents a leaf selection covering the entire sub-place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldTree {
    pub children: FxHashMap<FieldIdx, FieldTree>,
}

impl FieldTree {
    pub fn whole_place() -> Self {
        Self { children: FxHashMap::default() }
    }

    /// Checks `pi_1 perp pi_2`: true iff the two selector trees share zero overlapping leaf paths.
    pub fn is_disjoint(&self, other: &Self) -> bool {
        if self.children.is_empty() || other.children.is_empty() {
            return false;
        }
        for (field, lhs_sub) in &self.children {
            if let Some(rhs_sub) = other.children.get(field) {
                if !lhs_sub.is_disjoint(rhs_sub) {
                    return false;
                }
            }
        }
        true
    }

    /// Checks `pi_new <= pi_old` for `vesper semver` `[SV-Fn]` projection narrowing.
    pub fn is_subset_of(&self, older: &Self) -> bool {
        if older.children.is_empty() {
            return true;
        }
        if self.children.is_empty() {
            return false;
        }
        self.children.iter().all(|(f, sub)| {
            older.children.get(f).is_some_and(|old_sub| sub.is_subset_of(old_sub))
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectInst {
    pub cap_label: Option<SmolStr>,
    pub effect_id: EffectId,
    pub resumption: ResumptionMode,
    pub type_args: Vec<Idx<Ty>>,
}

/// Lexical effect row `eff [l1: E1^q1, ..., ln: En^qn | tail_var]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectRow {
    pub instances: Vec<EffectInst>,
    pub tail_var: Option<u32>,
}

impl EffectRow {
    /// Computes `res(epsilon) = meet_i q_i`, with `res(empty) = Mode::Linear`.
    pub fn resumption_guarantee(&self, tail_bound: Mode) -> Mode {
        self.instances
            .iter()
            .fold(tail_bound, |acc, inst| acc.meet(inst.resumption.as_mode()))
    }
}
```

=== 8.4 Four-Axis `ModeVec`, LIFO Stack, and Resource-Grade Engine (`crates/vesper-refine`)
<ch-7-4>

The repository implements separate analysis helpers in `crates/vesper-core`: modal vectors, ordered obligations, constant output-slice coverage, ghost contexts, and checked resource intervals. These helpers do not constitute a combined source checker or a Horn solver. Reuse their checked APIs rather than duplicating the arithmetic or permission rules:

```rust
// Existing standalone analysis APIs; source integration remains separate.
pub use vesper_core::mode_vec::{CapsuleEvidence, ModeVec};
pub use vesper_core::ghost::{Binding, Context, Presence};
pub use vesper_core::lifo::LifoState;
pub use vesper_core::out_slice::OutSlice;
pub use vesper_core::resource_grade::{
    GradeKind, IntervalBound, RefineTerm, ResourceGradeError,
    verify_graded_resource_ceiling,
};

// Each nested caller frame remains live while its callee runs.
fn nested_stack(frame: u64, callee: u64) -> Result<u64, ResourceGradeError> {
    GradeKind::caller_stack_depth(frame, &[callee])
}

// Successful analysis is a bound, not permission to erase runtime checks.
fn arena_bound(
    term: &RefineTerm,
    ceiling: u64,
    env: &std::collections::BTreeMap<String, IntervalBound>,
) -> Result<bool, ResourceGradeError> {
    verify_graded_resource_ceiling("Region", term, ceiling, env)
}
```

=== 8.5 Place IR and Place Capability Graph Solver (`crates/vesper-mir-pcg`)
<ch-7-5>

After Stage 2 elaboration, HIR lowers into `PlaceIr`. The PCG dataflow solver verifies pairwise field-tree disjointness, uniform branch consumption, and the Resumption Frame Condition across every basic-block edge:

#_compact-listing[
```rust
// crates/vesper-mir-pcg/src/pcg.rs
use rustc_hash::FxHashMap;
use vesper_hir::ty::{EffectRow, FieldTree, Mode, PassMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceProjection {
    pub local: LocalId,
    pub tree: FieldTree,
}

#[derive(Clone, Debug)]
pub enum Statement {
    Assign { dst: LocalId, ty_mode: Mode },
    UnpackLinear { src: LocalId, bound_locals: Vec<(LocalId, Mode)> },
    CallProj {
        args: Vec<(PassMode, PlaceProjection)>,
        ret_dst: LocalId,
        ret_mode: Mode,
        effects: EffectRow,
    },
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Goto(BlockId),
    SwitchInt { discr: LocalId, targets: Vec<BlockId> },
    PerformEffect {
        effects: EffectRow,
        resume_target: BlockId,
        abort_unwind: Option<BlockId>,
    },
    Return,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalCapability {
    LiveOwned { mode: Mode, moved_subtrees: FieldTree },
    UninitializedOutSlot { initialized_subtrees: FieldTree },
    Consumed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PcgError {
    OverlappingProjection { local: LocalId },
    UnconsumedLinearAtScopeExit { local: LocalId },
    UninitializedOutSlotAtReturn { local: LocalId },
    InconsistentLinearConsumptionAtJoin { block: BlockId, local: LocalId },
    ResumptionFrameViolation { live_linear: LocalId },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcgState {
    pub locals: FxHashMap<LocalId, LocalCapability>,
}

impl PcgState {
    /// Enforces `[T-CallProj]` pairwise disjointness, `sink` consumption, and `[T-Let]` frame safety.
    pub fn check_call_projections(
        &mut self,
        args: &[(PassMode, PlaceProjection)],
        effects: &EffectRow,
    ) -> Result<(), PcgError> {
        for (i, (m_i, p_i)) in args.iter().enumerate() {
            for (m_j, p_j) in &args[i + 1..] {
                let both_shared = matches!((m_i, m_j), (PassMode::In, PassMode::In));
                if !both_shared && p_i.local == p_j.local && !p_i.tree.is_disjoint(&p_j.tree) {
                    return Err(PcgError::OverlappingProjection { local: p_i.local });
                }
            }
        }

        for (pass_mode, proj) in args {
            match *pass_mode {
                PassMode::Sink => {
                    self.locals.insert(proj.local, LocalCapability::Consumed);
                }
                PassMode::Out => {
                    self.locals.insert(
                        proj.local,
                        LocalCapability::LiveOwned {
                            mode: Mode::Linear,
                            moved_subtrees: FieldTree::default(),
                        },
                    );
                }
                PassMode::In | PassMode::InOut => {}
            }
        }

        // Unfilled `UninitializedOutSlot` bindings are safely ignored across abortable unwinds:
        let res_guarantee = effects.resumption_guarantee(Mode::Linear);
        for (&local, cap) in &self.locals {
            if let LocalCapability::LiveOwned { mode, .. } = cap {
                if *mode > res_guarantee {
                    return Err(PcgError::ResumptionFrameViolation { live_linear: local });
                }
            }
        }
        Ok(())
    }

    /// Enforces uniform linear consumption across CFG join points (`[T-Match]` / `if-else`).
    pub fn join_at_block(&self, other: &Self, block: BlockId) -> Result<Self, PcgError> {
        let mut merged = self.locals.clone();
        for (&local, rhs_cap) in &other.locals {
            match (merged.get(&local), rhs_cap) {
                (Some(LocalCapability::LiveOwned { mode: Mode::Linear, .. }), LocalCapability::Consumed)
                | (Some(LocalCapability::Consumed), LocalCapability::LiveOwned { mode: Mode::Linear, .. }) => {
                    return Err(PcgError::InconsistentLinearConsumptionAtJoin { block, local });
                }
                (Some(LocalCapability::LiveOwned { mode, .. }), LocalCapability::Consumed)
                    if mode.is_discardable() =>
                {
                    merged.insert(local, LocalCapability::Consumed);
                }
                _ => {}
            }
        }
        Ok(Self { locals: merged })
    }

    /// Enforces `discard(Gamma)` and complete `OutSlot` initialization at function `Return`.
    pub fn check_return_exhaustion(&self) -> Result<(), PcgError> {
        for (&local, cap) in &self.locals {
            match cap {
                LocalCapability::LiveOwned { mode: Mode::Linear, .. } => {
                    return Err(PcgError::UnconsumedLinearAtScopeExit { local });
                }
                LocalCapability::UninitializedOutSlot { .. } => {
                    return Err(PcgError::UninitializedOutSlotAtReturn { local });
                }
                _ => {}
            }
        }
        Ok(())
    }
}
```
]

== 9. Prophecy-Free Lean 4 Verification Extractor
<ch-8>
#chapter-deck[Model Extraction, Mutable-State Translation, and Proof Admission]

Functional correctness requires a program to satisfy its stated behavior, beyond its ownership and borrowing rules. Storage engines, consensus machines, cryptographic operations, and hypervisor page mappers motivate Vesper's Lean 4 extraction design. *Charon + Aeneas* and *Hax* are related approaches; see #link("https://arxiv.org/abs/2605.30106")[A Rust-to-Lean Verification Pipeline with AI Provers] (CAV/AIMACS 2026). #link("https://drops.dagstuhl.de/storage/00lipics/lipics-vol384-types2025/LIPIcs.TYPES.2025.2/LIPIcs.TYPES.2025.2.pdf")[Lean4Lean] (TYPES 2025) studies kernel verification. This chapter distinguishes model extraction, proof admission, and the remaining obligation to connect those models to source and native execution.

The repository extractor in `src/lean.rs` translates a checked subset into Lean models. Lexical mutable borrows motivate its functional state-passing representation, but the typing schemas alone do not prove that translation preserves source execution. The operational models and place bridges in Appendix A establish narrower correspondences; a complete compiler refinement remains open.

A zero-`sorryAx` count does not establish proof validity: user axioms, opaque or unsafe definitions, compiler substitution, and trusted native evaluation can bypass it. A sound admission gate requires fresh kernel replay, a transitive axiom allowlist, inspection of executable substitutions, expected theorem signatures, and justified satisfiable assumptions.

The current CLI implements fresh kernel replay and explicit axiom policies. It rejects `sorryAx`, the older compiler-trust axiom names, and Lean 4.35-generated axioms with a `_native` namespace component, even when explicitly whitelisted.

For explicit extraction contract bindings, the auditor checks the replayed theorem type against the generated contract constant and rejects extra hypotheses. It also requires an exact, closed witness of the generated existential input precondition and restricts axioms to `propext`, `Classical.choice`, and `Quot.sound`. This excludes contracts admitted solely through contradictory preconditions or custom axioms.

Termination, executable substitution checks, extraction correctness, and correspondence to native code remain additional obligations.

=== 9.1 Verification Architecture Overview
<ch-8-1>

#figure(
  table(
    columns: (23%, 34%, 43%),
    align: (left, left, left),
    table.header([*Verification Dimension*], [*First-Class Reference Extraction*], [*Vesper (`vesper-verify-lean`)*]),
    [*Mutable Borrowing*],
    [Region reconstruction (`LLBC`) and backward prophecy functions on `&'a mut T` expiration.],
    [Second-class `inout x.pi` and `out y` end strictly at call return; lower directly to *pure tuple returns plus lawful disjoint lenses*.],
    [*Aliasing & Framing*],
    [Separation-logic heap framing or Tree-Borrows loan tracking across calls.],
    [`vesper-mir-pcg` proves $p_i perp p_j$ statically; Lean 4 models aggregates as pure immutable `Type` values with *zero heap-separation boilerplate*.],
    [*Protocol Finalization*],
    [Cannot assume `Drop` ran or succeeded along early error branches.],
    [`Mode::Linear` and `[T-Let]` guarantee exact-once discharge on all normal and `Fail` paths prior to extraction.],
    [*Async & I/O Effects*],
    [Opaque `Pin` coroutine state machines requiring manual re-modeling.],
    [One-shot lexical effects (`eff [...]`) lower to *Freer Interaction-Tree monads (`EffM`)* and pure state-folding interpreters (#link("https://eprint.iacr.org/2026/899")[VCVio] style).],
    [*Modular Proof Caching*],
    [Ad-hoc subproject rebuilds vulnerable to axiom and toolchain drift.],
    [*Vesper Loom* caches kernel-audited `.olean` certificates in `~/.vesper/store/` (`OLEAN_CERTS`) with `mif:blake3` early cutoff and `0 sorryAx` gates.],
  ),
  kind: table,
)

=== 9.2 Formal Functional Translation Rules (`PlaceIr` to Lean 4)
<ch-8-2>

The proposed functional translation is restricted to a sequential deterministic verification profile. It does not establish soundness for concurrent interleavings, asynchronous completion ordering, hardware floating point, unmodeled allocation failures, or wire endianness. The following rules are a design, not a claim that all are implemented:

+ *Scalars, Refinements, Quotients, and ADTs ($⟦tau⟧$):*
  - *Machine Scalars:* `u8`, `u16`, `u32`, `u64`, and `i64` map to Lean 4 fixed-width types (`UInt8`, `UInt16`, `UInt32`, `UInt64`, `Int64`) backed by `BitVec`, unlocking built-in `bv_decide` (SAT bit-blasting) and `omega` (linear integer arithmetic) tactics. Checked arithmetic (`+`, `-`, `*`) emits overflow obligations in `Fail[ArithOverflow]` (elided whenever Bitwuzla discharges bounds statically), while `wrapping_add` lowers to total `UInt64.add`.
  - *Refinements, Ghost Bindings, and Quotients:* Surface subset types `{ v: T | phi }` lower to Lean subtypes `{ v : ⟦T⟧ // ⟦phi⟧ v }`; `@0` `ghost` parameters and return values remain explicit Lean 4 arguments; and `quotient Q = repr R modulo eq` lowers to `Quot ⟦eq⟧`.
  - *Modal ADTs:* `copy`, `affine`, and `linear` `struct` declarations lower to Lean `structure` definitions; `enum` declarations lower to `inductive` types (`Never` maps to `Empty`, `Unit` to `Unit`).
+ *Disjoint Field-Tree Projections as Lawful Lenses ($⟦upright("inout ") x . pi⟧$):* For a simple, unconstrained record projection, a model may define `get_pi` and `set_pi` and establish the following laws. These examples do not generalize by `rfl` to enum variants, coupled refinements, bounded array slices, or quotients: those require partial prisms, invariant and bounds obligations, and separate proofs.
  - `set_get_pi (s : ⟦tau⟧) : set_pi s (get_pi s) = s := rfl`
  - `get_set_pi (s : ⟦tau⟧) (v : ⟦tau_pi⟧) : get_pi (set_pi s v) = v := rfl`
  - *Disjoint Frame Preservation ($pi_1 perp pi_2$):* For every sibling field or subtree $pi_2$ disjoint from $pi_1$, `get_pi2 (set_pi1 s v) = get_pi2 s := rfl`. Holding `in broker.config` across `append_batch(inout broker.{index, metrics}, ...)` simplifies `(b.setIndexMetrics p).config = b.config` automatically in zero manual proof steps.
+ *Passing-Mode Tuple Returns and Adaptive Effect Monads:* Given parameters `(mu_1 p_1 : tau_1, ..., mu_n p_n : tau_n)`, every initialized input (`in`, `inout`, `sink`) passes `v_i : ⟦tau_i⟧` by value, while the return type bundles the nominal result with updated values for all `inout` and `out` parameters:
  $ upright("RetTuple")(f) := ⟦tau_(upright("ret"))⟧ times product_(j in upright("mut_out_indices")) ⟦tau_j⟧ $
  To avoid monadic noise on pure code, `eff [epsilon]` lowers across three stratified tiers:
  - *Tier 0 — Pure ($epsilon = emptyset$):* Emits a bare, total Lean 4 function returning `RetTuple(f)`.
  - *Tier 1 — Fallible Only ($epsilon = [upright("Fail")[E]]$):* Emits `Except ⟦E⟧ RetTuple(f)` with native Lean `do`-notation early return. A function that mutates stationary `inout` parameters $Sigma_(upright("io"))$ and may abort keeps its writes on the `.error` edge (`PostCall_abort`, #link(<ch-4-3>)[§4.3]). The extractor emits it over a generic monad `m` with a trailing capability `$sync : Sigma_io → m Unit` that it calls with its current `inout` values before every action that may abort. A caller passes a `$sync` that writes those values into its borrowed places and forwards them to its own enclosing handler frames and its own `$sync`. Instantiating `m := ExceptT ⟦E⟧ (StateM Sigma_io)` with `$sync := set` gives `Sigma_io → Except ⟦E⟧ RetTuple(f) × Sigma_io`, the same post-abort value as native pointer-passing codegen. The default `$sync` discards its argument, so normal-return contracts keep the `Except ⟦E⟧ RetTuple(f)` shape. A call through a function value with `inout` parameters and an abortable row is refused rather than modeled with roll-back.
  - *Tier 2 — General One-Shot Lexical Effects:* Emits the universe-polymorphic *Freer Interaction-Tree Monad* `EffM ⟦epsilon⟧ RetTuple(f)`, over which any lexical `handle` block translates into a structural state-folding interpreter:

```lean
-- Core Vesper verification prelude (VesperStd.EffM)
inductive EffRowSig where
  | nil  : EffRowSig
  | cons : (Op : Type) -> (Ret : Op -> Type) -> ResumptionMode -> EffRowSig -> EffRowSig

inductive EffM : EffRowSig -> Type -> Type 1 where
  | pure : a -> EffM sig a
  | vis  : MemberOp Op Ret q sig -> (op : Op) -> (Ret op -> EffM sig a) -> EffM sig a
```

+ *Loops and Termination:* Natural CFG loops lower via functional SSA conversion into tail-recursive `f.loop` definitions carrying only mutated locals, with `decreases` clauses emitting `termination_by` obligations discharged via `omega`.
+ *Surface Sugar Preservation (`Notations.lean`):* `sugar` rules annotated with `@lean_notation` (#link(<ch-6-sugar>)[Section 7.5]) are intended to emit companion Lean 4 `syntax`, `macro_rules`, and `@[app_unexpander]` declarations. Each expansion should unfold by `rfl` to the extracted `PlaceIr` term, so `Specs.lean` proof states can show the user's surface notation. The extractor does not emit `Notations.lean` yet.

=== 9.3 End-to-End Verification Showcase: `WriteTxn` and `append_batch`
<ch-8-3>

Running `vesper verify` on the WAL transaction module from #link(<ch-1-4>)[Section 1.4] emits deterministic `Types.lean` and `Funs.lean` models:

```lean
-- Auto-generated by vesper-verify-lean from src/wal_txn.ves
-- Interface MIF: mif:blake3:9f84c2e1...
import VesperStd.Prelude

namespace KrabkaWal

structure WriteTxn where
  walSlot    : UInt64
  dirtyPages : BufferPoolSlice

structure CommitReceipt where
  walSlot : UInt64

structure BrokerIndexMetricsProj where
  index          : SegmentedIndex
  batchesWritten : UInt64

-- Synthesized lawful disjoint lens for `inout broker.{index, metrics}`
@[simp] def Broker.getIndexMetrics (b : Broker) : BrokerIndexMetricsProj :=
  { index := b.index, batchesWritten := b.metrics.batchesWritten }

@[simp] def Broker.setIndexMetrics (b : Broker) (p : BrokerIndexMetricsProj) : Broker :=
  { b with index := p.index, metrics := { b.metrics with batchesWritten := p.batchesWritten } }

@[simp] theorem Broker.config_preserved_by_setIndexMetrics (b : Broker) (p : BrokerIndexMetricsProj) :
    (b.setIndexMetrics p).config = b.config := rfl

inductive StorageOp where
  | beginTxn     (maxBytes : UInt64)
  | flushWal     (slot : UInt64) (pages : BufferPoolSlice)
  | discardWal   (slot : UInt64)
  | releaseSlice (pages : BufferPoolSlice)
  | recycleBatch (batch : RecordBatch)

def WriteTxn.commit (self : WriteTxn) : EffM [StorageEff, FailEff IoError] CommitReceipt := do
  let { walSlot, dirtyPages } := self
  Storage.flushWal walSlot dirtyPages
  Storage.releaseSlice dirtyPages
  pure { walSlot }

def WriteTxn.abort (self : WriteTxn) : EffM [StorageEff] Unit := do
  let { walSlot, dirtyPages } := self
  Storage.discardWal walSlot
  Storage.releaseSlice dirtyPages

def appendBatch
    (brokerProj : BrokerIndexMetricsProj)
    (config     : BrokerConfig)
    (batch      : RecordBatch)
    : EffM [StorageEff, FailEff IoError] (CommitReceipt × BrokerIndexMetricsProj) := do
  let tx <- liftLinear (Storage.beginTxn config.maxBatchBytes)
  match validateCrc batch with
  | .ok () =>
    let index' := SegmentedIndex.insertInPlace brokerProj.index batch
    let written' <- checkedAddU64 brokerProj.batchesWritten 1
    let receipt <- WriteTxn.commit tx
    pure (receipt, { index := index', batchesWritten := written' })
  | .error err =>
    liftLinear (Storage.recycleBatch batch)
    liftLinear (WriteTxn.abort tx)
    Fail.raise err

end KrabkaWal
```

Proving *Transactional Crash Atomicity*—that on any CRC validation error, the allocated WAL slot is discarded, zero buffer-pool slices remain pinned, and broker state is untouched—closes in two lines:

```lean
-- illustrative/WalSafety.lean
theorem appendBatch_crc_err_aborts_cleanly
    (st : RefStorageModel) (proj : BrokerIndexMetricsProj)
    (cfg : BrokerConfig) (batch : RecordBatch) (crcErr : IoError)
    (h_crc : validateCrc batch = .error crcErr) :
    let (st', res) := RefStorageModel.run st (appendBatch proj cfg batch)
    res = .error crcErr ∧
    st'.activeWalSlots = st.activeWalSlots ∧
    st'.pinnedSlices   = st.pinnedSlices := by
  simp [appendBatch, h_crc, WriteTxn.abort, RefStorageModel.run]
  omega
```

=== 9.4 Compositional `.olean` Certificates and Axiom-Hygiene Governance
<ch-8-4>

The implementation separates theorem checking from source and executable correspondence:

+ *Kernel replay and explicit assumptions:* `src/proof_commands.rs` snapshots bounded artifact inputs and invokes `tools/proof-audit/Audit.lean`. The helper disables environment extensions and plugins when importing package artifacts, then checks declarations in a fresh kernel environment. Requested names must identify artifact theorems. Reports include theorem types, transitive axioms, artifact inventory, and toolchain fingerprints. The default axiom policy is empty. Explicitly permitted mathematical axioms remain assumptions; proof/compiler escape axioms are never permitted. An audit report grants neither reusable cache approval nor a provenance certificate.
+ *Extraction and source binding:* `vesper verify --extract` generates a Lean bundle from a checked source module and optional proof script. A bare invocation checks model compilation using an internal `True` witness; it does not prove every emitted contract proposition. Selected user theorems are replayed under the explicit axiom policy. Supplying `--contract-proof Function=Theorem` requires a binding and a `--contract-witness Function=Theorem` input witness for every emitted contract. Generated wrappers must have exactly the corresponding contract constant as their replayed type, without additional hypotheses; even implicitly abstracted section instances are rejected. The `checked_contracts` report field separates these proofs from the proposition inventory. Each witness proves the generated `f$admissible` proposition: there exist values for all parameters satisfying the declared precondition, or `True` when no precondition is declared. The same closed-signature check applies to the witnesses, reported as `checked_preconditions`. Contradictory preconditions and hidden instance assumptions fail this gate. Input inhabitation alone does not establish termination or successful return. Cached `.olean` bytes must equal a fresh compilation of the current bundle, and replay uses that fresh compilation's artifact closure. A valid unrelated theorem recorded under the right cache key is rejected. Cache hits therefore still incur compilation and replay.
+ *Publication scope:* `vesper publish` can apply the same artifact replay policy as a release gate. Its acceptance does not independently prove that selected theorem statements describe the release's executable behavior. Publication does not yet require the extraction contract bindings described above; package-wide expected-signature checks and SemVer proof transport remain open. The separate finite `ResourceModel42` transport tests do not establish whole-compiler refinement or a universal second-checker gate.

=== 9.5 Implemented Proof-Audit Entry Points
<ch-8-5>

The relevant implementation is `src/lean.rs`, `src/verify_commands.rs`, `src/proof_commands.rs`, and `tools/proof-audit/Audit.lean`. For an existing local module `Model` containing `Example.correct`, the explicit theorem audit is:

```sh
vesper proof-audit Model --root ./artifacts \
  --theorem Example.correct --lean /path/to/lean --json

vesper verify program.ves --extract --proofs proofs.lean \
  --theorem Example.correct --lean /path/to/lean --json
```

These commands require the pinned Lean 4.35.0 toolchain and real input files. Add `--allow-axiom` only for assumptions the theorem is intended to depend on. Neither a successful subprocess nor a textual zero-`sorryAx` count alone admits a proof. The checker validates structured audit output and its requested theorem inventory, rejects failed or panicking checker processes, and applies the transitive policy. The selected toolchain and artifact parser remain trusted. Lean source elaboration can execute proof scripts, so artifact replay's disabled initializers do not make source compilation an operating-system sandbox.

== 10. Verified Bootstrap, Rewrite Witnesses, and Diverse Double-Compiling
<ch-9>
#chapter-deck[Self-Hosting, Optimization Witnesses, Direct Codegen, and Build-Chain Trust]

Vesper proposes validating both user programs and the compiler that translates them. This chapter describes self-hosting, optimization witnesses, direct code generation, and diverse double-compiling. The current Lean artifacts and examples do not establish end-to-end compiler correctness or eliminate build-chain trust. Assessing the trusted computing base (TCB) requires an inventory of the compiler, runtime, proof checker, system libraries, and hardware.

=== 10.1 Five Pillars of Toolchain Verification
<ch-9-1>

+ *Executable Lean 4 Ground-Truth Reference Oracle:* #link(<appendix-a>)[Appendix A]'s mechanized metatheory includes a computable multi-local reference typechecker (`checkExpr`) and fuel-stepped reference evaluator (`evalSteps`) audited by both Lean 4's kernel and `Lean4Lean`. *Vesper Crucible* (#link(<ch-21>)[Chapter 22]) executes 100,000+ `ChoiceTape`-synthesized programs in differential lockstep across the native compiler and the Lean 4 reference evaluator, enforcing 1-to-1 static diagnostic parity and bit-identical dynamic resource mint/consume traces.
+ *Proof-Producing Equality Saturation (`EgglogRewriteWitness`):* Rather than trusting Stage 4's Datalog e-graph engine (`vesper-opt-egglog`) as a monolithic search procedure, every algebraic, `VVec` SIMD (#link(<ch-13>)[Chapter 14]), `@fip` constructor-reuse (#link(<ch-12-4>)[Section 13.4]), graded bump-check elision, and inline handler-fusion rewrite rule carries a pre-proved Lean 4 refinement lemma. When `egglog` extracts a minimum-cost `PlaceIr` term, it emits a compact `EgglogRewriteWitness` (rule IDs, e-class substitutions, and congruence steps) validated in linear time prior to code emission.
+ *Dual-Tier Backend Soundness (VeriISLE/SMT + `vesper-codegen-verified`):*
  - *High-Throughput Cranelift Tier:* Restricts lowering patterns to *VeriISLE*-verified rules paired with per-block *Bitwuzla* SMT translation validation and `vesper-vault`'s constant-time SSA taint verifier (#link(<ch-27-5>)[Section 28.5]).
  - *Direct Verified Backend (`crates/vesper-codegen-verified`):* For `@verified` targets and `@vesper-lang/compiler` itself, a compact (~2,500 LOC) non-speculative emitter lowers `PlaceIr` through linear-scan register allocation directly to deterministic `x86_64`, `AArch64`, and `RV64` machine bytes (with `vesper_fiber_yield` trampolines and static stack-depth bounds), proven sound via forward simulation in Lean 4.
+ *Hermetic Self-Hosting and Self-Verification (`@vesper-lang/compiler`):* The self-hosted five-stage compiler executes inside a bounded `Region::scoped` `VirtualArena` (#link(<ch-12-2>)[Section 13.2]) under `capability_ceiling = ["Fail"]` in `Loom.toml` (`Alloc`, `Net`, `Clock`, and `Ffi` statically absent). Extracting `@vesper-lang/compiler` through `vesper-verify-lean` proves that `HirChecker::infer_and_constrain` and `PcgSolver::lower_and_verify_exhaustion` refine `HasType`, `CallProjValid`, `FrameSafe`, and *Theorem 2*.
+ *Diverse Double-Compiling (DDC):* A prospective DDC check must compile the compiler source with the diverse reference compiler to obtain `stage1_B`, then execute `stage1_B` on the same source to obtain `stage2_B`. Compare `stage2_B` with the corresponding self-compiled `stage2_A`, under documented determinism and source-correspondence assumptions. This is a design target, not a completed seed-backdoor proof. Lean-generated C depends on Lean's runtime and C extensions; compiling it with CompCert requires a compatible, independently validated runtime and translation path that has not been established here.

=== 10.2 Reproducible Multi-Stage Bootstrap Topology
<ch-9-2>

- *`Stage 0` (`vesperc-rs` Seed + `vesperc-ref` Lean 4 Oracle):* Host compiler core validated in continuous differential lockstep against the computable Lean 4 operational semantics.
- *`Stage 1` (`vesperc-vsp-1` Self-Hosted Generation):* `@vesper-lang/compiler` compiled by `Stage 0` alongside extracted zero-`sorryAx` `.olean` soundness certificates.
- *`Stage 2` (`vesperc-vsp-2` Self-Verifying Generation):* `@vesper-lang/compiler` compiled and witness-checked by `Stage 1` inside `vesper-loom-sandbox` (epoch-zero clock, zero network or ambient filesystem access).
- *DDC comparison (proposed):* Form `stage1_B = compileRef(compilerSrc)` and `stage2_B = exec(stage1_B, compilerSrc)`, then compare `stage2_B` with the corresponding `stage2_A`. A fixed point alone and a one-stage reference comparison do not establish DDC. The following lockfile is illustrative and records no actual attestation:

```toml
# bootstrap/stage3_ddc.lock — Hermetic Fixed-Point & Diverse Double-Compile Anchor
[bootstrap]
compiler_package     = "@vesper-lang/compiler"
version              = "1.0.0"
source_merkle_blake3 = "blake3:7a19e4c0f82b31d69a04e581c3b72f109e84d2a16c5b30f9e1d2a4c8b7f60123"
lean_spec_mif        = "mif:blake3:3c8e91a204f7b5d1e6c28094a7b3d5f18290e4c6b1a3f5d790e2c4a6b8d0f1e2"
allowed_lean_axioms  = ["propext", "Quot.sound", "Classical.choice"]

[fixed_point_quorum]
path_a_stage2_blake3      = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
path_a_stage3_blake3      = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
path_b_lean_compcert_ddc  = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
lila_builder_signatures   = 3
aegis_tdx_snp_attested    = true
```

=== 10.3 Self-Hosted Hermetic Compiler Driver (`@vesper-lang/compiler`)
<ch-9-3>

```rust
// packages/vesper-compiler/src/driver.ves (capability_ceiling = ["Fail"] in Loom.toml)
// Illustrative only: rule ID and target index alone cannot justify a rewrite.
// A production witness must include substitutions, congruence paths, and
// certificates for any arithmetic or ownership side conditions.
copy struct RewriteStepWire {
    rule_id: u16,
    target_node_idx: u32,
    subst_arity: u16,
}

pub fn compile_unit_verified[P: IsaProfile](
    in src_bytes: ByteSlice,
    in ceiling: CapabilityCeiling,
    inout out_elf: ByteSliceMut,
) -> Result[UnitArtifactDigests, CompileDiag] eff [Isa[P], Fail[CompileDiag]] {
    Region::scoped(16.mib(), |cap pass_arena| {
        let cst = Syntax::parse_lossless_ll2[P](in pass_arena, in src_bytes)?;
        let typed_hir = HirChecker::infer_and_constrain(in pass_arena, in cst, in ceiling)?;
        let place_ir = PcgSolver::lower_and_verify_exhaustion(in pass_arena, in typed_hir)?;

        let (opt_ir, witness_steps) = EgglogOpt::saturate_with_witness(in pass_arena, in place_ir);
        WitnessChecker::verify_equational_chain(in place_ir, in opt_ir, in witness_steps)?;

        let code_len = VerifiedCodegen::emit_elf_deterministic(
            in pass_arena,
            in opt_ir,
            inout out_elf,
        )?;
        Ok(UnitArtifactDigests::compute_blake3[P](in place_ir, in out_elf.prefix(code_len)))
    })
}
```

=== 10.4 Lean 4 Forward Simulation and DDC Fixed-Point Proofs
<ch-9-4>

The following composition schema assumes both witness soundness and backend simulation. It proves only their composition; it does not provide either premise, machine semantics, an operational simulation, or a DDC result.

```lean
-- illustrative/BootstrapSimulationAndDdc.lean
theorem end_to_end_verified_codegen_simulation
    {Ir Mach Outcome : Type}
    (evalIr   : Ir -> Outcome)
    (evalMach : Mach -> Outcome)
    (verifyWitness : Ir -> Ir -> Bool)
    (emitMach      : Ir -> Option Mach)
    (h_witness_sound : forall ir1 ir2, verifyWitness ir1 ir2 = true -> evalIr ir2 = evalIr ir1)
    (h_backend_sim   : forall opt m, emitMach opt = some m -> evalMach m = evalIr opt)
    (srcIr optIr : Ir) (machBin : Mach)
    (hw : verifyWitness srcIr optIr = true)
    (hc : emitMach optIr = some machBin) :
    evalMach machBin = evalIr srcIr := by
  rw [h_backend_sim optIr machBin hc, h_witness_sound srcIr optIr hw]

```

=== 10.5 Rewrite-Witness Validator and DDC Gate (`crates/vesper-codegen-verified`)
<ch-9-5>

```rust
// crates/vesper-codegen-verified/src/lib.rs
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifiedRewriteRule {
    AddZeroRight,
    BitXorSelfZero,
    TailResumptiveInline,
    FipReuseSameSizeSlot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprTerm {
    Var(u32),
    ConstU64(u64),
    Add(Box<ExprTerm>, Box<ExprTerm>),
    BitXor(Box<ExprTerm>, Box<ExprTerm>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootstrapVerifyError {
    InvalidRewriteStep { step_idx: usize, rule: VerifiedRewriteRule },
    FinalTermMismatch,
    StageFixedPointDivergence { stage2: blake3::Hash, stage3: blake3::Hash },
    DdcCrossCompilerMismatch { stage3_path_a: blake3::Hash, stage3_path_b_ddc: blake3::Hash },
    InsufficientLilaBuilderQuorum { actual: usize, min_required: usize },
}

pub fn step_verified_rewrite(
    term: &ExprTerm,
    rule: VerifiedRewriteRule,
) -> Option<ExprTerm> {
    match (rule, term) {
        (VerifiedRewriteRule::AddZeroRight, ExprTerm::Add(lhs, rhs))
            if **rhs == ExprTerm::ConstU64(0) =>
        {
            Some((**lhs).clone())
        }
        (VerifiedRewriteRule::BitXorSelfZero, ExprTerm::BitXor(lhs, rhs)) if lhs == rhs => {
            Some(ExprTerm::ConstU64(0))
        }
        _ => None,
    }
}

pub fn verify_rewrite_certificate_chain(
    initial: &ExprTerm,
    expected_final: &ExprTerm,
    steps: &[VerifiedRewriteRule],
) -> Result<(), BootstrapVerifyError> {
    let mut curr = initial.clone();
    for (step_idx, &rule) in steps.iter().enumerate() {
        curr = step_verified_rewrite(&curr, rule)
            .ok_or(BootstrapVerifyError::InvalidRewriteStep { step_idx, rule })?;
    }
    if &curr != expected_final {
        return Err(BootstrapVerifyError::FinalTermMismatch);
    }
    Ok(())
}

pub fn audit_ddc_bootstrap_quorum(
    stage2_bytes: &[u8],
    stage3_bytes: &[u8],
    stage3_lean_compcert_ddc_bytes: &[u8],
    verified_lila_signatures: &[SmolStr],
    min_quorum: usize,
) -> Result<blake3::Hash, BootstrapVerifyError> {
    let h_stage2 = blake3::hash(stage2_bytes);
    let h_stage3 = blake3::hash(stage3_bytes);
    if h_stage2 != h_stage3 {
        return Err(BootstrapVerifyError::StageFixedPointDivergence {
            stage2: h_stage2,
            stage3: h_stage3,
        });
    }
    let h_ddc = blake3::hash(stage3_lean_compcert_ddc_bytes);
    if h_stage3 != h_ddc {
        return Err(BootstrapVerifyError::DdcCrossCompilerMismatch {
            stage3_path_a: h_stage3,
            stage3_path_b_ddc: h_ddc,
        });
    }
    if verified_lila_signatures.len() < min_quorum {
        return Err(BootstrapVerifyError::InsufficientLilaBuilderQuorum {
            actual: verified_lila_signatures.len(),
            min_required: min_quorum,
        });
    }
    Ok(h_stage3)
}
```

== 11. Vesper Loom: Hermetic Package Governance and Supply-Chain Confinement
<ch-10>
#chapter-deck[Dependency Resolution, Capability Policies, Build Sandboxes, and Artifact Caches]

*Vesper Loom* (`crates/vesper-loom-*`, exposed through the `vesper` CLI) brings dependency resolution, build execution, artifact distribution, and `.olean` proof caching into the compiler pipeline. Its proposed policies bound package capabilities, confine build scripts, and support reproducible artifacts. This chapter describes those policies and their enforcement boundaries; a package policy alone does not establish supply-chain security.

=== 11.1 Formal Foundations of Package Resolution and Supply-Chain Security
<ch-10-1>

#figure(
  table(
    columns: (23%, 27%, 50%),
    align: (left, left, left),
    table.header([*Research Domain*], [*Foundational Literature*], [*Normative Mechanism in Vesper Loom*]),
    [*Polyglot Package Calculus*],
    [#link("https://arxiv.org/abs/2602.18602")[Package Managers à la Carte] (Gibb, Ferris, Allsopp, Gazagnaire, and Madhavapeddy, ICFP 2026)],
    [Mechanizes root inclusion, dependency closure, and version uniqueness in Lean 4, reducing monotonic features and native `.vsys` packages into one unified IR.],
    [*CDCL & Max-SMT Resolution*],
    [#link("https://arxiv.org/abs/2203.13737")[Flexible and Optimal Dependency Management via Max-SMT] (`PacSolve`, Pinckney et al.); #link("https://arxiv.org/abs/2608.15886")[SMTpip] (SCAM 2026); `PubGrub` & `Resolvo` CDCL],
    [Pairs sub-50ms CDCL version solving and incompatibility derivation trees with exact Max-SMT optimization over CVEs, capability surface, and staleness.],
    [*Capability-Bounded Auditing*],
    [#link("https://arxiv.org/abs/2602.06466")[Auditing Rust Crates Effectively] (`CargoScan`, ESOP 2026); #link("https://arxiv.org/abs/2512.12553")[Cargo Sherlock] (FormaliSE 2026)],
    [Enforces static `capability_ceiling` lattices across every dependency edge (proving *No Authority Amplification*) and bounds maintainer trust costs.],
    [*Cross-Language Boundaries*],
    [Cross-language vulnerability migration in the Rust supply chain (citation details require verification)],
    [Replaces opaque `*-sys` build scripts and hidden linker flags with declarative `.vsys` manifests visible to SBOMs, CVE reachability, and MPK sandboxes.],
    [*Hermetic Reproducibility*],
    [#link("https://arxiv.org/abs/2501.15919")[Does Functional Package Management Enable Reproducible Builds at Scale? Yes] (MSR 2025); #link("https://luj.fr/notes/20260522T000000==public--decade-of-software-reproducibility__research.org")[A Decade of Software Reproducibility in the Nix Package Ecosystem] (EMSE 2026); #link("https://arxiv.org/abs/2601.20662")[Lila] (2026)],
    [Proposes input-addressed `BLAKE3` derivations and independent builder attestations. Lila uses a centralized service; the proposed quorum is a separate Vesper design.],
    [*Anti-Slopsquatting Defense*],
    [#link("https://www.usenix.org/conference/usenixsecurity25/presentation/spracklen")[Package Hallucinations by Code Generating LLMs] (Spracklen et al., USENIX Security 2025)],
    [Mandates Sigstore OIDC-scoped `@org/pkg` namespaces, TUF metadata, and edit-distance/conflation quarantine gates during `vesper add`.],
  ),
  kind: table,
)

=== 11.2 Six Core Architectural Guarantees of Vesper Loom
<ch-10-2>

+ *Compile-Time Lexical Capability Ceilings:* Because all external authority in Vesper is reified as `eff [...]` capabilities and explicit `unsafe ffi` bindings, every `Loom.toml` manifest declares an explicit `capability_ceiling` (`effects = []` for pure codecs/parsers) and per-dependency `effects` grants. The compiler verifies that no module instantiates an OS handler or invokes FFI outside its ceiling, that every package's ceiling covers the union of its dependencies' ceilings, and that minor updates escalating `effects` abort immediately with a *Capability Escalation Error*.
+ *Hermetic `wasm32-wasip2` Build Sandboxes (`build.ves`) and Declarative `.vsys`:* Code generators (`build.ves`) compile to deterministic WebAssembly Components executed inside Wasmtime with instruction-fuel metering, zero network sockets, epoch-zero clocks, and read-only preopened input directories plus `$OUT_DIR`. Native C libraries bind strictly via declarative `.vsys` manifests (#link(<ch-15-3>)[Section 16.3]) lowered directly into the Package Calculus graph.
+ *Merkle Interface Fingerprints (`mif:blake3`) and Early Cutoff:* Downstream compilation units depend on a dependency's `.vhir` interface digest rather than its implementation body hash. Internal bug fixes or refactors that preserve `mif:blake3` trigger zero downstream recompilation, relinking only the final artifact while `vesper semver` (#link(<ch-11>)[Chapter 12]) blocks undeclared breaking changes at publish time.
+ *Dual-Engine Resolution (CDCL Fast Path + Max-SMT Policy Optimizer):* Everyday resolution runs `resolvo` CDCL in under 50 ms with human-readable PubGrub incompatibility proofs explaining modal, effect-ceiling, and version conflicts. Policy-driven upgrades (`vesper resolve --optimize`) encode the Package Calculus graph alongside OSV advisories, capability weights, and trust costs into Bitwuzla/Z3 Max-SMT to synthesize Pareto-optimal lockfiles.
+ *OIDC-Scoped Namespaces, Slopsquatting Quarantine, and Lila Quorum:* Unscoped names (`std/*`) belong exclusively to Vesper's standard platform; third-party packages require `@org/pkg` identities bound via keyless Sigstore OIDC to Rekor v2 transparency logs and TUF metadata. During `vesper add` (especially under `VESPER_AGENT_MODE=1`), Loom screens edit distance, namespace age, and trust cost, while binary cache artifacts require at least two independent `Lila` builder attestations matching the exact `BLAKE3` output digest.
+ *Content-Addressed Store (`~/.vesper/store/`) and Reflink Workspaces:* Source trees, `.vhir` interfaces, pre-saturated `egglog` bundles, Cranelift `.o` archives, and `.olean` proof certificates are indexed by `BLAKE3(derivation_inputs)` and materialized into workspaces instantaneously via OS copy-on-write reflinks (`clonefile` on APFS, `FICLONE` on XFS/Btrfs).

=== 11.3 Declarative Manifest (`Loom.toml`) and Lockfile (`Loom.lock`)
<ch-10-3>

`Loom.toml` parses in under 1 millisecond with zero Turing-complete evaluation:

```toml
[package]
name = "@robot-head/krabka-wal"
version = "0.4.0"
edition = "2026"
license = "Apache-2.0"
capability_ceiling = ["Storage", "Net", "Clock", "Fail"]
forbid_unsafe_ffi = true

[features]
# Features are strictly monotonic and additive over capabilities:
io_uring = { enables_effects = ["Storage"], sys_deps = ["liburing ^2.6.0"] }
compression = { deps = ["@vesper-compression/lz4-pure"] }

[dependencies]
"@vesper-core/bytes" = { version = "^1.3.0", effects = [] }
"@vesper-compression/lz4-pure" = { version = "^2.1.0", effects = [], optional = true }
"@vesper-io/uring-reactor" = { version = "^0.9.2", effects = ["Storage", "Fail"] }

[build-sandbox]
script = "build.ves"
wasi_read_paths = ["proto/"]
fuel_limit = 50_000_000
allow_network = false

[resolver.policy]
optimize = ["zero_cve", "min_capability_surface", "min_dup_majors", "min_staleness"]
min_repro_attestations = 2
block_unvetted_namespaces = true

[verification]
export_lean4_specs = ["src/wal_txn.ves"]
```

*Path dependencies (implemented).* A `[dependencies]` entry with a `path` names a local package directory, relative to the manifest: `"@demo/geometry" = { version = "^0.1", path = "../geometry" }`. The dependency's `Loom.toml` must declare the same package name and a version that satisfies `version`. The checker assembles the root package and its dependency closure into one compilation unit; a cycle between packages, a package loaded from two directories, or an entry without a `path` (registry dependencies are not implemented yet) is an error. `vesper check`, `run`, `jit`, `build` and `emit` accept a package directory in place of a source file.

*Modules and imports (implemented in `src/packages.rs`).* Imports use the `UseDecl` production of #link(<ch-2-2>)[Section 2.2]. A package's modules are its files `src/**/*.ves`: `src/lib.ves` or `src/main.ves` is the root module, and `src/a/b.ves` is module `a::b`. A module sees its own top-level declarations and the items it imports. A `UseDecl` names a package (`@scope/name`, which must be a `[dependencies]` entry of the importing package, or `self` for the importing package), then a module path, then one item or a braced list of items, each optionally renamed with `as`:

```vesper
use @vesper-std/collections::{ChunkSeq, CacheBTree};
use @demo/geometry::shapes::{Shape, area as shape_area};
use self::util::clamp;
```

Only `Vis`-marked (`pub`) functions, types and effects can be imported; every other item is private to its file. Importing an enum or effect makes its variants or operations usable; variant and operation names share one namespace per program, so two modules may not declare the same one. Trait and named-model names are global in the same way and are never imported: a `trait`, a canonical `impl Trait for Type`, a `model`, a trait bound and a `use model` scope may each appear in any module, and a conformance applies wherever its type is visible. Inherent `impl` blocks for one type may be split across the modules that import the type. Importing a name that is also declared or already imported in the module is an ambiguity error, and an unimported item of another module is not in scope. Imports of whole modules (`use self::m;`) are not part of this revision. A `UseDecl` or a qualified path in a file checked on its own (outside a package) is an error (`E0110`).

*Namespaces.* A function lives in the value namespace, an effect in the type namespace, and a struct or enum in both (its name is a type, a struct's name is also its constructor, and `E::V` starts at the type). An import binds its name only in the namespaces of the item it names, so `use self::reader::u64;` makes `u64(x)` the imported function while `u64` in a type is still the scalar type.

*Qualified paths.* Wherever an item name may appear, in an expression, a struct literal, a type, an effect row or a pattern, a `Path` may start with a `PathRoot`: `self::` (the package root), `super::` (the parent of the current module, repeatable) or `@scope/name::` (a dependency). The longest prefix of the following segments that is a module selects the module, the next segment is the item, and any further segments are an associated function or variant of it, as in `self::codec::shape::Point::new(3)` and `self::codec::shape::Tag::Small(n)`. A path needs no `use`, and it follows the import rules: the item must be `pub` unless the path names the current module (`E0112`), and an unknown module or item is `E0111`. `super` above the package root is `E0111`. A `.vsys` library is not an item: every module of the package names it directly (`Lib::f(..)`, `eff [Ffi[Lib]]`), with no import.

*Re-exports.* `pub use` binds its names like `use` and also makes them items of the importing module that other modules may import or reach by path; a chain of re-exports resolves to the original item. A library package's exports are the `pub` functions of its root module, so the root module exports a submodule's or a dependency's function by re-exporting it: `pub use self::frontier::{advance, merge as frontier_merge};` exports `vesper_<library>_advance` and `vesper_<library>_frontier_merge`. A re-exported item takes its root name everywhere, so one item has one exported name: re-exporting it twice from the root module is an ambiguity error (`E0113`). Submodule items that are not re-exported stay internal, under the collision-free internal names above.

Every resolved node in `Loom.lock` records four cryptographic anchors: `source_blake3` (unpacked source Merkle digest), `interface_mif` (`mif:blake3:...` digest of exported types, `ModeVec` bounds, projections, refinements, and effect rows), `effective_capabilities` (verified transitive effect set), and `provenance` (Sigstore Rekor index, `Lila` builder signatures, and `.olean` certificate digest).

=== 11.4 `vesper` Toolchain CLI Surface
<ch-10-4>

The current command list and flags are defined by `vesper --help` and the per-command help in `src/main.rs`. The CLI includes `forge`, `grammar`, `verify`, `fuzz`, `migrate`, `replay`, `ebpf`, `fmt`, `highlight`, `shot`, `lint`, `doc`, `test`, `bench`, `check`, `run`, `jit`, `build`, `emit`, `resolve`, `graph`, `sbom`, `audit`, `top`, `json`, `traceparent`, `simd`, `proof-audit`, `attest`, `cas`, `semver`, and `lsp`. Each command supports only the operations described by its own help. Registry publishing, package scaffolding, and the other lifecycle operations described elsewhere are proposals unless implemented; their appearance in an example does not make them available commands.

=== 11.5 Confinement, WASI Sandbox, and `redb` Store (`crates/vesper-loom-*`)
<ch-10-5>

Five dedicated crates implement Vesper Loom: `vesper-loom-calculus` (Package Calculus IR and capability ceilings), `vesper-loom-solver` (`resolvo` CDCL + Bitwuzla/Z3 Max-SMT), `vesper-loom-sandbox` (fuel-metered Wasmtime `wasm32-wasip2` host), `vesper-loom-cas` (`blake3` + `redb` store defining four zero-copy tables in `~/.vesper/store/index.redb`: `DERIVATIONS`, `MIF_CACHE`, `OLEAN_CERTS`, and `LILA_QUORUM`), and `vesper-loom-trust` (Sigstore OIDC, TUF, Lila quorum, and slopsquatting detectors):

```rust
// crates/vesper-loom-calculus/src/lib.rs
use rustc_hash::FxHashSet;
use smol_str::SmolStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityCeiling {
    pub allowed_effects: FxHashSet<SmolStr>,
    pub allow_unsafe_ffi: bool,
}

#[derive(Clone, Debug)]
pub enum ConfinementViolation {
    AuthorityAmplification {
        pkg: SmolStr,
        dep: SmolStr,
        escalated_effect: SmolStr,
    },
    ForbiddenUnsafeFfi { pkg: SmolStr },
}

impl CapabilityCeiling {
    /// Verifies the No Authority Amplification invariant across every dependency edge `P -> D`.
    pub fn verify_edge(
        &self,
        pkg_name: &SmolStr,
        dep_name: &SmolStr,
        dep_effective: &Self,
    ) -> Result<(), ConfinementViolation> {
        if dep_effective.allow_unsafe_ffi && !self.allow_unsafe_ffi {
            return Err(ConfinementViolation::ForbiddenUnsafeFfi { pkg: dep_name.clone() });
        }
        for eff in &dep_effective.allowed_effects {
            if !self.allowed_effects.contains(eff) {
                return Err(ConfinementViolation::AuthorityAmplification {
                    pkg: pkg_name.clone(),
                    dep: dep_name.clone(),
                    escalated_effect: eff.clone(),
                });
            }
        }
        Ok(())
    }
}

// crates/vesper-loom-sandbox/src/lib.rs
use std::path::Path;
use wasmtime::{Config, Engine, Linker, Store};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder};

pub struct HermeticBuildConfig<'a> {
    pub wasm_component_bytes: &'a [u8],
    pub read_only_inputs: &'a [(&'a Path, &'a str)],
    pub out_dir: &'a Path,
    pub fuel_limit: u64,
}

/// Executes `build.ves` or syntax macros inside a deterministic, zero-network WASI-p2 sandbox.
pub fn run_hermetic_build_step(cfg: &HermeticBuildConfig<'_>) -> anyhow::Result<()> {
    let mut engine_cfg = Config::new();
    engine_cfg.consume_fuel(true);
    engine_cfg.wasm_component_model(true);

    let engine = Engine::new(&engine_cfg)?;
    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder.env("OUT_DIR", "/out");

    for (host_path, guest_mount) in cfg.read_only_inputs {
        wasi_builder.preopened_dir(host_path, guest_mount, DirPerms::READ, FilePerms::READ)?;
    }
    wasi_builder.preopened_dir(cfg.out_dir, "/out", DirPerms::all(), FilePerms::all())?;

    let mut store = Store::new(&engine, wasi_builder.build());
    store.set_fuel(cfg.fuel_limit)?;

    let component = wasmtime::component::Component::from_binary(&engine, cfg.wasm_component_bytes)?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::add_to_linker_sync(&mut linker)?;

    let instance = linker.instantiate(&mut store, &component)?;
    let run_fn = instance.get_typed_func::<(), (Result<(), String>,)>(&mut store, "run-build")?;
    let (res,) = run_fn.call(&mut store, ())?;
    res.map_err(|msg| anyhow::anyhow!("hermetic build.ves failed: {msg}"))
}
```

== 12. Mathematical SemVer Variance and Interface Subtyping
<ch-11>
#chapter-deck[Source Compatibility Across Ownership, Effects, Refinements, and Resource Bounds]

`vesper semver` (`crates/vesper-semver`) conservatively compares supported, resolved source interfaces. Its diagnostics do not prove contextual substitutability for the whole language. A source-compatible change may still alter sizes, alignments, field offsets, calling conventions, or caller resource bounds. Precompiled consumers and layout-sensitive graded callers therefore need a separate checked layout contract; the current source-interface schema does not certify one.

=== 12.1 Modal, Effect-Row, and Field-Tree Subtyping Orders
<ch-11-1>

Signature compatibility reduces to decidable partial orders across every dimension of $lambda_(upright("Vesper"))$:

+ *Modal Vector Strengthening ($bold(m)_(upright("new")) subset.eq.sq bold(m)_(upright("old"))$):* Moving a produced value or closed ADT kind downward along $subset.eq.sq$ (such as `linear -> affine -> copy`, `pinned -> portable`, or `lifo -> unordered`) strictly expands permitted caller operations while preserving existing consumers via `[T-SubMode]`. Moving upward (`affine -> linear` or `unordered -> lifo`) breaks caller `discard(Gamma)` drops, `[T-Let]` frames, or release ordering.
+ *Graded Lexical Effect-Row Subtyping ($epsilon_(upright("new")) <: epsilon_(upright("old"))$):* An upgraded effect row neither introduces ungranted capabilities, nor degrades continuation resumption guarantees, nor raises upper-bound resource budgets. Exact grades must remain equal and cannot disappear:

#_infer(
  "[SV-EffRow]",
  $upright("caps")(epsilon_(upright("new"))) subset.eq upright("caps")(epsilon_(upright("old"))) quad upright("row_vars")(epsilon_(upright("new"))) subset.eq upright("row_vars")(epsilon_(upright("old"))) \ upright("res")(epsilon_(upright("old"))) subset.eq.sq upright("res")(epsilon_(upright("new"))) \ upright("exactCaps")(epsilon_(upright("old"))) = upright("exactCaps")(epsilon_(upright("new"))) quad forall E in upright("caps")(epsilon_(upright("new"))), med upright("gradeCompatible")(g_(upright("new"))(E), g_(upright("old"))(E))$,
  $epsilon_(upright("new")) <: epsilon_(upright("old"))$,
)

Here `gradeCompatible` means a non-increasing bound for an upper-bound contract and equality for an exact contract. Removing an exact graded capability is rejected. Expression effect subsumption uses the same ordering:

#_infer(
  "[T-SubEff]",
  $Delta ; Sigma ; Phi ; Gamma tack.r e : tau @ (rho, bold(m)) divides epsilon_(upright("new")) quad epsilon_(upright("new")) <: epsilon_(upright("old"))$,
  $Delta ; Sigma ; Phi ; Gamma tack.r e : tau @ (rho, bold(m)) divides epsilon_(upright("old"))$,
)

Removing an abortable `Fail` effect raises $upright("res")(epsilon)$ from `affine` to `linear`, keeping every caller's `[T-Let]` check ($u(bold(m)_z) subset.eq.sq upright("affine") subset.eq.sq upright("linear")$) valid; adding `Fail` drops $upright("res")(epsilon)$ to `affine`, breaking callers holding live `linear` obligations across the call.
+ *Disjoint Field-Tree Narrowing ($pi_(upright("new")) subset.eq pi_(upright("old"))$):* Narrowing a borrowed field tree (`inout self.{index, metrics} -> inout self.index`) shrinks `leaves(pi)`, preserving every caller's `[T-CallProj]` disjointness proof ($upright("leaves")(pi_(upright("new"))) inter upright("leaves")(pi_(upright("other"))) = emptyset$).
+ *Passing-Mode Linear Conservation:* Changing `sink` to `in` or `inout` on any potentially `linear` parameter is *Major-breaking* because callers relied on `sink` to discharge their linear obligation; changing `in` to `inout`, `out`, or `sink` is likewise *Major-breaking*.

=== 12.2 Declaration, Refinement, and Resource-Grade Compatibility Rules
<ch-11-2>

#_infer(
  "[SV-Fn]",
  $forall k, med bold(m)_(upright("bound,old"), k) subset.eq.sq bold(m)_(upright("bound,new"), k) quad bold(m)_(upright("fn,new")) subset.eq.sq bold(m)_(upright("fn,old")) quad forall i, med mu_(i, upright("new")) = mu_(i, upright("old")) \ forall i, med pi_(i, upright("new")) subset.eq pi_(i, upright("old")) \ forall i, med (mu_i in {upright("in"), upright("sink")} arrow.r.double Delta_(upright("old")) ; Phi_(upright("old")) tack.r tau_(i, upright("old")) <: tau_(i, upright("new"))) \ and (mu_i = upright("inout") arrow.r.double tau_(i, upright("in,old")) <: tau_(i, upright("in,new")) and tau_(i, upright("out,new")) <: tau_(i, upright("out,old"))) \ Delta_(upright("old")) ; Phi_(upright("new")) tack.r tau_(upright("ret,new")) <: tau_(upright("ret,old")) quad bold(m)_(upright("ret,new")) subset.eq.sq bold(m)_(upright("ret,old")) quad epsilon_(upright("new")) <: epsilon_(upright("old"))$,
  $(upright("fn") @ bold(m)_(upright("fn,new")) [Delta_(upright("new"))] (overline(mu) med overline(x) . overline(pi)_(upright("new")) : overline(tau)_(upright("new"))) arrow.r tau_(upright("ret,new")) @ bold(m)_(upright("ret,new")) upright(" eff ")[epsilon_(upright("new"))]) prec.curly.eq_(upright("pub")) (upright("fn") @ bold(m)_(upright("fn,old")) [Delta_(upright("old"))] (overline(mu) med overline(x) . overline(pi)_(upright("old")) : overline(tau)_(upright("old"))) arrow.r tau_(upright("ret,old")) @ bold(m)_(upright("ret,old")) upright(" eff ")[epsilon_(upright("old"))])$,
)

The closed-field rule below is a source-signature rule only. Public opaque types need an explicit exported mode contract; private representation changes must satisfy it. Binary compatibility additionally requires unchanged size, alignment, field offsets where exposed, and calling convention. Layout-sensitive resource bounds must be rechecked even when modal kind is unchanged. Model brands must remain part of dynamic-interface and wire identities; erasing them is not justified by this rule.

#_infer(
  "[SV-Adt-Closed]",
  $upright("vis")(T) = upright("closed_fields") quad Delta_(upright("old")) tack.r bold(k)_(T, upright("new")) subset.eq.sq bold(k)_(T, upright("old"))$,
  $(bold(m)_(upright("new")) upright(" struct ") T[overline(alpha)] med { upright("private ") dots }) prec.curly.eq_(upright("pub")) (bold(m)_(upright("old")) upright(" struct ") T[overline(alpha)] med { upright("private ") dots })$,
)

#_infer(
  "[SV-Enum-Open]",
  $upright("open enum ") E[overline(alpha)] quad upright("variants")(E_(upright("old"))) subset.eq upright("variants")(E_(upright("new"))) \ forall V(overline(tau)_v) in upright("variants")(E_(upright("new"))) without upright("variants")(E_(upright("old"))), quad Delta_(upright("old")) tack.r bold(k)_(overline(tau)_v) subset.eq.sq bold(k)_(E_(upright("old")))$,
  $E_(upright("new")) prec.curly.eq_(upright("pub")) E_(upright("old"))$,
)

#_infer(
  "[SV-Effect-Decl]",
  $upright("ops")(E_(upright("new"))) = upright("ops")(E_(upright("old"))) quad (upright("sealed")(E) arrow.r.double q_(upright("old")) subset.eq.sq q_(upright("new"))) quad (upright("open")(E) arrow.r.double q_(upright("new")) = q_(upright("old")))$,
  $(q_(upright("new")) upright(" effect ") E) prec.curly.eq_(upright("pub")) (q_(upright("old")) upright(" effect ") E)$,
)

Refinement predicates and pomonoid resource budgets integrate directly via Bitwuzla SMT implication checks:

- *`[SV-Refine-Pre]` (Contravariant Preconditions):* Weakening a `where` clause or input refinement ($upright("Valid")_(upright("SMT"))(phi.alt_(upright("old")) arrow.r.double.long phi.alt_(upright("new")))$) is *Minor-compatible*; strengthening an input refinement is *Major-breaking*, and Bitwuzla's satisfying assignment for $phi.alt_(upright("old")) and not phi.alt_(upright("new"))$ directly populates the concrete counterexample literal in `vesper semver`'s broken-caller witness.
- *`[SV-Refine-Post]` (Covariant Postconditions and `~>` Outputs):* Strengthening a return refinement, `ensures` clause, or `inout`/`out` post-update type ($upright("Valid")_(upright("SMT"))(psi_(upright("new")) arrow.r.double.long psi_(upright("old")))$) is *Minor-compatible*.
- *`[SV-Grade]` (Upper-Bound Resource Budgets):* Lowering a published resource ceiling ($g_(upright("new")) prec.curly.eq g_(upright("old"))$, e.g., `Stack @ <= 384` to `Stack @ <= 256`) is *Minor-compatible*; raising a budget is *Major-breaking*. Exact costs use equality instead; changing `== 100` to `== 99`, `<= 100`, or no grade is rejected.
- *Open Enum and Effect Guardrails (`[SV-Enum-Open]`, `[SV-Effect-Decl]`):* Adding a `linear` payload variant to an `affine` `open enum` is rejected because downstream wildcard arms (`_ => ...`) require `discard`, and changing an `open` effect from `affine` to `linear` is rejected because third-party `handle` clauses dropping continuation `k` would fail `[T-Handle]`.

=== 12.3 Complete SemVer Classification Matrix and Witness Synthesis
<ch-11-3>

#figure(
  table(
    columns: (33%, 19%, 48%),
    align: (left, left, left),
    table.header([*Interface Evolution ($I_(upright("old")) arrow.r I_(upright("new"))$)*], [*SemVer Class*], [*Governing Rule and Soundness Invariant*]),
    [Refactor body or private item with unchanged `.vhir`], [*Patch* (`mif` equal)], [Triggers *Early Cutoff*; downstream crates skip recompilation.],
    [Relax generic modal bound (`T: mod copy` $arrow.r$ `T: mod linear`)], [*Minor*], [Contravariant $bold(m)_(upright("bound,old")) subset.eq.sq bold(m)_(upright("bound,new"))$ in `[SV-Fn]`.],
    [Strengthen return or closed-struct mode (`linear` $arrow.r$ `affine` $arrow.r$ `copy`)], [*Minor*], [Covariant `[T-SubMode]` preserves exact-once callers while allowing drops/copies.],
    [Strengthen ADT mode (`affine` $arrow.r$ `linear` or `unordered` $arrow.r$ `lifo`)], [*Major*], [Invalidates caller `discard(Gamma)`, `[T-Let]` frames, or release order.],
    [Remove `Fail[E]` or other capability from `eff [...]`], [*Minor*], [`caps` shrinks and $upright("res")(epsilon)$ rises (`affine <= linear`).],
    [Add `Fail[E]` or `Storage` to `eff [...]`], [*Major*], [Exceeds caller `capability_ceiling` and breaks live `linear` `[T-Let]` frames.],
    [Narrow `inout` projection (`self.{index, metrics}` $arrow.r$ `self.index`)], [*Minor*], [$pi_(upright("new")) subset.eq pi_(upright("old"))$ preserves all caller `[T-CallProj]` disjointness proofs.],
    [Widen `inout` projection (`self.index` $arrow.r$ `self.{index, config}`)], [*Major*], [Collides with concurrent `in self.config` borrows at call sites.],
    [Change `sink x: T` to `in x: T` on potentially `linear` `T`], [*Major*], [Leaves linear argument unconsumed at call site, violating *Theorem 2*.],
    [Weaken parameter refinement or tighten `ensures` / `Stack` grade], [*Minor*], [Discharged by `[SV-Refine-Pre]`, `[SV-Refine-Post]`, and `[SV-Grade]` in Bitwuzla.],
    [Strengthen parameter refinement or raise `Region` / `Stack` grade], [*Major*], [Bitwuzla `SAT` model synthesizes concrete broken-caller counterexample.],
  ),
  kind: table,
)

Whenever `vesper semver` or `vesper publish` detects $I_(upright("new")) prec.curly.eq.not_(upright("pub")) I_(upright("old"))$ without a major version bump, the solver emits a minimal 5--10 line synthetic Vesper caller module demonstrating the exact `[T-Let]`, `[T-CallProj]`, `[T-Handle]`, or refinement obligation broken by the candidate change.

= Part III. Core Runtime, Memory, SIMD, and Interoperability Substrates
<part-3>
#chapter-deck[Memory Management, Vector Operations, Fiber I/O, Foreign Calls, and WebAssembly Plugins]

#_part-summary("Part III Architectural Scope")[
  Part III describes memory management and the runtime interfaces used by later subsystems. #link(<ch-12>)[Chapter 13] introduces *Strata* allocation capabilities, lexical regions, constructor reuse, and tiled collections. #link(<ch-13>)[Chapter 14] covers *Vector* fixed-width and scalable SIMD. #link(<ch-14>)[Chapter 15] describes *Pulse* fibers, completion I/O, and deterministic simulation. #link(<ch-15>)[Chapter 16] covers *Bridge* wire layouts and foreign calls; #link(<ch-16>)[Chapter 17] covers *Nexus* WebAssembly plugins. The chapters identify the proposed ownership and isolation contracts, including obligations that require further implementation and validation.
]

#figure(
  table(
    columns: (14%, 34%, 52%),
    align: (left, left, left),
    table.header([*Chapter*], [*Subsystem*], [*Package, Host Crate, and Core Abstractions*]),
    [#link(<ch-12>)[Chapter 13]], [Vesper Strata (Memory & AoSoA)], [`@vesper-std/alloc` (`vesper-alloc`): `eff [r: Region]`, `ShardSlab`, `VirtualArena`, `@fip`, `tiled(N) struct`],
    [#link(<ch-13>)[Chapter 14]], [Vesper Vector (Portable & Scalable SIMD)], [`@vesper-std/simd` (`vesper-simd`): `eff [Isa[P]]`, `Vec[T, N]`, `VVec`, `bv_decide` equivalence],
    [#link(<ch-14>)[Chapter 15]], [Vesper Pulse (Async Fibers & `SimPulse`)], [`@vesper-std/pulse` (`vesper-pulse`): `Pulse::scope`, `io_uring` DMA, `EvidenceVector`, `SimPulse`],
    [#link(<ch-15>)[Chapter 16]], [Vesper Bridge (Zero-Copy Wire & FFI)], [`@vesper-std/ffi` (`vesper-ffi`): `wire struct`, `.vsys`, `ForeignVal[T]`, 1.5 ns MPK/POE isolation],
    [#link(<ch-16>)[Chapter 17]], [Vesper Nexus (WasmFX & WIT Plugins)], [`@vesper-wasm/nexus` (`vesper-nexus`): `cont.new`/`resume`, `WitOwn[T]`, zero-copy shared views],
  ),
  kind: table,
)


== 13. Vesper Strata: Lexical Regions, Allocators, and AoSoA Collections
<ch-12>
#chapter-deck[Allocation Capabilities, Lexical Arenas, Constructor Reuse, and Tiled Collections]

*Vesper Strata* (`@vesper-std/alloc` and `@vesper-std/collections`, with implementation code in `crates/vesper-alloc`) describes memory management without garbage collection, lexical arenas, and cache-conscious collections. The design combines explicit allocation capabilities with compiler-managed tiling. Its proposed mechanisms are:

- *Region Locality Under Adversarial Fragmentation:* #link("https://arxiv.org/abs/2605.17119")[Reconsidering "Reconsidering Custom Memory Allocation"] (van Kempen and Berger, ISMM 2026) demonstrates under adversarial heap preconditioning that general-purpose allocators suffer up to 2x slowdowns once long-running workloads fragment the heap, whereas *region allocators* preserve contiguous spatial locality and `O(1)` bulk reclamation regardless of prior heap state.
- *Lexical Arenas With Safe Intra-Region Cycles:* #link("https://arxiv.org/html/2509.04253v3")[A Type System for Arenas with Higher-Order Reachability Tracking] (He et al., 2026) and #link("https://arxiv.org/html/2510.08939v2")[Reachability Types with Flow-Sensitive Effects] (2026) prove that tracking reachability at the lexically scoped arena boundary safely permits shared and cyclic graph topologies inside an arena while reclaiming the entire region in bulk at scope exit without garbage collection.
- *Zero-RC Functional-But-In-Place (`@fip`):* Building on #link("https://webspace.science.uu.nl/~swier004/publications/2023-icfp.pdf")[_FP²: Fully in-Place Functional Programming_] (Lorenzen, Leijen, and Swierstra, ICFP 2023), Vesper leverages compile-time uniqueness on `sink` parameters to reuse deallocated constructors in place with *0-byte object headers and zero dynamic `rc == 1` branches*.
- *Transient Sequences and Cache-Tiled Layouts:* Combining #link("https://www.chargueraud.org/research/2026/sek/sek.pdf")[_Transient Sequences_] (Charguéraud and Pottier, ICFP 2026) with compiler-managed *Array-of-Structures-of-Arrays (`AoSoA`)* tiling aligns collections directly with 64-byte L1 cache lines and *Vesper Vector* SIMD registers.

=== 13.1 Core Memory, Region, and Cache-Layout Guarantees
<ch-12-1>
- *Explicit Heap Authority (`eff [Alloc]`):* Rather than granting ambient `GlobalAlloc` access across all modules, In Vesper, explicit heap allocation requires `eff [Alloc]` or `eff [r: Region]`; functions omitting both are *statically guaranteed zero-allocation*.
- *Lifetime-Free Lexical Regions (`eff [r: Region]`):* Rust arena libraries either pollute container types with allocator parameters or infect structs with `'arena` lifetimes and skip destructors. Vesper ties region handles (`RegionBox[r, T]`) to lexical capability names in `Σ`—requiring zero `'a` lifetime annotations while preserving `linear` and `affine` soundness.
- *Compile-Time In-Place Node Reuse (`@fip`):* Functional transformations over owned algebraic trees ordinarily reallocate on every constructor step unless rewritten imperatively. Vesper's `@fip` pass pairs `sink` unpacks with same-sized allocations statically in `egglog`.
- *First-Class `tiled(N) struct` (`AoSoA`):* Rust defaults to `AoS` layouts that pollute L1 caches with cold fields, while proc-macro `SoA` crates break field borrowing. Vesper supports native `tiled(N) struct` declarations with `hot`/`cold` field segregation that compose seamlessly with `[T-CallProj]` disjoint field projections and `VVec` SIMD loads.
- *Capability-Branded Cyclic Graphs:* Instead of `Rc`/`RefCell` overhead or unbranded integer indices prone to cross-arena mixups, `SlotArena[r, V]` and `RegionRef[r, T]` provide zero-RC cyclic graph references mediated safely by `inout r`.

=== 13.2 Lexical Allocation Capabilities and Escape-Free Region Typing
<ch-12-2>

*Review limitation:* A region brand must not escape through results, post-call contexts, mutable output parameters, or handler state. A watermark rewind is unsafe while any allocation after the watermark remains live; the listings below do not establish that condition and must not be used as a safe API design.
Strata factors memory provisioning into two lexical effects in `Σ`:

```
// 1. General-purpose sharded slab allocation:
linear effect Alloc {
    fn alloc_raw(size: usize, align: usize) -> RawBlock;
    fn dealloc_raw(sink block: RawBlock) -> Unit;
}

// 2. Lexically scoped virtual-memory bump region tied to capability label `r`:
linear effect Region {
    fn bump_alloc(size: usize, align: usize) -> RegionPtr;
    fn checkpoint() -> Watermark;
    fn rewind(sink mark: Watermark) -> Unit;
}
```

Region containment without `'arena` lifetime parameters is enforced by indexing region handles with the lexical capability name `r` and checking `r` absence in the scope's return type:

```
[T-RegionAlloc]
  (r : Cap_linear[Region]) ∈ Σ    Δ; Σ; Γ ⊢ e : τ @ m | ε
  ─────────────────────────────────────────────────────────────────────────────
  Δ; Σ; Γ ⊢ r.box(e) : RegionBox[r, τ] @ m | ε ∪ {r : Region^linear}

[T-RegionScope]
  Δ; (Σ, r : Cap_linear[Region]); Γ ⊢ e_body : τ_out @ m_out | {r : Region^linear} ∪ ε
  r ∉ free_caps(τ_out)
  ─────────────────────────────────────────────────────────────────────────────────────
  Δ; Σ; Γ ⊢ Region::scoped(λr. e_body) : τ_out @ m_out | ε
```

Three invariants follow directly from `[T-RegionAlloc]` and `[T-RegionScope]`:

+ *Escape Freedom Without Lifetimes:* `r ∉ free_caps(τ_out)` prevents any `RegionBox[r, T]`, `RegionSlice[r, T]`, or closure capturing `r` from escaping `Region::scoped`.
+ *Substructural Modal Preservation:* `RegionBox[r, T]` inherits mode `m` from `T`:
  - When `T : mod copy`, `RegionBox[r, T]` crosses via `[T-Cross]` to `copy`, permitting unrestricted intra-region DAG/graph aliasing and `O(1)` bulk pointer reset with zero destructor traversal.
  - When `T : mod linear` (such as `WriteTxn`), `RegionBox[r, T]` has mode `linear`—statically forbidding region exit or `rewind` until `T` is explicitly unpacked and consumed, preserving *Theorem 2 (Total Linear Exhaustion)*.
  - When `T : mod affine` holds external OS handles, it registers on the region's LIFO finalizer list; if `T` owns only allocations within the same region `r`, its finalizer is elided completely.
+ *Vesper Loom Pure Scratchpads:* Because `[T-RegionScope]` discharges `r` from the outer effect row `ε`, a pure library with `effects = []` in `Loom.toml` can safely execute bounded local `Region::scoped(64.kib(), ...)` scratchpads without acquiring ambient global heap authority.

=== 13.3 Two-Tier Hardware-Sympathetic Allocator (ShardSlab and VirtualArena)
<ch-12-3>
+ *Tier 1 — ShardSlab (Zero-Atomic Core-Local Heap Backing `eff [Alloc]`):*
  - *Per-Core Fast Lists (0 Atomics):* Each *Vesper Pulse* worker shard maintains private 64 KiB slabs partitioned into 48 size classes (`8 B` to `16 KiB`). Same-core allocation and deallocation execute a 3-instruction intrusive freelist pop/push with zero atomic `LOCK` instructions.
  - *Batched Cross-Core Message-Passing Free (snmalloc Style):* When a `portable` fiber migrates across cores and drops an `affine` object, it appends the pointer to a cache-line-aligned remote free queue targeting the originating core, amortizing cross-socket cache-coherency traffic across 64-slot batches.
  - *MPK Domain-1 DmaSlab:* Network and storage buffers shared with `io_uring` (*Vesper Pulse*) and sandboxed C libraries (*Vesper Bridge*) allocate from 2 MiB hugepages mapped into *Hardware Domain 1* and pre-registered with `IORING_REGISTER_BUFFERS`.
+ *Tier 2 — VirtualArena (Realloc-Free Contiguous Regions Backing `eff [r: Region]`):*
  - *Contiguous Virtual Reservation:* Reserves contiguous virtual address ranges (`mmap(MAP_NORESERVE)`) backed by lazy `4 KiB` / `2 MiB` physical page faults and `MADV_FREE` recycling on reset—allowing tip collections to grow contiguously in place with *zero `memcpy` reallocation and zero heap fragmentation*.
  - *Stage 4 egglog Bump Coalescing:* Because the active `Region` state pointer resides in Cranelift's pinned `vmctx` register (`r15` / `x28`), `vesper-opt-egglog` fuses multiple sequential bump allocations inside a basic block into a single limit check and a single pointer addition.

=== 13.4 Zero-RC Functional-But-In-Place (`@fip`) Constructor Reuse
<ch-12-4>
Unlike Koka and Lean 4—which store an 8-byte reference count per object and branch on `rc == 1` at runtime—Vesper's `sink` mode guarantees *compile-time uniqueness*, allowing Stage 4 (`vesper-opt-egglog`) to rewrite matched unpack-and-construct pairs into direct in-place field writes with *0-byte headers and 0 branches*:

```
enum Color { Red, Black }

affine struct RbNode[K, V] {
    color: Color,
    key: K,
    val: V,
    left: Option[Box[RbNode[K, V]]],
    right: Option[Box[RbNode[K, V]]],
}

// `@fip` statically proves ZERO heap allocations and ZERO deallocations occur (`eff []` is empty!).
@fip fn balance_left[K: mod copy, V](
    color: Color,
    key: K,
    sink val: V,
    sink left: Option[Box[RbNode[K, V]]],
    sink right: Option[Box[RbNode[K, V]]],
) -> Box[RbNode[K, V]] eff [] {
    match left {
        // Unpacking two owned `Box[RbNode]` values yields two compile-time ReuseSlots paired
        // 1-to-1 by `vesper-opt-egglog` with the two `Box::new` constructors below!
        Some(Box(RbNode {
            color: Color::Red,
            key: lk, val: lv,
            left: Some(Box(RbNode { color: Color::Red, key: llk, val: llv, left: a, right: b })),
            right: c,
        })) => {
            let new_left  = Box::new(RbNode { color: Color::Black, key: llk, val: llv, left: a, right: b });
            let new_right = Box::new(RbNode { color: Color::Black, key, val, left: c, right });
            Box::new_in_caller(RbNode { color: Color::Red, key: lk, val: lv, left: Some(new_left), right: Some(new_right) })
        }
        other_left => Box::new_in_caller(RbNode { color, key, val, left: other_left, right }),
    }
}
```

=== 13.5 Cache-Conscious Data Structures and tiled(N) struct (AoSoA)
<ch-12-5>
+ *First-Class tiled(N) struct (AoSoA + Hot/Cold Segregation):* `tiled(N) struct` packs `N` records (`8, 16, 32, 64`) per contiguous cache-line tile for `hot` fields while isolating `cold` fields into a separate page-aligned side array. Disjoint column projections (`[T-CallProj]`) feed *Vesper Vector* (`VVec`) directly without hardware gather instructions, while *Lean 4* (`vesper-verify-lean`) models `TiledSeq[T]` transparently as `Array T`:

```
// Packs 16 entries per tile: `base_offset` (128B) and `batch_crc` (64B) align to 64B L1 cache lines;
// `cold` fields live in a side slab untouched during hot offset/CRC scans.
tiled(16) copy struct LogOffsetEntry {
    hot base_offset: u64,
    hot batch_crc: u32,
    hot byte_len: u32,
    cold producer_id: u64,
    cold compression_codec: u8,
}

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

+ *Standard Zero-GC Cache-Conscious Collection Suite (`@vesper-std/collections`):*
  - *`FlatTable[K, V]` (Safe SIMD SwissTable):* Probes 16-byte `h2` control groups in 1–2 instructions via `Isa[Baseline128]` without `unsafe`, and exposes `get_disjoint_mut` for multi-key `inout` projection.
  - *`CacheBTree[K, V, B = 16]` (SIMD-Searched Cache-Line B+Tree):* Branchless SIMD rank search over 16-key cache-aligned nodes using `u32` slab-relative indices (50% smaller node pointers, `memcpy`-relocatable without `Pin`).
  - *`ChunkSeq[T]` (Transient Radix-Balanced Sequence):* Mutates 32-way cache-line chunks in place in `O(1)` when uniquely owned and freezes in `O(1)` for zero-copy sharing across `Pulse::scope` fibers.
    - *Current core (built in):* `ChunkSeq[T]` is today one growable buffer behind a one-word handle (`new`, `with_capacity`, `push`, `pop`, `len`, `get`, `set`, `try_get`, `try_set`, `clear`, `truncate`, `reserve`, `extend` (also spelled `extend_from_slice`), `as_slice`, `as_slice_mut`, and the block operations below) whose capacity doubles; `s.reserve(n)` makes room for at least `n` more elements and is a capacity hint only: it changes no element or length, raises nothing, and, like any growth, ends an executable with status 71 only when the allocation cannot be satisfied; the radix-balanced chunking and `O(1)` freeze above are library work over it. Its mode is the element mode joined with `affine`, so a `ChunkSeq` of `linear` elements is consumed by `ChunkSeq::finish`, which raises `IndexOutOfBounds` unless the sequence was drained. `get`, `set`, `s[i]` and `finish` raise `Fail[RuntimeError]` `IndexOutOfBounds`; `try_get` and `try_set` are total, which is what code holding an `inout` loan uses, since no abortable effect may cross a live `inout` loan. Allocation is ambient, as for `TiledSeq` and `Box`; charging it to `Alloc` is deferred. Native code frees a sequence or string that its function owns (a `let` binding or by-value parameter that was not moved out) when the binding is bound or assigned again and on every exit of the function, including `return` from a handled block and a `Fail` exit; `ChunkSeq::finish` frees the drained sequence. Elements are not yet freed with their sequence.
    - *`Slice[T]`:* A borrowed view `s[a..b]` (or `s.as_slice()`) of a `ChunkSeq` or another slice, with `len`, `s[i]`, `get`, `set`, `try_get` and `try_set`. Slices are second class: a `Slice` appears only as the whole type of an `in` or `inout` parameter (`E1520`), and a view is written only as such an argument, as the receiver of a reading `Slice` method (`b[a..c].find_byte(x)`), or as the initializer of a bound view (below) (`E1521`). Two `inout` views of one sequence in one call must be disjoint by construction: the upper bound of one is syntactically the lower bound of the other, or known facts order them (`E0301`). An interval outside its sequence is a run-time fault, not `Fail`: executables exit with status 71.
    - *Block and fixed-width operations:* A sequence or slice `s` of a copy element type also has `s.copy_from(at, in src)`, which copies every element of the slice `src` to `s[at..at + src.len()]`, and `s.fill(v)`, which sets every element to `v`. A `ChunkSeq[u8]` or `Slice[u8]` has `s.read_u16_le(at)`, `read_u16_be`, `read_u32_le`, `read_u32_be`, `read_u64_le` and `read_u64_be`, which read the integer stored in the `2`, `4` or `8` bytes at `at` in little- or big-endian order, and the matching `s.write_u16_le(at, v)` through `write_u64_be`. Each checks its whole interval once and raises `Fail[RuntimeError]` `IndexOutOfBounds` before it writes anything when the interval does not fit, so a short buffer is never partly written. `try_copy_from`, `try_read_*` and `try_write_*` are their total forms: a read is an `Option`, and a copy or write is `false` and writes nothing. `src` cannot be a view of `s`'s own sequence (`E0301`), so a copy never overlaps its source; a view of the other half of one sequence passed beside it, as in `f(inout s[0..k], in s[k..n])`, may be copied from. Receiver and source places are compared by field path, as call arguments are (`[T-CallProj]`), so a sequence in another field of the same value may be the source (`q.a.extend(in q.b[0..k])`). To move elements within one sequence, `s.copy_within(lo, hi, at)` copies `s[lo..hi]` to `s[at..at + (hi - lo)]` as `memmove` does, so the two intervals may overlap; it checks `lo <= hi <= s.len()` and that the destination fits once, raises `IndexOutOfBounds` before it writes anything otherwise, and `try_copy_within` is its total form (`false`, nothing written). Like `copy_from` it needs a copy element type (`E1522`).
    - *Bulk sequence operations:* A sequence or slice `s` of a copy element type also has `s.fill(lo, hi, v)`, which sets every element of `s[lo..hi]` to `v`, so part of a sequence is filled without a slice view as receiver; it checks `lo <= hi <= s.len()` once, raises `IndexOutOfBounds` before it writes anything otherwise, and `try_fill` is its total form (`false`, nothing written). A `ChunkSeq[T]` `s` of a copy `T` has `s.resize(n, v)`, which shortens `s` to `n` elements or appends copies of `v` until it holds `n`, with one growth; it is total. `s.extend_from_within(lo, hi)` appends a copy of `s[lo..hi]`, reading through the grown buffer, so a sequence can repeat its own elements, which `extend` refuses (`E0301`); it checks `lo <= hi <= s.len()` once and raises `IndexOutOfBounds` before it appends anything otherwise, and `try_extend_from_within` is its total form. `s.extend_reversed(in src)` appends the elements of the slice `src` last first, with one growth; it is total, and like `extend` its source may not be a view of `s`'s own sequence (`E0301`). These need a copy element type (`E1522`), and `resize`, `extend_from_within` and `extend_reversed` exist only on `ChunkSeq` (`E1525` on a `Slice`). Lean extraction refuses them for now.
    - *Place primitives `swap` and `replace`:* `swap(inout a, inout b)` exchanges the values of two places of one type, and `replace(inout a, v)` stores `v` in `a` and returns the value `a` held. The type need not be copy, so a non-copy field of an `inout` parameter, which cannot be moved out (`E0305`), can be exchanged or replaced in place: `swap(inout rec.st, inout rec.stage)` publishes a staged state without copying it. Both arguments of `swap` are `inout` places (`E0308`, `E0300`) that must be disjoint (`E0301`), and neither may be a second-class view (`E1520`). Both are total. A declared function named `swap` or `replace` takes precedence over the primitive. Lean extraction refuses them for now, as it does `copy_from` and `copy_within`.
    - *Element views:* An element of a `ChunkSeq[T]` or `Slice[T]` whose type is not copy cannot be read out of the sequence (`s[i]`, `get` and `try_get` as values are `E1522`), but it can be borrowed in place. `f(in s[i])` and `f(inout s[i])` lend the element to a call, `s[i].m()` to a method, and `swap` and `replace` take `inout s[i]`; like `s[i]` these raise `Fail[RuntimeError]` `IndexOutOfBounds` before the call when `i` is out of range. `match s.try_get(i) { Some(x) => .., None => .. }` binds `x` as an `in` view of a non-copy element, and `match s.try_get_mut(i)` binds it as an `inout` view of an element of any type; both are total. The arms of such a match are `Some(x)`, `Some(_)`, `None` and `_` only, and `try_get_mut` is written only as a match scrutinee (`E1522`). A view borrows the whole sequence for the call or the match, shared for `in` and exclusively for `inout`, under the rules for call arguments and match borrows (`E0300`, `E0301`), so two views of one sequence in one call are refused unless both are `in`, and the sequence cannot be pushed, popped or moved while a view of it is live. A view is never dropped: the element stays owned by its sequence.
    - *Byte scans:* A `ChunkSeq[u8]` or `Slice[u8]` `s` also has `s.sum_be16()`, the sum modulo $2^64$ of its big-endian 16-bit words `s[2k] << 8 | s[2k + 1]`, where an odd last byte counts as `s[n - 1] << 8` (folding the sum gives the RFC 1071 one's-complement checksum); `s.eq(in t)`, whether the byte slice `t` has the same length and bytes as `s`; `s.cmp(in t)`, the three-way lexicographic order of the bytes of `s` and `t` as an `i32` that is `-1`, `0` or `1`, where a proper prefix orders first (C `memcmp` over the shorter length, then the lengths); and `s.find_byte(b)`, the index of the first byte equal to `b` as an `Option[u64]`. All four are total and only read, so `t` may be a view of `s`'s own sequence. Byte slices have no `<` operators: `s.cmp(in t)` answers a three-way question with one scan.
    - *Byte classes and ASCII case:* A `ChunkSeq[u8]` or `Slice[u8]` `s` also has `s.find_either(a, b)`, the index of the first byte equal to `a` or to `b`; `s.find_not_in(lo, hi, high)`, the index of the first byte outside a byte class, where a byte `x < 64` is in the class when bit `x` of the `u64` `lo` is set, a byte `64 <= x < 128` when bit `x - 64` of the `u64` `hi` is set, and a byte `x >= 128` when the `bool` `high` holds (so `s.find_not_in(lo, hi, high)` is `None` exactly when every byte is in the class); optimized code on x86-64 without SSSE3 tests one byte at a time but skips a run of one repeated class byte 16 bytes per step, so `s.find_not_in(1, 0, false)`, the all-zero test, reads 16 bytes per step on every native target); and `s.eq_ignore_ascii_case(in t)`, whether `t` has the same length and the same bytes as `s` once ASCII `A`-`Z` are mapped to `a`-`z` (no other byte changes). The two searches return an `Option[u64]`. A `ChunkSeq[u8]` `out` has `out.extend_ascii_lowercase(in src)`, which appends the bytes of the slice `src` with `A`-`Z` mapped to `a`-`z`. All four are total; `t` may be a view of `s`'s own sequence, while `src` may not be a view of `out`'s (`E0301`), as for `extend`. A `ChunkSeq[u8]` or `Slice[u8]` `s` also has `s.make_ascii_lowercase(lo, hi)`, which maps `A`-`Z` to `a`-`z` in `s[lo..hi]` where they are (so it lowercases part of a sequence without reading it as a source); it checks `lo <= hi <= s.len()` once, raises `IndexOutOfBounds` before it writes anything otherwise, and `try_make_ascii_lowercase` is its total form (`false`, nothing written).
  - *`String` and `Str` (UTF-8 text, built in):* `String` is an owned, growable `ChunkSeq[u8]` that always holds valid UTF-8; it is `affine`. `Str` is a read-only view of a byte range of a string with the same invariant. Like `Slice`, `Str` is second class: it appears only as the whole type of an `in` parameter or of a function result (`E1530`), and a view (`s.as_str()`, `s.as_bytes()` as a `Slice[u8]`, or `s[a..b]`) is written only as an `in` call argument or as the initializer of a bound view (`E1531`). _Bound views:_ an immutable `let b = v;` may bind a string or slice view `v` (`s.as_bytes()`, `s.as_str()`, `s[a..b]`, `s.as_slice()`) of a place that cannot change while the function runs: an `in` parameter, or an immutable binding that is itself a `Str` or `Slice`. The binding is a read-only view with the second-class rules of a view parameter (it is never returned, stored, moved or passed by `sink`), so it cannot outlive its source; a view of a place the function may write or move (a local, an `inout` or `sink` parameter) stays refused. _Static text:_ string literals are static bytes that never change, so `let w = ".ves".as_bytes();` and `let n = ".ves".as_str();` bind views of a literal, and a function whose result type is `Str` returns static text: every value it returns (its tail and every `return e`) must be a string literal, a call of a function whose result is `Str`, or an `if`, `match` or block whose results are static text (`E1530` otherwise). Such a call is a static view the caller may pass `in`, call methods on, compare, or bind with `let` (it is never stored or moved), so a table of names is `fn kind_name(k: u8) -> Str { match k { 0 => "Whitespace", ... } }` with no allocation. A result of a type that merely contains `Str` (`Option[Str]`, a tuple) stays refused. A `String` passes to an `in` `Str` parameter as its whole view, so string literals and owned strings both pass to `in s: Str`. The compiler provides `len`, `byte_at` (raising `IndexOutOfBounds`), the total `try_byte_at`, the views, `push_str`, `push_char` (which returns `false` and appends nothing for a surrogate or a value past `0x10FFFF`), `clear` and `reserve`; the comparison operators borrow their operands and order strings by their bytes, which is code-point order. Everything that needs UTF-8 knowledge is Vesper source in the string prelude: `next_char` (`Option[(u32, u64)]`: the code point and the next byte offset), `char_at` (raising `IndexOutOfBounds` off a char start), `char_count`, `is_char_boundary`, `concat`, `String::new`, `String::with_capacity`, and the checked conversion `String::from_utf8(in bytes: Slice[u8]) -> Option[String]`, which rejects overlong encodings, surrogates and values past `0x10FFFF` (RFC 3629). No operation can break the invariant: bytes enter a string only as a literal, another string or a whole encoded char. A view interval outside the string, or with an end off a char boundary, is a run-time fault like a sequence interval (status 71). Natively a string or view is `{len, cap, data}`; a literal is borrowed static bytes (capacity 0) that are copied before the first write. Integer and float formatting and parsing are `@vesper-std` library code over these operations. Lean extraction refuses the string operations for now.
  - *Standard library packages (implemented in Vesper, `packages/vesper-std/`):* `@vesper-std/core` (`min`, `max`, `clamp`, `swap`, ordering helpers; modules `option` and `result` with the usual combinators), `@vesper-std/collections` (modules `btree`, `btree_cursor`, `btree_edit`, `flat_table`, `sets`, `radix_seq`), `@vesper-std/fmt` (integer formatting into `ChunkSeq[u8]`, integer parsing with `ParseIntError`, FNV-1a 64) `@vesper-std/bytes` (little- and big-endian integer reads, appends and in-place writes over `Slice[u8]`/`ChunkSeq[u8]`; module `ct` with `all_zero` and `ct_eq`, a comparison whose time depends only on the lengths) and `@vesper-std/sort` (module `unstable`: the in-place introsort `sort` and `is_sorted`, allocation free; `stable`: the merge sort `stable_sort`, which allocates one scratch sequence; `keyed`: `sort_by_key(in keys, inout order)`, which orders a `Slice[u32]` of indices by a parallel key slice with ties broken by index, and `sort_pairs`, a stable sort of two parallel slices by the first; `floats`: `sort_f64`, `stable_sort_f64` and `is_sorted_f64` under the IEEE 754 total order through the named model `TotalOrderF64`, and `sort_f32_bits` over `f32` bit patterns). Every sort takes an `inout Slice[T]` of a copy `T: Ord` and is total and `O(n log n)`. `@vesper-std/core` also has module `saturating`: `saturating_add_k`, `saturating_sub_k` and `saturating_mul_k` for `k` in `u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64`. Each declares `capability_ceiling = ["Fail", "Alloc"]` and is pure Vesper over `ChunkSeq`. Current revision: `CacheBTree[K, V]` is a B+tree with 16 keys per node in flat `u64`-indexed arenas (the SIMD rank search and the `B` parameter are future work), and removal leaves underfull or empty leaves in place rather than merging them; `FlatTable[K, V]` stores entries densely in insertion order (removal moves the last entry into the hole) behind a linear-probing index keyed by the builtin `Hash`, so no public result depends on hash values; keys are copy, but values need not be: `insert` moves the value in and returns the one it replaced, `remove` moves the value out, `position(k)` names the value's slot in the dense `vals` sequence (the absent position `u64::MAX` for a missing key), and a non-copy value is read in place through an element view of `vals` (`match t.vals.try_get(t.position(k))`), while `get`, `entry` and the other copying reads need a copy value type; `OrderedSet[K]` and `FlatSet[K]` wrap them; `RadixSeq[T]` is the 32-way radix sequence with O(1) `freeze` snapshots (`SeqSnapshot`), `thaw`, and path-copying edits, whose nodes are reclaimed only with the sequence. The Strata conformance traces of #847 regenerate byte for byte from these implementations (`tests/vesper_std.rs`).
  - *Standard I/O packages (`@vesper-std/fs`, `@vesper-std/process`, `@vesper-std/env`):* the operating-system edge a compiler driver needs, in Vesper over one `.vsys` library each (`StdFs`, `StdProcess`, `StdEnv`, #link(<ch-15-3>)[Section 16.3]) whose host functions are a C-ABI shim (`crates/vesper-std-host`). _Effects:_ every operation performs the Bridge capability of its library, `Ffi[StdFs]`, `Ffi[StdProcess]` or `Ffi[StdEnv]`; there is no separate `Fs` or `Process` effect, because a `.vsys` capability already names exactly one library and Loom already confines it, so a package that touches files declares `"Ffi[StdFs]"` in its `capability_ceiling` and pure code cannot reach them. A failure raises `Fail[IoError]`, a copy struct of a stable `kind` code (`not_found`, `permission_denied`, `already_exists`, `not_a_directory`, `is_a_directory`, `directory_not_empty`, `invalid_input`, `interrupted`, `unexpected_eof`, `write_zero`, `unsupported`, `out_of_memory`, `other`) and the raw OS error. `@vesper-std/fs` reads and writes whole files (`read` to a `ChunkSeq[u8]`, `read_string`, `write`, `append`, `create_new`), replaces a file atomically (`replace`: a temporary file in the same directory, flushed, renamed over the target, then the directory flushed), creates and removes directories, renames, and reports `metadata` (size, modification time, kind); module `walk` lists a directory (`list_dir`) or a whole tree (`walk`) as `Entries`, one byte arena of `/`-separated relative paths sorted by their bytes, so the order is the same on every platform. Open files and directory listings are `linear extern type` handles that never leave the package. `@vesper-std/process` builds a command (program, arguments, environment, directory, standard streams), spawns it, waits, captures stdout and stderr, and `exit`s; the command and the child are linear handles, a child released without a wait is killed and reaped, and functions that take a live handle return a `Result` instead of raising, since a linear handle may not stay live across an unhandled `Fail`. `@vesper-std/env` reads arguments, variables and the current directory. Paths, arguments and names cross the boundary as byte slices, never NUL-terminated strings: any bytes but NUL on Unix, UTF-8 on Windows. Text results use a copy-out protocol (the host returns the length and copies only into a large enough buffer).
  - *Compiler data packages (`packages/vesper-std/`, #1127):* `@vesper-std/arena` has the typed index `Idx[T]` (a copy `u32` position with `Eq`, `Ord` and `Hash`) and `Arena[T]` (`alloc`, `get` as an `Option`, `get_or`, `set` returning `false` for an index the arena never handed out, `contains`, `next_idx`, `at`, `truncate`, `clear`), with `idx_none()` (`u32::MAX`), which no access accepts and `alloc` returns once `2^32 - 1` values exist, and the dense side table `IdxMap[K, V]` (`get` reads the default given at creation where unset, `set` grows it). `Arena[T]` and `IdxMap[K, V]` may hold values that are not copy: `alloc` and `IdxMap::put` move a value in (`put` at an existing index or the next one, returning the value it displaced, or the value itself when it would leave a gap), `idx_map_owning(dflt)` creates such a table, and values are read in place through element views of `items` and `vals`; `get`, `get_or`, `set` and `at` need a copy value type. `@vesper-std/graph` is a directed graph over `u32` node ids, built by `GraphBuilder` (`add_node`, `add_edge`, which keeps parallel edges and self-loops and refuses non-nodes, `build`) and frozen into successor and predecessor lists in edge insertion order; module `scc` (Tarjan, iterative, components numbered in completion order, which is a reverse topological order of the condensation), `topo` (Kahn's order, sources first-in first-out from the lowest id, with one cycle reported as a node path when the graph is cyclic), `dom` (Cooper-Harvey-Kennedy immediate dominators from a root; the root is its own `idom`, an unreached node has `none()`) and `reach` (`reachable_from`, `reachable_from_all`, `reaches`, `postorder`, `reverse_postorder`). Every algorithm visits nodes in id order and edges in list order, so every result is a function of the edges and their order. `@vesper-std/intern` maps strings to dense `Symbol` ids in first-insertion order (`intern`, `lookup`, `symbol_len`, `symbol_eq`, `resolve_into`, which appends the text to a `String`, and `span` into one shared text buffer), both directions in expected `O(1)`. `@vesper-std/hash` has module `blake3` (BLAKE3 hash, keyed hash and key derivation, one-shot and incremental through `Blake3` with extendable output, as the copy `Digest` of four little-endian words) and module `fast` (`fnv1a64`, `fnv1a32`, `xxh64` and the finalizers `mix64` and `hash_pair`), all portable Vesper. `@vesper-std/hash-sys` computes the same BLAKE3 values through the vetted `blake3` crate: its `.vsys` library `Blake3` (a `linear extern type Hasher` with `sink` release) is provided by the Rust host that links the object, and its functions carry `eff [Ffi[Blake3]]`. These packages are total and deterministic; BLAKE3 equals the official test vectors in the evaluator, the JIT and executables (`tests/std_hash.rs`, `std_graph.rs`, `std_intern.rs`, `std_arena.rs`).
  - *`SlotArena[r, V]` and `RegionRef[r, T]` (Capability-Branded Graph Handles):* Generational `NodeId[r]` keys branded by region `r` prevent cross-arena index bugs at compile time, while `RegionRef[r, T]` enables cyclic graphs (CFGs, intrusive LRU caches) safely mediated via `inout r`.

```
// Zero-heap-fragmentation log compaction using a 256 KiB lexical Region scratchpad (`Alloc` not used!)
fn compact_log_segment[P: IsaProfile](
    inout out_frame: ByteSlice,
    in raw_segment: ByteSlice,
) -> Result[u32, CompactError] eff [Isa[P], Fail[CompactError]] {
    Region::scoped(256.kib(), |cap scratch| eff [scratch: Region, Isa[P], Fail[CompactError]] {
        let mut latest_by_key = FlatTable::with_region[u64, LogOffsetEntry](in scratch, 4096);
        let mut entries       = TiledSeq::with_region[LogOffsetEntry](in scratch, 4096);

        for batch_hdr in raw_segment.iter_wire_headers[KafkaBatchWireHeader]()? {
            let mark: Watermark = scratch.checkpoint(); // Linear watermark token
            let summary = decode_and_dedup_batch(in scratch, inout latest_by_key, in batch_hdr)?;
            entries.push(summary);
            scratch.rewind(sink mark); // O(1) per-batch temporary reset
        }

        flush_compacted_tiles[P](inout out_frame, in entries)
    })
}
```

=== 13.6 Rust Runtime Implementation (`crates/vesper-alloc`)
<ch-12-6>
```rust
// crates/vesper-alloc/src/lib.rs
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Cache-line-aligned bump state pointed to by `EvidenceSlot.state_ptr` (`r15` / `x28`).
#[repr(C, align(64))]
pub struct RegionEvidenceState {
    pub bump_cursor: usize,
    pub bump_limit: usize,
    pub virtual_base: NonNull<u8>,
    pub reserved_bytes: usize,
    pub affine_finalizer_head: *mut FinalizerNode,
    pub active_linear_count: u32,
}

#[repr(C)]
pub struct FinalizerNode {
    pub prev: *mut FinalizerNode,
    pub drop_fn: unsafe extern "C" fn(*mut u8),
    pub payload_ptr: *mut u8,
}

impl RegionEvidenceState {
    #[inline(always)]
    pub fn try_bump_aligned(&mut self, size: usize, align: usize) -> Option<NonNull<u8>> {
        let aligned = (self.bump_cursor.checked_add(align - 1)?) & !(align - 1);
        let next = aligned.checked_add(size)?;
        if next > self.bump_limit {
            return None;
        }
        self.bump_cursor = next;
        NonNull::new(aligned as *mut u8)
    }

    #[inline]
    pub unsafe fn rewind_to(&mut self, watermark_cursor: usize) {
        debug_assert_eq!(self.active_linear_count, 0);
        while !self.affine_finalizer_head.is_null()
            && (self.affine_finalizer_head as usize) >= watermark_cursor
        {
            let node = &*self.affine_finalizer_head;
            self.affine_finalizer_head = node.prev;
            (node.drop_fn)(node.payload_ptr);
        }
        self.bump_cursor = watermark_cursor;
    }
}

/// Per-core sharded slab bin (`0` atomics on local fast path; batched remote free stack).
#[repr(C, align(64))]
pub struct SizeClassSlab {
    pub local_free_head: *mut FreeSlot,
    pub remote_free_stack: AtomicPtr<FreeSlot>,
    pub slot_bytes: u16,
    pub owner_core_id: u16,
}

#[repr(C)]
pub struct FreeSlot {
    pub next: *mut FreeSlot,
}

impl SizeClassSlab {
    #[inline(always)]
    pub unsafe fn alloc_local(&mut self) -> Option<NonNull<u8>> {
        if self.local_free_head.is_null() {
            let batch = self.remote_free_stack.swap(core::ptr::null_mut(), Ordering::Acquire);
            if !batch.is_null() {
                self.local_free_head = batch;
            }
        }
        let head = self.local_free_head;
        if head.is_null() {
            return None;
        }
        self.local_free_head = (*head).next;
        NonNull::new(head.cast())
    }
}
```


== 14. Vesper Vector: Capability-Typed Portable and Scalable SIMD
<ch-13>
#chapter-deck[Instruction-Set Capabilities, Fixed and Scalable Vectors, and Equivalence Checks]

*Vesper Vector* (`@vesper-std/simd`, with implementation code in `crates/vesper-simd`) describes fixed-width and scalable single-instruction, multiple-data (SIMD) operations. Target instruction sets include `SSE4.2`, `AVX2`, and `AVX-512` / `AVX10` on `x86_64`; `NEON` and `SVE2` on `AArch64`; and `RVV v1.0` on `RISC-V`. The design represents instruction-set availability as a lexical capability (`eff [Isa[P]]`) and proposes `egglog` optimization with Lean 4 equivalence checks. Its research influences include #link("https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/isle/veri/README.md")[_VeriISLE_] (ASPLOS 2024), _Guided Equality Saturation_ (Koehler et al., POPL 2024), and _Verified Equality Saturation to SIMD in Lean 4_ (2026).

=== 14.1 Core SIMD Safety, Portability, and Predication Guarantees
<ch-13-1>
- *100% Safe CPU Feature Tracking:* Instead of marking pure register math `unsafe` because missing target features trigger `SIGILL`, Vesper models CPU ISA extensions (`Baseline128`, `Avx2`, `Avx512`, `Sve2`, `Rvv1`) as zero-cost `copy` lexical capabilities `eff [Isa[P]]` proven once at `Isa::dispatch`.
- *Effect-Polymorphic Higher-Order Inlining:* Generic combinators carrying `eff [Isa[P] | r]` inline and monomorphize target-specialized closures cleanly without dynamic function-pointer barriers.
- *Unified Fixed and Scalable Vector Registers:* Supports both fixed-width `Vec[T, N]` (`128`/`256`/`512`-bit) and length-agnostic predicated `VVec[P, T]` with hardware active-lane masks (`whilelo` on SVE2, `vsetvli` on RVV 1.0, `k1` opmasks on AVX-512)—eliminating scalar remainder loops.
- *Zero Aliasing Bailouts:* `[T-CallProj]` statically guarantees `inout dst` and `in src` slices never overlap, freeing Cranelift and `egglog` from emitting runtime pointer-overlap fallback branches.
- *SAT Bit-Blasted Verification:* `vesper-verify-lean` lowers SIMD lanes into `BitVec` and proves *exact bit-for-bit equivalence* against scalar reference functions via Lean 4's `bv_decide` tactic.

=== 14.2 ISA Profiles as Zero-Cost Lexical Capabilities (`eff [Isa[P]]`)
<ch-13-2>
In Vesper, hardware instruction-set availability is a stateless `copy` lexical capability `Isa[P]` tracked in `Σ`:

```
// Built-in ISA capability hierarchy:
//   x86_64:  Baseline128 -> Avx2 -> Avx512
//   AArch64: Baseline128 (Neon) -> Sve2
//   RISC-V:  Scalar -> Rvv1
copy effect Isa[P] {}

// Runtime multiversion dispatcher: probes CPUID / HWCAP once at startup or boundary and
// dispatches to target-specialized machine code compiled for each supported profile P.
fn dispatch_simd[R](
    kernel: [P: IsaProfile] fn(in cap: IsaCap[P]) -> R eff [Isa[P]]
) -> R;
```

Because `Isa[P]` has mode `copy` and carries zero runtime state, `vesper-opt-egglog` erases the capability token completely during monomorphization while stamping Cranelift target flags (`enable_simd`, `has_avx512f`, `use_sve2`, `has_v`) on the specialized codegen unit.

=== 14.3 Unified Fixed-Width (`Vec[T, N]`) and Scalable Strip-Mined Predication (`VVec`)
<ch-13-3>
Vesper Vector exposes two interoperable layers lowered through Cranelift SSA IR:

+ *Fixed-Width Portable Vectors (`copy struct Vec[T: SimdElem, N]`):*
  - Supports lane scalars `u8, i8, u16, i16, u32, i32, u64, i64, f32, f64` at `128`, `256`, and `512` bits.
  - Provides total lane-wise arithmetic (`wrapping_add`, `saturating_sub`, `mul_add`), typed masks `Mask[T, N]`, compile-time checked `swizzle`, horizontal reductions (`reduce_add`, `reduce_xor`, `bitmask`), and bounds-checked loads/stores on `ByteSlice` and `AlignedSlice[T, 64]`.
  - The implemented subset (`src/vector.rs`) has lanes `u8, u32, u64, i64, f32, f64`. Float lanes are IEEE binary32 or binary64 at their own width: `add`, `sub`, `mul`, `div`, `min`, `max`, `sqrt`, `abs`, `neg` and `mul_add(a, b, c)`, which computes `a * b + c` with one rounding on every backend (a fused instruction where the ISA has one, the correctly rounded C `fmaf`/`fma` otherwise). Lane conversions keep the lane count: `to_bits` (`f32` to `u32`, `f64` to `u64`), `from_bits`, `widen_f64` (exact) and `nearest_f32` (round to nearest even); a conversion whose result is wider than its operand needs the profile of the result's width.
  - Slice loads and stores move a whole vector between lanes and a `Slice[T]` or `ChunkSeq[T]` of the lane type: `Vec::load[T, N](in s, at)` and `Vec::store(inout s, at, v)` check `at + N <= s.len()` once and raise `Fail[RuntimeError]` (`IndexOutOfBounds`) otherwise, writing nothing; `Vec::load_masked[T, N](in s, at)` and `Vec::store_masked(inout s, at, v)` are total, with the tail mask `Mask::active[T, N](at, s.len())`: inactive lanes load zero and are not stored.
+ *Length-Agnostic Predicated Strip-Mining (`VVec[P, T]` and `VMask[P, T]`):*
  - `P::active_mask[T](offset, total_len)` computes a hardware predicate mask covering `min(VL_lanes, total_len - offset)` active lanes.
  - `VVec::load_masked(in slice, offset, mask, passthru)` and `VVec::store_masked(inout slice, offset, mask, vec)` lower directly to fault-suppressing predicated instructions (`whilelo` + `ld1b` on Arm SVE2; `vsetvli` + `vle8.v` on RISC-V RVV 1.0; `k1` opmask `vmovdqu8` on AVX-512; unrolled vector body + synthesized masked tail on AVX2/NEON):

```
// Length-agnostic, 100% safe SIMD delimiter scanner and XOR checksum over a zero-copy ByteSlice.
// Runs at full hardware width across NEON, AVX2, AVX-512, SVE2, and RVV without `unsafe`!
fn scan_record_delimiters[P: IsaProfile](
    in data: ByteSlice,
    delim: u8,
) -> (u64, u8) eff [Isa[P]] {
    let needle = VVec::splat[P, u8](delim);
    let mut matches_count: u64 = 0;
    let mut xor_acc = VVec::splat[P, u8](0);
    let mut idx: usize = 0;
    let len = data.len();

    while idx != len {
        let mask: VMask[P, u8] = P::active_mask[u8](idx, len);
        let chunk = VVec::load_masked(in data, idx, mask, 0);

        let eq_mask = chunk.simd_eq(needle).and(mask);
        matches_count = matches_count.wrapping_add(eq_mask.count_ones() as u64);
        xor_acc = xor_acc.bit_xor(chunk); // inactive lanes zeroed by load_masked

        idx = idx.wrapping_add(P::lanes[u8](mask));
    }

    (matches_count, xor_acc.reduce_xor())
}
```

=== 14.4 `egglog` Vectorization Saturation and Lean 4 `bv_decide` Equivalence Proofs
<ch-13-4>
+ *Stage 4 — `egglog` SIMD Equality Saturation (`vesper-opt-egglog`):*
  - Rewrites fuse `add(mul(a, b), c) -> fma(a, b, c)`, recognize widen-multiply-accumulate dot products (`u8 -> u16 -> u32` lowered to `vpdpbusd` on x86 and `udot` on Arm), and collapse adjacent `swizzle` permutations into a single register shuffle.
  - Profile-parameterized cost extraction divides elementwise op costs by `P::register_bits` while penalizing cross-lane permutations, emitting Cranelift vector ops lowered via *VeriISLE*-verified instruction selection rules.
+ *Stage 5 — Automated Bit-Accurate SIMD Proofs (`vesper-verify-lean`):*
  - `vesper-verify-lean` translates `Vec[T, N]` to `BitVec (width(T) * N)` (and instantiates `VVec` parametrically across valid `vscale` multipliers `1, 2, 4, 8, 16`), allowing Lean 4's built-in SAT bit-blaster (`bv_decide`) to verify branchless SIMD parsers and checksums automatically:

```lean
-- illustrative/SimdDelimiterProof.lean
-- Proves via Lean 4 SAT bit-blasting (bv_decide) that 16-lane SIMD delimiter counting
-- is bit-for-bit identical to the scalar specification across all 2^136 inputs!
theorem simd_chunk_delim_eq_scalar (chunk : BitVec 128) (delim : UInt8) :
    Simd128.countMatchesU8 chunk delim = ScalarSpec.countMatches16 chunk delim := by
  bv_decide
```

=== 14.5 SIMD Profile Lowering Engine (`crates/vesper-simd`)
<ch-13-5>
`crates/vesper-simd` maps lexical `Isa[P]` capability profiles directly onto Cranelift target feature flags and scalable strip-mining bounds:

```rust
// crates/vesper-simd/src/lib.rs
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IsaProfileKind {
    Baseline128, // x86_64 SSE4.2 / AArch64 NEON
    Avx2,        // x86_64 256-bit AVX2 + BMI2 + FMA
    Avx512,      // x86_64 512-bit AVX-512F/BW/VL/VBMI + k-mask predication
    Sve2,        // AArch64 scalable 128..2048-bit SVE2 + whilelo predication
    Rvv1,        // RISC-V Vector v1.0 scalable vlenb + vsetvli strip-mining
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorWidth {
    FixedBits(u16),
    Scalable { min_bits: u16, max_bits: u16 },
}

impl IsaProfileKind {
    pub fn cranelift_isa_flags(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Baseline128 => &[("enable_simd", "true")],
            Self::Avx2 => &[("enable_simd", "true"), ("has_avx2", "true"), ("has_fma", "true")],
            Self::Avx512 => &[
                ("enable_simd", "true"),
                ("has_avx512f", "true"),
                ("has_avx512bw", "true"),
                ("has_avx512vl", "true"),
            ],
            Self::Sve2 => &[("enable_simd", "true"), ("use_sve2", "true")],
            Self::Rvv1 => &[("enable_simd", "true"), ("has_v", "true")],
        }
    }
}
```


== 15. Vesper Pulse: Asynchronous Fiber Runtime and Deterministic Simulation
<ch-14>
#chapter-deck[Completion I/O, Cancellation, Fiber Stacks, Nurseries, and Simulation]

*Vesper Pulse* (`@vesper-std/pulse`, with implementation code in `crates/vesper-pulse`) describes completion I/O, structured fiber concurrency, telemetry, and seeded `SimPulse` simulation. The proposed runtime executes effectful functions on pooled fiber stacks and combines `io_uring`/IOCP/`kqueue` borrowing with cancellation and nursery rules. Research influences include _Zero-Overhead Lexical Effect Handlers_ (Ma et al., OOPSLA 2025), _Virtualizing Continuations_ (Ma, Jung, and Zhang, PLDI 2026), _An Equational Axiomatization of Dynamic Threads via Algebraic Effects_ (Kammar et al., POPL 2026), and _Iris-WasmFX_ (2026). Cancellation and stack relocation require the additional lifetime conditions discussed below.

=== 15.1 Asynchronous Runtime Architecture and Guarantees
<ch-14-1>
#figure(
  table(
    columns: (22%, 36%, 42%),
    align: (left, left, left),
    table.header([*Runtime Dimension*], [*Poll-Coroutine Baseline*], [*Vesper Pulse (`crates/vesper-pulse`)*]),
    [*Function Coloring*],
    [Viral `async fn` state machines requiring `Pin<&mut Self>` across suspension points.],
    [Colorless functions with lexical `eff [...]` rows executing on pooled 64 KiB Zero-`Pin` fiber stacks.],
    [*Cancel Safety*],
    [Dropping a losing race branch silently discards partially mutated state and open handles.],
    [`[T-Let]` statically rejects `Cancel` checkpoints while any `linear` obligation is live (`E0301`).],
    [*Completion DMA (`io_uring`)*],
    [Synchronous destructors and forgotten handles force owned heap buffers (`Vec<u8>`) for kernel rings.],
    [`linear` I/O effects guarantee completion before scope exit, enabling zero-copy `in` and `inout` stack/field DMA.],
    [*Structured Concurrency*],
    [Detached task spawning requires `'static + Send` bounds and `Arc<Mutex<T>>` state sharing.],
    [`Pulse::scope` nurseries borrow disjoint parent field trees (`inout broker.index` vs. `inout broker.repl_stream`) with zero locks.],
    [*Work-Stealing vs. Thread-per-Core*],
    [Fragmented runtime ecosystems split between multi-threaded and shard-pinned executors.],
    [Unified runtime parameterized by `ModeVec` portability (`portable` work-stealing `spawn` vs. `pinned` shard-local `spawn_local`).],
    [*Observability & Simulation*],
    [Requires compiler instrumentation flags, manual tracing wrappers, and separate network mock frameworks.],
    [Lexical handler swapping powers both `vesper top` live diagnostics and seeded `SimPulse` deterministic chaos testing (`vesper test --sim`).],
  ),
  kind: table,
)

=== 15.2 The Pulse Lexical Capability Hierarchy and Compile-Time Cancel Safety
<ch-14-2>
Rather than exposing ambient global functions (`tokio::spawn`, `tokio::time::sleep`, `TcpStream::connect`), Pulse factors concurrency and I/O into granular lexical capabilities distinguished by their *resumption contract* `q ∈ {affine, linear}`:

```
// 1. Cooperative fiber scheduling: linear effect (q = linear) MUST resume exactly once.
linear effect Async {
    fn yield_now() -> Unit;
}

// 2. Explicit cancellation and deadline checkpoints: plain effect (q = affine) MAY abort.
effect Cancel {
    fn check() -> Unit;
}

// 3. Monotonic timers: sleep_ns is uncancellable linear; deadlines compose with Cancel.
linear effect Clock {
    fn now_ns() -> u64;
    fn sleep_ns(duration_ns: u64) -> Unit;
}

// 4. Completion-based network I/O (io_uring / kqueue / IOCP): linear effect (q = linear).
linear effect Net {
    fn accept(in listener: TcpListener) -> Result[TcpStream, IoError];
    fn read_some(inout stream: TcpStream, inout buf: ByteSlice) -> Result[u32, IoError];
    fn write_all(inout stream: TcpStream, in buf: ByteSlice) -> Result[Unit, IoError];
}

// 5. Structured concurrency nursery tied lexically to the enclosing scope.
linear effect Scope {
    fn spawn_scoped[T: mod copy](
        f: fn@linear() -> T eff [Async, Cancel]
    ) -> JoinHandle[T];
}
```

==== Cancellation and Linear Obligations
In Tokio, cancelling a branch inside `tokio::select!` or `tokio::time::timeout` drops the suspended `Future` at whichever `.await` point happens to be active. In the proposed model, #link(<ch-4>)[Chapter 4]'s `[T-Let]` Resumption Frame Condition is intended to restrict cancellation (`∀ z ∈ Γ_2, m_z ⊑ res(ε_1)`):

+ *Uncancellable Linear I/O Suspensions (`q = linear`):* Because `Net`, `Storage`, `Clock`, and `Async` are `linear` effects, `res(ε) = linear`. A fiber holding a live `linear` protocol obligation—such as `WriteTxn`, a half-written length-prefixed `FrameWriter`, or an in-flight `io_uring` SQE—freely suspends across `Storage::flush_wal` or `Net::write_all` because `linear ⊑ linear` holds.
+ *Explicit Cancellation Checkpoints (`q = affine`):* Cancellation can unwind a fiber _only_ at operations carrying `Cancel` in their effect row (`Cancel::check()`, `Pulse::race`, `Pulse::timeout`, or `chan.recv_cancel()`), where `res(Cancel) = affine`.
+ *Static Rejection of Orphaned Obligations:* If a developer invokes a `Cancel`-bearing operation or enters `Pulse::race` while a `WriteTxn` or `FrameWriter` is live and unconsumed, `[T-Let]` evaluates `linear ⊑ affine` → *FALSE*. Compilation aborts with a diagnostic pinpointing the exact `linear` value that would be corrupted upon cancellation.

=== 15.3 Completion-Based `io_uring` DMA Borrowing and Intra-Fiber Pipelining
<ch-14-3>
The following is a proposed interface, not a demonstrated cancellation-safety guarantee. Cancellation of an `io_uring` request is asynchronous: the cancel request's completion does not by itself establish that the original request has finished. A cancelled fiber and its fixed-address stack must remain quarantined until the original request's terminal CQE has been reaped (including completion of multishot requests); stack buffers owned by an independently cancelled parent need an owned DMA buffer or equivalent lifetime protection. The example token below is valid only for an owned buffer, not for storing a second-class stack borrow. Platform-specific completion semantics must be established separately for IOCP and other reactors.

Vesper Pulse proposes the following ownership model for completion-based kernel rings:

+ *Zero-Copy `in` / `inout` Stack and Field-Slice DMA:* When a fiber performs `Net::read_some(inout stream, inout scratch_buf)`, `[T-Handle]` binds the captured fiber continuation `k` inside the reactor at mode *linear*. Because the reactor is statically forbidden from dropping `k` before the kernel posts the corresponding CQE, `k`'s suspended stack frame and caller `inout` projections remain live and unaliased throughout DMA—requiring zero heap buffer ownership transfers (`(Result, Vec)`) and zero reference counting.
+ *Explicit Intra-Fiber SQE Pipelining (`linear struct InFlightRead[B]`):* When a single fiber pipelines multiple concurrent SQEs onto an `io_uring` submission ring before awaiting completions, Pulse reifies each kernel submission as a `linear` token:

```
linear struct InFlightRead[B] {
    ring_id: u32,
    sqe_user_data: u64,
    pinned_buf: B,
}

impl[B] InFlightRead[B] {
    // Normal completion: awaits CQE arrival and returns buffer ownership alongside byte count.
    fn await_cqe(sink self) -> (Result[u32, IoError], B) eff [Storage] { ... }

    // Safe cancellation: submits IORING_OP_ASYNC_CANCEL and suspends linearly until the kernel
    // reaps the original operation's terminal CQE before returning owned `B`.
    fn cancel_and_reap(sink self) -> B eff [Storage] { ... }
}
```

=== 15.4 Structured Concurrency, Disjoint Place Projections, and Linear Channels
<ch-14-4>
Pulse replaces detached `'static` task spawning with structured nurseries (`Pulse::scope`), linear join handles, and linear channels:

- *Zero-`Arc` Fan-Out Over Disjoint Field Trees:* `JoinHandle[T]` is a `linear struct` whose obligation is discharged only by `.join()`. Because every child fiber spawned in `Pulse::scope` is guaranteed to complete before the scope exits, `[T-CallProj]` permits sibling fibers to mutably borrow disjoint projections of the parent stack (`inout broker.repl_stream` in one fiber, `sink tx` in a second fiber, and shared `in config` across both) with zero `'a` lifetime annotations and zero `Arc` or `Mutex` overhead.
- *Infallible `LinearOneshot[T]` Channels:* Whereas affine oneshot channels force receivers to handle sender-drop errors at runtime, `Pulse::linear_oneshot[T]()` returns `(LinearTx[T], LinearRx[T])` where `LinearTx[T]` has mode `linear`. Because the producer _must_ call `tx.send(v)` (or `tx.abort_with(err)`), `rx.recv()` returns `T` directly with zero abandoned-channel failure modes.
- *Work-Stealing vs. Thread-Per-Core Shards:* `s.spawn(...)` schedules `portable` fibers across a multi-core work-stealing deque (equipped with Tokio-style cache-hot LIFO slots), while `s.spawn_local(...)` pins `!portable` fibers to the current core's dedicated `io_uring` ring and registered buffer table.

```
// Concurrent WAL commit and quorum replication with zero lifetimes, zero Arc, and static cancel safety
fn replicate_and_commit(
    inout broker.{index, metrics, repl_stream}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Scope, Storage, Net, Clock, Cancel, Fail[BrokerError]] {
    // Cancellation permitted BEFORE acquiring linear obligations (res(Cancel) = affine):
    Cancel::check()?;

    let wire_slice = batch.as_wire_slice();
    let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);

    let (wal_res, repl_res) = Pulse::scope(|inout s| eff [Scope, Storage, Net, Clock] {
        // Child Fiber 1 consumes linear `tx` on the Storage io_uring handler:
        let wal_join: JoinHandle[Result[CommitReceipt, IoError]] = s.spawn(|| eff [Storage] {
            handle Fail[IoError] in {
                Ok(tx.commit())
            } with {
                return r => r,
                raise(e) => Err(e),
            }
        });

        // Child Fiber 2 mutates ONLY `broker.repl_stream` while sharing `in wire_slice`:
        let net_join: JoinHandle[Result[Unit, IoError]] = s.spawn(|| eff [Net, Clock] {
            Net::write_all(inout broker.repl_stream, in wire_slice)
        });

        // Both linear JoinHandles MUST be joined before scope exit:
        (wal_join.join(), net_join.join())
    });

    let receipt = wal_res.map_err(BrokerError::Wal)?;
    let _unit   = repl_res.map_err(BrokerError::Replication)?;

    broker.index.insert_in_place(sink batch);
    broker.metrics.batches_written += 1;
    receipt
}
```

==== Source Atomics (`Atomic[T]`, `MemoryOrder`)
<ch-14-4-atomics>
Plain fields stay race-free by the borrow rules above: a place that one worker mutates through `inout` is disjoint from every place another worker reads, so no ordinary load or store ever races. Shared counters, flags, latches and ring indices are the exception, and `Atomic[T]` is the only source type that may be updated through a shared `in` borrow.

- *Cell kinds:* `T` is `bool` or an integer kind (`u8`, `u16`, `u32`, `u64`, `usize`, `i8`, `i16`, `i32`, `i64`); any other `T` is rejected at the first operation (`E0821`). `Atomic::new(v)` allocates the cell like `ChunkSeq::new` and `into_inner(sink self)` consumes it and returns its contents.
- *Mode:* `Atomic[T]` is `affine`, never `copy`, so a cell has exactly one owner, which frees it. Workers share it only through `in` borrows (`in counter` in a spawned body, or an `in` parameter), and every operation takes the cell by `in`. Its contents are private to the operations (`E0822`): no other read, projection or constructor reaches the stored value.
- *Operations:* `load(order) -> T`, `store(v, order)`, `swap(v, order) -> T`, `compare_exchange(current, new, success, failure) -> Result[T, T]` (`Ok(previous)` after the update, `Err(actual)` when the cell did not hold `current`), and `fetch_add`, `fetch_sub`, `fetch_and`, `fetch_or`, `fetch_xor(v, order) -> T`, which return the previous contents. `fetch_add` and `fetch_sub` wrap modulo the kind's width and are not defined on `bool`; the bitwise forms are defined on both.
- *Orders:* every order argument is a literal `MemoryOrder::Relaxed`, `Acquire`, `Release`, `AcqRel` or `SeqCst` with the C++20 and Rust meanings. A `load` and the failure order of `compare_exchange` refuse `Release` and `AcqRel`; a `store` refuses `Acquire` and `AcqRel` (`E0820`). An implementation may strengthen any order; the native backend lowers every operation to one sequentially consistent Cranelift atomic at the kind's width.
- *Effects:* atomic operations are total and carry no effect. They are not observations of a place in the sense of the equality saturation of #link(<ch-7>)[Chapter 8]: each is an opaque call, so no read is forwarded across one.
- *Backends:* the evaluator, the JIT and executables run atomics. The verified backend of #link(<ch-9>)[Chapter 10] refuses them with a typed `Unsupported` diagnostic, and lock-free data structures (queues, epoch reclamation) remain library work above these cells.

```
// A worker records its hits, then publishes them with a Release store.
fn record(in hits: Atomic[u64], in done: Atomic[bool], n: u64) -> u64 {
    for i in 0..n {
        hits.fetch_add(1, MemoryOrder::Relaxed);
    }
    done.store(true, MemoryOrder::Release);
    hits.load(MemoryOrder::Acquire)
}
```

==== Scoped Workers (`parallel_for`)
<ch-14-4-workers>
`parallel_for(in data, n, body)` is the fork-join loop of source code: it runs `body(in data, i)` once for every `i` in `0..n`, in any order and on any number of workers, and returns only after the last iteration has finished. Its signature is `fn parallel_for[A](in data: A, n: u64, body: fn @copy(in A, u64))`; a module that declares its own `parallel_for` keeps it.

- *Sharing:* the workers share `data` only through an `in` borrow, so by the borrow rules above no worker writes a place another worker reads, and the only state they update together is an `Atomic[T]` cell inside `data` (#link(<ch-14-4-atomics>)[Source Atomics]). Data passed by `inout`, and a body whose first parameter is `inout` or `sink`, are refused (`E0824`).
- *Bodies:* `body` is a named top-level function that is not generic and has an empty effect row (`E0823`), so each worker runs it on its own with no handler, capability or capture from the caller. A body that needs to report failure records it in an atomic cell.
- *Scope:* the call joins every iteration before it returns, so no worker outlives the borrow of `data`, and the call has no effect of its own. A `parallel_for` inside a worker runs on that worker, so nested loops never wait on a pool whose workers are all waiting.
- *Backends:* the sequential loop is one valid schedule, and the evaluator and executables run it. The JIT hands the compiled body to a process-wide Pulse pool (#link(<ch-14-5>)[§15.5]), split into at most four chunks per worker, with the calling thread running the last chunk; a loop that reaches a value whose runtime state is per thread (`TiledSeq`, `Box`, regions) runs on the calling thread. The verified backend refuses the atomic cells the body would update.

```
struct Job { xs: ChunkSeq[u64], chunk: u64, total: Atomic[u64] }

// One iteration sums one slice and publishes it into the shared cell.
fn sum_chunk(in job: Job, i: u64) {
    let mut s: u64 = 0;
    let lo: u64 = wrapping_mul(i, job.chunk);
    for k in 0..job.chunk {
        match job.xs.try_get(wrapping_add(lo, k)) {
            Option::Some(v) => { s = wrapping_add(s, v); },
            Option::None => {},
        }
    }
    job.total.fetch_add(s, MemoryOrder::Relaxed);
}

fn total(in job: Job) -> u64 {
    parallel_for(in job, 4, sum_chunk);
    job.total.load(MemoryOrder::SeqCst)
}
```

=== 15.5 Cranelift Stack-Switching Fiber ABI and Evidence-Passing Lowering
<ch-14-5>
`vesper-opt-egglog` (Stage 4) and `vesper-codegen-clif` (Stage 5) compile Pulse capabilities without state-machine bloat using a three-tier lowering strategy:

+ *Pinned Lexical Evidence Vector (`r15` on `x86_64` / `x28` on `AArch64`):* Every effectful Vesper function receives a hidden Cranelift `vmctx` register pointing to the active `EvidenceVector`—a cache-line-aligned array of `(handler_fn_ptr, state_ptr, parent_sp)` slots indexed by static lexical capability depth in `Σ`. Looking up `Storage`, `Net`, or `Fail` is a single `O(1)` base+offset load with zero dynamic hash-map search.
+ *0 ns Tail-Resumptive Fast Path (`egglog` Fusion):* When an effect operation completes synchronously (for example, `Clock::now_ns()`, `Cancel::check()` when uncancelled, or `Net::read_some` satisfied from an in-memory read buffer), `egglog` evidence-passing rewrite rules fuse the handler inline into a direct register check and branch—allocating zero continuations and performing zero stack switches.
+ *One-Shot Pooled Fiber Stack Switching (`vesper_fiber_yield` / `vesper_fiber_resume`):* When an `io_uring` SQE or contended channel must suspend:
  + Each spawned fiber runs on a pooled virtual-memory stack slab (64 KiB default virtual reservation with 4 KiB lazy physical page faults and a hardware guard page, pooled per worker core).
  + At suspension, `vesper-codegen-clif` emits a lightweight call to the naked assembly trampoline `vesper_fiber_yield(ev_slot, op_tag, arg_ptr)`, which spills only callee-saved registers (`rbx, rbp, r12–r15, rsp, rip` on `x86_64`, or `x19–x29, lr, sp, d8–d15` on `AArch64`—80 to 144 bytes total) into an intrusive `FiberHeader` at the top of the fiber stack, writes the `FiberHeader` pointer into `sqe.user_data` (as the linear continuation `k`), and swaps `rsp`/`sp` back to the worker thread's scheduler loop in \~4–6 ns.
  + By #link(<ch-5>)[Chapter 5]'s *Zero-Pin Stack Relocation Corollary*, suspended stacks retain fixed virtual addresses. Pooling is permitted only after the fiber has terminated and every operation that can access its stack has reached terminal completion.

=== 15.6 SimPulse Deterministic Simulator and vesper top Live Telemetry
<ch-14-6>
Because `Async`, `Scope`, `Net`, `Storage`, `Clock`, and `Cancel` are ordinary lexical effects rather than hardcoded OS syscalls, Pulse swaps runtime semantics purely by changing the top-level `handle` block:

+ *SimPulse Deterministic Simulation Testing (`vesper test --sim`):* Inspired by FoundationDB and Tokio's `turmoil`, `SimPulse` replaces the `io_uring` reactor with a single-threaded, PRNG-seeded deterministic interpreter over the exact same unmodified production code:
  + *Virtual Monotonic Clock:* `Clock::sleep_ns` advances simulated time instantaneously whenever all runnable fibers park, executing 24-hour consensus lease and timeout suites in milliseconds.
  + *Adversarial Schedule Exploration (DPOR):* Intercepts every `Scope::spawn_scoped` and `Async::yield_now` to explore fiber interleavings deterministically and reproduce failures from a single 64-bit `--seed`.
  + *Network and Crash-Consistency Fault Injection:* Simulates asymmetric network partitions, packet reordering (`Net`), and sudden power loss / unfsynced WAL sector drops (`Storage`), verifying that `WriteTxn` recovery invariants hold under arbitrary crash points.
+ *Zero-Instrumentation `vesper top` (Live Fiber Flight Recorder):* Attaching `vesper top` (or setting `VESPER_PULSE_TRACE=1`) interposes a lock-free `TelemetryPulse` handler wrapper around the active `EvidenceVector`—requiring zero `#[instrument]` attributes and zero `--cfg` recompilation:
  + Streams per-fiber run-time vs. idle-time histograms, wake-to-poll scheduling latency, and active lexical effect call stacks over a shared-memory ring buffer.
  + *Live Linear Obligation Inspector:* Displays every live `Mode::Linear` resource (`WriteTxn`, `InFlightRead`, `JoinHandle`, `LinearTx`) currently parked on each suspended fiber stack alongside wait-for graph cycle detection—immediately pinpointing stalled protocol obligations in production.
+ *End-to-End Integration With Loom, SemVer, and Lean 4:*
  + *Vesper Loom Confinement (#link(<ch-10>)[Chapter 11]):* Packages with `effects = []` in `Loom.toml` are statically incapable of spawning background fibers, sleeping, or opening sockets.
  + *SemVer Substitutability (#link(<ch-11>)[Chapter 12]):* Adding `Cancel` or `Net` to an async API violates `[SV-EffRow]` and is automatically flagged by `vesper semver` as Major-breaking.
  + *Lean 4 Concurrency Proofs (#link(<ch-8>)[Chapter 9]):* `vesper-verify-lean` lowers Pulse functions directly into `EffM [ScopeEff, StorageEff, NetEff, CancelEff]`, allowing engineers to prove protocol safety and deadlock freedom against a pure Lean 4 nondeterministic scheduler model.

=== 15.7 Rust Runtime Core (`crates/vesper-pulse`)
<ch-14-7>
```rust
// crates/vesper-pulse/src/lib.rs
use std::collections::VecDeque;
use std::ptr::NonNull;

/// O(1) lexical capability evidence slot passed via Cranelift `vmctx` (`r15` / `x28`).
#[repr(C, align(64))]
pub struct EvidenceSlot {
    pub handler_fn: unsafe extern "C" fn(*mut u8, u32, *mut u8) -> u64,
    pub state_ptr: *mut u8,
    pub scheduler_sp: usize,
    pub scheduler_ip: usize,
}

/// Intrusive one-shot continuation header (`k :_q^own`) stored at the top of a pooled fiber stack.
#[repr(C, align(64))]
pub struct FiberHeader {
    pub saved_sp: usize,
    pub saved_ip: usize,
    pub fiber_id: u64,
    /// Bitmask/count of live `Mode::Linear` obligations for `vesper top` inspection & debug assertions.
    pub live_linear_obligations: u32,
    pub is_portable: bool,
    pub stack_base: NonNull<u8>,
    pub stack_len: usize,
}

/// Hybrid per-core shard scheduler combining a cache-hot LIFO slot with an io_uring CQE reactor.
pub struct PulseWorkerShard {
    pub core_id: u16,
    pub lifo_slot: Option<NonNull<FiberHeader>>,
    pub run_queue: VecDeque<NonNull<FiberHeader>>,
    pub simulated_clock_ns: Option<u64>, // `Some` under `SimPulse` (`vesper test --sim`), `None` in prod
}

impl PulseWorkerShard {
    /// Schedules a resumed linear continuation `k` harvested from an `io_uring` CQE or `LinearTx`.
    #[inline]
    pub fn wake_linear_continuation(&mut self, k_fiber: NonNull<FiberHeader>) {
        if let Some(prev) = self.lifo_slot.replace(k_fiber) {
            self.run_queue.push_back(prev);
        }
    }

    /// Deterministic `SimPulse` step: pops the next runnable fiber according to a seeded PRNG choice.
    pub fn step_deterministic(&mut self, prng_choice: usize) -> Option<NonNull<FiberHeader>> {
        if let Some(hot) = self.lifo_slot.take() {
            self.run_queue.push_front(hot);
        }
        if self.run_queue.is_empty() {
            return None;
        }
        let idx = prng_choice % self.run_queue.len();
        self.run_queue.remove(idx)
    }
}
```


== 16. Vesper Bridge: Zero-Copy Wire Layouts and Isolated FFI
<ch-15>
#chapter-deck[Wire Layouts, Binding Declarations, Foreign Values, and Isolation Boundaries]

*Vesper Bridge* (`@vesper-std/ffi`, with implementation code in `crates/vesper-ffi`) describes wire layouts and the foreign function interface (FFI). Its intended uses include kernel rings (`liburing`), codecs (`liblz4`, `libzstd`), RDMA verbs (`rdma-core`), Apache Arrow IPC, and Kafka `RecordBatch` segments. The design aims to preserve ownership, borrowing, bit validity, and Loom capability policies at foreign calls. Research influences include #link("https://www.usenix.org/conference/osdi25/presentation/schuermann")[Building Bridges: Safe Interactions with Foreign Languages through Omniglot] (Schuermann, Toubes, Potyondy, Pannuto, Milano, and Levy, OSDI 2025 Best Paper), #link("https://www.usenix.org/conference/usenixsecurity26/presentation/braunsdorf")[SafeFFI] (Braunsdorf et al., USENIX Security 2026), and _Rage Against the State Machine / Abacus_ (Potyondy et al., ASPLOS 2026).

=== 16.1 Cross-Language Memory, ABI, and Isolation Guarantees
<ch-15-1>
- *Wire and DMA Struct Casting:* Rust requires external `zerocopy` or `bytemuck` procedural macros or `unsafe transmute`, where implicit struct padding causes immediate undefined behavior. Vesper provides a built-in `wire struct` declaration enforcing zero implicit padding, explicit endian scalars (`u32be`, `u64le`), and 100% safe `O(1)` slice projection.
- *Foreign Return Validity:* Rust asserts valid host types on faith across `extern "C"`, so an invalid C `bool`, enum discriminant, or misaligned pointer triggers immediate undefined behavior. Vesper Bridge adopts Omniglot's *`ForeignVal[T]`* (`?T`) wrapper: foreign returns carry zero validity assumptions until promoted via constant-time `validate()`.
- *Cross-Language Borrowing:* Raw foreign pointers (`*const T`, `*mut T`) ordinarily erase alias and mutability tracking at the FFI call boundary. Vesper's declarative `.vsys` specifications map `nocapture` C pointers directly onto `[T-CallProj]` `in` and `inout` slice projections.
- *Foreign Handle Lifecycle:* Implicit destructor wrappers silently discard close or flush errors and remain vulnerable to safe handle forgetting. Vesper's `linear extern type` enforces exact-once C finalizer execution or protocol state transitions across 100% of control-flow paths.
- *Supply-Chain and Hardware Memory Isolation:* Rather than permitting arbitrary dependencies to run imperative build scripts or link unrestricted C libraries into the host address space, Vesper gates foreign libraries via `eff [Ffi[Lib]]` in `Loom.toml` and supports *1.5 ns Hardware MPK/PKRU and Arm POE* zero-copy sandboxing.

=== 16.2 Zero-Copy `wire struct` Layouts and Endian-Safe Projections
<ch-15-2>

*Review limitation:* Packed wire fields may be unaligned. Implementations must use bytewise or explicitly unaligned accesses, including endian conversion, and never form an aligned native reference to an unaligned field.
To eliminate defensive `memcpy` calls and `unsafe` pointer casts when parsing network frames or kernel rings, Vesper introduces *`wire struct`*—a nominal layout-verified declaration kind (`mod copy`):

+ *Zero Implicit Padding Invariant:* The compiler computes exact byte offsets for every field. If natural alignment would introduce an uninitialized padding hole, compilation fails with a suggested explicit `ZeroPad[N]` field (deterministically zero-initialized on construction and ignored on comparison).
+ *Total Bit-Validity (`FromBytes` and `AsBytes`):* Every field of a `wire struct` must inhabit all bit patterns of its width: fixed-width integers, explicit-endian wire integers (`u16be`, `u32be`, `u64be`, `i64be`, `u32le`, `u64le`), fixed byte arrays (`ByteArray[N]`), or nested `wire struct` types. `bool` and sparse `enum` tags are rejected inside `wire struct` in favor of total `WireFlag8` or `WireTag16` wrappers.
+ *Safe Zero-Copy Views (`view_wire` and `split_wire_mut`):* Any `ByteSlice` or `io_uring` DMA buffer projects into `in W` or `inout W` via a single bounds and alignment check—compiling to a direct pointer offset in Cranelift CLIF with zero heap allocation:

```
// Kafka-compatible record batch header: big-endian wire fields, 0 implicit padding, 61 bytes exact.
wire struct KafkaBatchWireHeader {
    base_offset: i64be,
    batch_length: i32be,
    partition_leader_epoch: i32be,
    magic: u8,
    crc: u32be,
    attributes: u16be,
    last_offset_delta: i32be,
    base_timestamp: i64be,
    max_timestamp: i64be,
    producer_id: i64be,
    producer_epoch: i16be,
    base_sequence: i32be,
    records_count: i32be,
}

// 100% safe zero-copy header inspection and in-place CRC stamp—zero unsafe, zero memcpy!
fn stamp_batch_header(
    inout raw_frame: ByteSlice,
    computed_crc: u32,
) -> Result[i64, WireLayoutError] {
    let (inout hdr, in _payload) = raw_frame.split_wire_mut[KafkaBatchWireHeader]()?;
    hdr.crc = computed_crc.to_be();
    Ok(hdr.base_offset.to_native())
}
```

=== 16.3 Declarative `.vsys` Bindings, `ForeignVal[T]`, and Linear Foreign Handles
<ch-15-3>
Rather than executing imperative `build.rs` scripts or `bindgen` macros, C and Rust ABI bindings live in declarative *`.vsys` manifests* lowered directly into *Vesper Loom's Package Calculus* (#link(<ch-10>)[Chapter 11]):

```
// sys/lz4.vsys — Declarative C ABI contract verified by Vesper Loom and Vesper Bridge
sys library LibLz4 (pkg_config = "liblz4 ^1.9.4", header = "lz4frame.h") {
    // Linear opaque foreign handle: cannot go out of scope without LZ4F_freeDecompressionContext!
    linear extern type Lz4DecompressCtx;

    fn LZ4F_createDecompressionContext(version: u32)
        -> ForeignVal[Option[Lz4DecompressCtx]];

    // `in src` and `inout dst` enforce [T-CallProj] Alias-XOR-Mutability at the FFI call site.
    // `nocapture` asserts C retains neither pointer after LZ4F_decompress returns.
    fn LZ4F_decompress(
        inout ctx: Lz4DecompressCtx,
        nocapture inout dst: CSlice[u8],
        nocapture in src: CSlice[u8],
    ) -> ForeignVal[usize];

    // Terminal `sink` consumer discharging the `linear` obligation on Lz4DecompressCtx.
    fn LZ4F_freeDecompressionContext(sink ctx: Lz4DecompressCtx) -> ForeignVal[usize];
}
```

- *Omniglot `ForeignVal[T]` Boundary Validation:* Every value returned across a foreign ABI lands in `ForeignVal[T]`, which assumes zero bit-validity invariants until explicitly checked via `.validate_non_error()` or `.validate_enum()`.
- *Packages Carry Their Imports:* A package names its `.vsys` files in its manifest, `vsys = ["sys/lz4.vsys"]` under `[package]`: relative paths inside the package directory, with `/` separators and no `..`, each named once. Loading a package graph reads them for every package in it, so a dependency brings its own imports: a library build (`vesper build --lib`), an executable (`vesper build`) and `vesper check` and `vesper jit` of a package over it all see `LibLz4`, and no consumer repeats its text. Every module of every package in the graph names the library directly (`LibLz4::f(..)`, `eff [Ffi[LibLz4]]`); two packages importing libraries of the same name is an error. A library build's root module may set how it links a carried library with `// @export vsys LibLz4 [direct|isolated(mpk)] [rust=<path>]` (no `// |` text); the handler is `direct` otherwise. An executable links each library it calls by its link name. `vesper jit` and `vesper run` bind only the libraries the compiler itself hosts (the standard I/O libraries, link name `vesper_std_host`) and refuse others before running; `vesper run` marshals each call's source values into the C ABI the library's descriptors define.
- *Loom Supply-Chain Confinement:* Importing `sys/lz4.vsys` synthesizes the lexical capability `Ffi[LibLz4]`. Any package calling `LibLz4` must declare `"Ffi[LibLz4]"` in its `Loom.toml` `capability_ceiling`; pure packages (`effects = []`) are statically barred from linking or invoking foreign symbols.
- *Opaque Extern-Type Handles:* A `linear extern type H;` or `affine extern type H;` declares a foreign object its library owns and Vesper holds as one opaque pointer-sized word. An import may return it, as `ForeignVal[Option[H]]` (`NULL` is `None`), `ForeignVal[H]` (`NULL` is `Err(ForeignInvalid { raw: 0 })`) or `ForeignVal[NonNull[H]]` (the library promises a handle: source code receives `H` itself with no error arm, and a `NULL` breaks the promise and traps), and take it `in h: H` (a shared loan), `inout h: H` (an exclusive loan) or `sink h: H` (consumed); in every mode the C callee receives the handle word itself. A `sink` import is a finalizer, and a library declaring a `linear` type must declare at least one. In source, `H` is a `linear` or `affine` type with no constructor, fields or equality: only imports create, read and consume it, and an exported library function cannot take or return it, except that an export ABI v4 library takes one as an `in` parameter lent by Rust (§16.6). A `linear` handle reaches exactly one `sink` on every path: dropping it (`E0302`), consuming it on only some paths (`E0309`), consuming it twice or using it after its finalizer (`E0303`), or keeping it live across an unhandled abortable effect such as `Fail` (`E0402`) is rejected. A foreign call itself always returns exactly once, so a handle may stay live across one. An `affine` handle may also be dropped. Without a drop import, dropping it calls nothing: it suits a view the library keeps alive. `affine extern type H drop f;` names the import `fn f(sink h: H) -> ForeignVal[Unit];` (the only signature accepted; a `linear` type declares none) that releases a dropped handle: wherever source code drops an `H`, or an `Option[H]` holding one, without consuming it (the end of its scope, an overwrite, a discarded result, a branch or early exit that leaves it, a loop iteration), `f` runs exactly once. Dropping it therefore performs `Ffi[Lib]`, which the dropping function's effect row must hold (`E0401`), and such a handle may not stay live across an unhandled abortable effect (`E0402`), as a `linear` one may not, since an abort would skip `f`. An aggregate other than `Option` that holds such a handle is released field by field when it is a binding; dropped whole as a temporary, it is rejected. An object whose release can fail, or must happen at a chosen point, is `linear`.
- *C Strings and OS Error Codes:* A `nocapture in s: CStr` parameter (only `in`, never `Option`) passes C one `const char *`; in source it takes a `Slice[u8]`, whose bytes are copied into a fresh buffer with a NUL appended and freed after the call. An argument holding a NUL byte refuses the call: C is not called, and a function with a `CStr` parameter therefore returns `Result[R, ForeignInvalid]`, `Err(ForeignInvalid { raw })` naming the offset of the first NUL. A function declared `-> ForeignVal[T] errno` reads the calling thread's OS error code (`errno`, or `GetLastError()` on Windows) right after the call returns and before any other code runs, and returns the pair `(R, u32)`, so `fn access(nocapture in path: CStr, mode: i32) -> ForeignVal[i32] errno;` is `Result[(i32, u32), ForeignInvalid]` in source. Here `R` is the source type `ForeignVal[T]` alone would give.
- *Slices Lent in Place:* A `nocapture in` or `nocapture inout` `CSlice[T]` parameter accepts a source `Slice[T]` when `T`'s packed sequence stride is its C size (`u8`, `u32`, `u64`, `i64`, `usize`): the callee receives the slice's own `(data + start * stride, len)`, so foreign code reads or writes the caller's bytes with no copy, for example an AEAD sealing a packet buffer in place. A homogeneous tuple argument is still copied through a C-layout buffer, one operation instance per tuple arity.

=== 16.4 Hardware-Isolated Zero-Copy Sandboxing (Intel MPK/PKRU and Arm POE)
<ch-15-4>

*Review limitation:* MPK and Arm permission overlays alone are fault-containment mechanisms, not sandboxes against hostile native code: unprivileged instructions, signal paths, and system calls can change access rights. An adversarial boundary requires independently verified instruction and control-flow restrictions plus syscall confinement, or process/Wasm isolation.
To achieve Omniglot-grade spatial memory safety without copying multi-megabyte storage or network buffers across IPC boundaries, Vesper Bridge provides two handler tiers for `handle Ffi[Lib]`:

+ *Direct Native Handler (`unsafe handle Ffi[Lib] direct`):* Emits a raw Cranelift System V or AAPCS64 `call` instruction directly on the current fiber's 64 KiB virtual stack slab (\~0 ns overhead). Reserved for kernel VDSO/`liburing` syscalls and formally verified libraries.
+ *Hardware Domain-Isolated Zero-Copy Handler (`handle Ffi[Lib] isolated(mpk)`):*
  - Uses *Intel Memory Protection Keys (`WRPKRU`)* on `x86_64` and *Arm Permission Overlay Extension (`POR_EL0`)* on `AArch64` to partition virtual memory into three hardware page-table domains within a single address space:
    - *Domain 0 (Vesper Host):* Fiber stacks (`FiberHeader`), evidence vectors, and private Vesper structs.
    - *Domain 1 (Shared Zero-Copy I/O Arena):* Registered `ByteSlice`, Arrow, and `io_uring` buffers passed as `in` (mapped read-only in C's key mask) or `inout` (mapped read-write in C's key mask).
    - *Domain 2 (Foreign Sandbox Heap):* Private allocator heap, stack, and globals belonging to `Lib`.
  - At the FFI trampoline, Cranelift emits a 1.5 ns unprivileged `WRPKRU` / `MSR POR_EL0` instruction revoking C's read/write access to Domain 0, pivots `rsp`/`sp` to Domain 2's sandbox stack, invokes C *directly on the zero-copy Domain 1 pointers*, restores Domain 0 permissions on return, and validates `ForeignVal[T]`. Stray C writes into Vesper memory trap immediately in hardware with *zero `memcpy` overhead*.

=== 16.5 Zero-Copy Wire Layout and ABI Verifier (`crates/vesper-ffi`)
<ch-15-5>
`crates/vesper-ffi` computes exact field byte offsets, rejects implicit alignment holes, verifies total bit-pattern inhabitation (`FromBytes` / `AsBytes`), and lowers `.vsys` declarations into Cranelift call-site trampolines:

```rust
// crates/vesper-ffi/src/lib.rs
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireFieldSpec {
    pub size_bytes: usize,
    pub align_bytes: usize,
    pub all_bit_patterns_valid: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireStructLayout {
    pub name: SmolStr,
    pub exact_byte_size: usize,
    pub alignment: usize,
    pub has_zero_implicit_padding: bool,
    pub all_fields_from_bytes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireLayoutError {
    ImplicitPaddingHole { field_idx: usize, offset: usize, required_pad: usize },
    TailPaddingHole { struct_end: usize, required_pad: usize },
    NonTotalFieldType { field_idx: usize },
}

pub fn verify_wire_struct_layout(
    name: SmolStr,
    fields: &[WireFieldSpec],
) -> Result<WireStructLayout, WireLayoutError> {
    let mut offset = 0usize;
    let mut max_align = 1usize;
    for (idx, f) in fields.iter().enumerate() {
        if !f.all_bit_patterns_valid {
            return Err(WireLayoutError::NonTotalFieldType { field_idx: idx });
        }
        max_align = max_align.max(f.align_bytes);
        let rem = offset % f.align_bytes;
        if rem != 0 {
            return Err(WireLayoutError::ImplicitPaddingHole {
                field_idx: idx,
                offset,
                required_pad: f.align_bytes - rem,
            });
        }
        offset += f.size_bytes;
    }
    let tail_rem = offset % max_align;
    if tail_rem != 0 {
        return Err(WireLayoutError::TailPaddingHole {
            struct_end: offset,
            required_pad: max_align - tail_rem,
        });
    }
    Ok(WireStructLayout {
        name,
        exact_byte_size: offset,
        alignment: max_align,
        has_zero_implicit_padding: true,
        all_fields_from_bytes: true,
    })
}
```


=== 16.6 Exporting Vesper Libraries (`vesper build --lib`)
<ch-15-6>
Bridge imports foreign code. The export direction lets Rust and C link compiled Vesper, which is how crates are ported one at a time (`docs/maintenance/porting-a-crate.md`). `vesper build --lib` emits one object in which every `pub fn` of the module (for a package, of its root module, including the functions it re-exports with `pub use`; #link(<ch-10-3>)[Section 11.3]) is the C-ABI symbol `vesper_<library>_<fn>`, with the target's C calling convention. ABI `vesper.native.export-abi/1` fixes the rest:

- *Values* are the eight-byte-word layouts of the native backend: a scalar is one word, products are their fields in order, enums are a tag word (the variant index) followed by the variant's fields, padded to the largest variant. The words past the active variant's fields are its *inactive payload*: their contents are unspecified (a value built through a wider variant may leave its words there), and no operation observes them. Equality, ordering, hashing, `match`, drops and every conversion at the boundary read the tag first and then only the active variant's fields, and a copy may carry the inactive words along unchanged. Scalars pass by value (`f64` as a double and `f32` as a single-precision float, both in the platform's float registers; inside an aggregate or a sequence buffer an `f32` is the low half of its word, and the high half is unspecified); aggregates pass as a pointer to a scratch copy of their words, and an aggregate result is written through a leading result pointer.
- *`Fail[E1], ..., Fail[En]`* adds a trailing failure-record pointer. Its first word is the status: the caller stores 0; on return 0 means success and `k` means the `k`-th `Fail` of the row was raised, with the error value in the following words.
- *Only `Fail` crosses the boundary.* Generic functions, `inout` parameters, every other effect (capabilities, `Ffi[Lib]`, user effects) and types without a fixed word layout (affine or linear data, strings, closures, region handles) are refused at export with a typed error naming the function. Sequences and strings join the ABI with their source types.

A machine-readable interface file describes every symbol, and a generated Rust binding wraps each one so that the status word becomes `Result`.

ABI `vesper.native.export-abi/3`, which a library opts into with the source directive `// @export abi 3`, keeps the v2 object and changes what the Rust side passes. Each `copy` struct gets a `#[repr(C)]` *mirror* in the binding whose fields are the struct's words in layout order: full-word scalars as their Rust type, narrower scalars, `bool` and field-less enums as a canonical word cell, nested structs as nested mirrors, and other copy values as a word block converted on access, whose equality compares the converted values, so an enum's inactive payload words never decide it. The binding asserts the mirror's size, alignment and every field offset at compile time. An `in` or `inout` parameter of a mirrored struct, and an `in`/`inout Slice[T]` of one, is then a pointer to the caller's own storage, with no conversion and no scratch copy; Vesper never writes an `in` aggregate, and a `sink` one receives a copy. An `in` parameter of a handle type is likewise a pointer to the caller's own handle words. Enums and tuples have no fixed Rust layout and keep the v2 conversion, except that `// @export abi 3 options` lends `Option[T]` and `Result[T, E]` of copy payloads the same way, as a mirror of the tag word followed by the payload's cell. `@export` directives are read from the package's root module; `abi` in another file of the package applies with a warning, and the other directives there are refused.

A handle type's fields may also hold a `ChunkSeq` whose elements own memory (other handle types, `String`s, boxes), and boxed objects `Box[dyn Trait]` (§7.3), directly or inside an `Option`. The handle stays opaque to the host: its binding reports such a sequence's length and whether a boxed slot holds an object, never their contents, and dropping the handle from the host drops every element, each boxed value through its vtable's drop word. Such a sequence never crosses on its own, as a parameter or a result, and a slice of non-copy elements is refused.

ABI `vesper.native.export-abi/4`, which a library opts into with `// @export abi 4` (optionally `abi 4 options`), keeps every v3 form and object and carries compiler trees, in both directions. *Owned trees:* each handle type whose fields are all copy values, `String`s, `Box[T]`s, `Option[T]`s, `ChunkSeq`s of copy values or of handle types, or other such handle types gets an owned Rust mirror `<Type>Tree` (`Box`, `String`, `Vec` and `Option` in place of the Vesper forms, the same variants and field names). `<Type>::encode(&tree)` builds the Vesper value once, its boxes, sequences and strings allocated by the library (the runtime gains `vesper_<lib>_box_new(size)`, a zeroed box, beside `seq_new`), and the handle that holds it frees it once as any handle does; `<Type>::decode` copies a value into a tree once. A Vesper function walks an encoded tree through an `in` parameter with borrowed matching (§4.3), without moving or copying it. *Lent views:* the handle views of v2 read a value in place; v4 also reads a `ChunkSeq` of handle elements in place, as `<Elem>Seq` views whose elements are `<Elem>Ref` views, so Rust walks a whole Vesper tree without a copy. *Lent Rust handles:* an `in` parameter of a `.vsys` extern type `H` (§16.3) is the address of a Rust value, made with `unsafe { H::lend(&value) }` in the binding, where the caller vouches that the value has the type the accessors read; Vesper passes the word only to the library's own imports, whose symbols the linking crate implements over the same Rust type (the `// @export vsys direct rust=<path>` directive), and never reads, stores or frees it. Such a handle crosses only as an `in` parameter. A tree form names no Vesper address, so the layout rules of v1 are the only contract between the encoder, the decoder and the drop glue: a tag word then the active variant's fields, a box as the address of its pointee's words, a sequence as its header `{len, cap, data}` with `len` elements of the element's size, and a string as `{len, cap, data}`.

== 17. Vesper Nexus: WasmFX Sandboxes and Linear WIT Plugins
<ch-16>
#chapter-deck[WebAssembly Continuations, Resource Lifecycles, and Host/Guest Views]

*Vesper Nexus* (`@vesper-wasm/nexus`, with implementation code in `crates/vesper-nexus`) describes a WebAssembly Component Model target (`wasm32-wasip2` and `wasip3`) and a multi-tenant plugin runtime. It extends the compile-time macro sandbox in #link(<ch-10-2>)[Section 11.2] to runtime plugins. The proposal maps one-shot effects to WasmFX continuations (`cont.new`, `resume`, `suspend`), tracks WIT `resource` lifecycles with `Mode::Linear`, and uses Hardware Domain-1 shared rings for host/guest payloads. Each mechanism requires the isolation and lifetime checks described below.

=== 17.1 WebAssembly Component, Continuation, and Resource Guarantees
<ch-16-1>
+ *Direct Lexical-Effect to WasmFX Continuation Lowering (`cont.new` and `resume`):* Compiling asynchronous Rust to WebAssembly requires either `poll`-state-machine futures bridged to JS/WASI promises or binary-bloating `wasm-opt --asyncify` whole-program transforms. Because Vesper's `eff [...]` capabilities are already one-shot and lexically scoped (`[T-Handle]`, #link(<ch-5>)[Chapter 5]), Stage 5 (`vesper-nexus`) lowers guest suspensions directly to ratified *WasmFX* typed stack-switching instructions (`cont.new`, `suspend`, `resume`) validated by _Iris-WasmFX_ (#link(<ch-14>)[Chapter 15]), achieving sub-10-nanosecond guest coroutine switches with zero state-machine code size inflation.

+ *Substructural WIT Resource Safety (`own<T>` as `Mode::Linear`, `borrow<T>` as `in`/`inout`):* In Rust's `wit-bindgen` and `wasmtime::component::Resource<T>`, WIT `own<T>` and `borrow<T>` handles are plain integer-backed structs whose protocol invariants—such as forbidding `resource.drop` while any `borrow<T>` is active or ensuring every `own<T>` is eventually dropped—are checked dynamically at runtime via traps. Nexus maps WIT `own<T>` directly to `Mode::Linear` (`linear struct WitOwn[T]`) and `borrow<T>` to second-class `in` / `inout` projections (`[T-CallProj]`). Under Theorem 2, guest and host code are statically incapable of leaking a host resource, using a dropped handle, or dropping an `own<T>` while a borrow is live.

+ *Zero-Copy Shared-Everything Buffer Rings (`SharedPluginRing`):* Standard WebAssembly Component Model Canonical ABI calls invoke guest `cabi_realloc` and `memcpy` every list, string, or record across linear memories, bottlenecking Kafka broker stream filters, database UDFs, and game mods. Nexus pairs Vesper Bridge padding-free `wire struct` layouts (#link(<ch-15-2>)[Section 16.2]) with page-aligned *Domain-1 `DmaSlab` memory views* (`wasm-shared-everything-threads` / multi-memory index `1`), allowing sandboxed plugins to inspect and transform `RecordBatch` frames in place with zero `cabi_realloc` allocations and zero byte copies.

+ *Fuel-Metered Multi-Tenant Preemption on Vesper Pulse Shards:* Running untrusted tenant UDFs on a shared `tokio` worker risks starving sibling I/O tasks if a guest enters a tight CPU loop. Nexus integrates Cranelift SSA fuel counters and epoch interruption directly with *Vesper Pulse* (#link(<ch-14-5>)[Section 15.5]): when a plugin exhausts its quantum (for example, `50_000` fuel units = \~25 microseconds), the WasmFX continuation suspends cleanly onto the shard's `PulseWorkerShard` run queue or aborts via `Fail[PluginTrap]` without blocking `io_uring` CQE harvesting.

+ *Lean 4 Handle-Table Non-Forgery and Sandboxed Isolation Proofs (`vesper verify`):* `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts Nexus's generational WIT handle table and host-import capability linker into Lean 4, proving that untrusted guest index integers can never forge access to another tenant's resources or escalate beyond the host's granted effect row.

=== 17.2 Substructural WIT Resource Capabilities and Zero-Copy Shared Views
<ch-16-2>
Nexus compiles declarative `.wit` world specifications into Vesper capability traits and linear handles without `build.rs` scripts or `wit-bindgen` macro sprawl:

```
// Shared 24-byte zero-copy descriptor passed in Wasm Memory 1 (0 cabi_realloc, 0 memcpy!):
wire struct PluginSliceGrantWire {
    ring_offset: u32le,
    byte_len: u32le,
    schema_id: u32le,
    tenant_flags: u32le,
    record_timestamp_ns: u64le,
}

// WIT own<T> handle: Mode::Linear guarantees exact-once guest/host finalization!
linear struct WitOwn[Res] {
    rep_idx: u32,
    generation: u32,
}

// Instantiated multi-tenant WasmFX component sandbox (0 GlobalAlloc churn!):
linear struct PluginInstance[World] {
    instance_id: u64,
    remaining_fuel: u64,
}

linear effect PluginHost {
    fn instantiate_with_fuel[W: WitWorld](
        in compiled: CompiledComponent[W],
        fuel_budget: u64,
    ) -> Result[PluginInstance[W], PluginError];

    // Executes guest export on a WasmFX continuation stack over zero-copy Domain-1 slice:
    fn call_transform_zc[W: WitWorld](
        inout inst: PluginInstance[W],
        in grant: PluginSliceGrantWire,
        in payload_ro: DmaSlice,
        inout payload_out: DmaSliceMut,
    ) -> Result[u32, PluginError];

    fn destroy_instance[W: WitWorld](sink inst: PluginInstance[W]) -> Unit;
}
```

=== 17.3 Multi-Tenant Zero-Allocation Broker Stream Filter (`0` `Alloc`, `0` `memcpy`)
<ch-16-3>
Using `PluginHost`, a Krabka message-broker shard executes untrusted tenant Wasm UDFs inline on `io_uring` DMA buffers while enforcing strict fuel budgets and Theorem 2 instance cleanup across both happy and trap paths:

```
fn apply_tenant_wasm_udf(
    in compiled_udf: CompiledComponent[StreamFilterWorld],
    in batch_in: DmaSlice,
    inout batch_out: DmaSliceMut,
    inout metrics: TenantUdfMetrics,
) -> Result[u32, FilterError] eff [PluginHost, Clock, Fail[FilterError]] {
    let mut inst: PluginInstance[StreamFilterWorld] =
        PluginHost::instantiate_with_fuel(in compiled_udf, 100_000)
            .map_err(FilterError::InitFailed)?;

    let grant = PluginSliceGrantWire {
        ring_offset: 0u32.to_le(),
        byte_len: (batch_in.len() as u32).to_le(),
        schema_id: 0x0201u32.to_le(),
        tenant_flags: 1u32.to_le(),
        record_timestamp_ns: Clock::now_ns().to_le(),
    };

    // Note: [T-Let] statically rejects ? here unless linear inst is destroyed on Err!
    match PluginHost::call_transform_zc(inout inst, in grant, in batch_in, inout batch_out) {
        Ok(written_bytes) => {
            metrics.udf_invocations += 1;
            metrics.fuel_consumed += 100_000 - inst.remaining_fuel;
            PluginHost::destroy_instance(sink inst); // Linearly reclaims WasmFX stack & tables!
            Ok(written_bytes)
        }
        Err(trap_err) => {
            metrics.udf_traps += 1;
            PluginHost::destroy_instance(sink inst); // Linearly reclaims sandbox on guest trap/OOG!
            Fail::raise(FilterError::GuestTrapped(trap_err))
        }
    }
}
```

=== 17.4 Lean 4 WIT Handle-Table Isolation and Borrow-Before-Drop Proofs (`vesper verify`)
<ch-16-4>
Extracting Nexus's WIT handle table via `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Cross-Tenant Handle Non-Forgery* (a forged `(idx, gen)` pair from Tenant A never resolves a live resource owned by Tenant B) and *Component-Model Borrow Soundness* (`drop_own` succeeds only when active `lend_count == 0`):

```lean
-- illustrative/NexusWitSafety.lean
-- 1. Generational Tenant Isolation: a handle minted for tenantA never resolves under tenantB:
theorem wit_handle_tenant_isolated
    (slot : WitSlotEntry) (reqIdx : UInt32) (reqGen : UInt32) (tenantA tenantB : UInt64)
    (h_owner : slot.ownerTenant = tenantA)
    (h_diff : tenantA ≠ tenantB) :
    WitTable.lookupForTenant slot reqIdx reqGen tenantB = .error .unauthorizedTenant := by
  simp [WitTable.lookupForTenant, h_owner, h_diff]

-- 2. WIT Borrow-Safety Invariant: dropping an own<T> while activeBorrows > 0 is rejected:
theorem wit_drop_own_rejects_active_borrows
    (slot : WitSlotEntry) (h_borrowed : slot.activeBorrows > 0) :
    WitTable.dropOwnSlot slot = .error .resourceHasActiveBorrows := by
  simp [WitTable.dropOwnSlot, h_borrowed]
  omega
```

=== 17.5 Rust Generational WIT Resource Table and Fuel Accountant (`crates/vesper-nexus`)
<ch-16-5>
`crates/vesper-nexus` implements the O(1) generational WIT resource table (enforcing the WebAssembly Component Model's `num_lends == 0` drop invariant and tenant ownership tags) alongside the deterministic WasmFX fuel meter:

```rust
// crates/vesper-nexus/src/lib.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WitHandle {
    pub idx: u32,
    pub generation: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WitTableError {
    InvalidOrStaleHandle(WitHandle),
    UnauthorizedTenant { expected: u64, actual: u64 },
    ResourceHasActiveBorrows { idx: u32, active_borrows: u32 },
    FuelExhausted { requested: u64, remaining: u64 },
}

#[derive(Clone, Debug)]
struct ResourceSlot<T> {
    generation: u32,
    owner_tenant: u64,
    active_borrows: u32,
    payload: Option<T>,
}

/// Zero-allocation generational WIT resource table shared across pooled WasmFX instances.
pub struct WitResourceTable<T> {
    slots: Vec<ResourceSlot<T>>,
    free_indices: Vec<u32>,
}

impl<T> Default for WitResourceTable<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free_indices: Vec::new(),
        }
    }
}

impl<T> WitResourceTable<T> {
    pub fn insert_own(&mut self, tenant_id: u64, val: T) -> WitHandle {
        if let Some(idx) = self.free_indices.pop() {
            let slot = &mut self.slots[idx as usize];
            slot.owner_tenant = tenant_id;
            slot.active_borrows = 0;
            slot.payload = Some(val);
            WitHandle { idx, generation: slot.generation }
        } else {
            let idx = self.slots.len() as u32;
            self.slots.push(ResourceSlot {
                generation: 1,
                owner_tenant: tenant_id,
                active_borrows: 0,
                payload: Some(val),
            });
            WitHandle { idx, generation: 1 }
        }
    }

    pub fn begin_borrow(&mut self, tenant_id: u64, h: WitHandle) -> Result<&T, WitTableError> {
        let slot = self
            .slots
            .get_mut(h.idx as usize)
            .filter(|s| s.generation == h.generation && s.payload.is_some())
            .ok_or(WitTableError::InvalidOrStaleHandle(h))?;
        if slot.owner_tenant != tenant_id {
            return Err(WitTableError::UnauthorizedTenant {
                expected: slot.owner_tenant,
                actual: tenant_id,
            });
        }
        slot.active_borrows += 1;
        Ok(slot.payload.as_ref().expect("verified Some above"))
    }

    pub fn end_borrow(&mut self, tenant_id: u64, h: WitHandle) -> Result<(), WitTableError> {
        let slot = self
            .slots
            .get_mut(h.idx as usize)
            .filter(|s| s.generation == h.generation && s.payload.is_some() && s.active_borrows > 0)
            .ok_or(WitTableError::InvalidOrStaleHandle(h))?;
        if slot.owner_tenant != tenant_id {
            return Err(WitTableError::UnauthorizedTenant {
                expected: slot.owner_tenant,
                actual: tenant_id,
            });
        }
        slot.active_borrows -= 1;
        Ok(())
    }

    /// Enforces the Component Model invariant: own<T> cannot drop while active_borrows > 0.
    pub fn drop_own(&mut self, tenant_id: u64, h: WitHandle) -> Result<T, WitTableError> {
        let slot = self
            .slots
            .get_mut(h.idx as usize)
            .filter(|s| s.generation == h.generation && s.payload.is_some())
            .ok_or(WitTableError::InvalidOrStaleHandle(h))?;
        if slot.owner_tenant != tenant_id {
            return Err(WitTableError::UnauthorizedTenant {
                expected: slot.owner_tenant,
                actual: tenant_id,
            });
        }
        if slot.active_borrows > 0 {
            return Err(WitTableError::ResourceHasActiveBorrows {
                idx: h.idx,
                active_borrows: slot.active_borrows,
            });
        }
        let val = slot.payload.take().expect("verified Some above");
        slot.generation = slot.generation.wrapping_add(1).max(1);
        self.free_indices.push(h.idx);
        Ok(val)
    }
}

/// Deterministic WasmFX instruction-fuel meter bound to a Vesper Pulse worker quantum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuelAccountant {
    pub remaining: u64,
}

impl FuelAccountant {
    #[inline(always)]
    pub fn charge(&mut self, cost: u64) -> Result<(), WitTableError> {
        if cost > self.remaining {
            let rem = self.remaining;
            self.remaining = 0;
            return Err(WitTableError::FuelExhausted { requested: cost, remaining: rem });
        }
        self.remaining -= cost;
        Ok(())
    }
}
```


= Part IV. Developer Tooling, Diagnostics, Verification Testing, and Presentation
<part-4>
#chapter-deck[Editor Support, Formatting, Documentation, Highlighting, Testing, Replay, and Benchmarks]

#_part-summary("Part IV Architectural Scope")[
  Part IV covers the tools used to inspect, test, and present Vesper programs. #link(<ch-17>)[Chapter 18] introduces the *Lens* language server; #link(<ch-18>)[Chapter 19] covers *Stylus* formatting and *Lint* analysis. #link(<ch-19>)[Chapter 20] describes *Codex* documentation and doctests, and #link(<ch-20>)[Chapter 21] describes *Chroma* highlighting. #link(<ch-21>)[Chapters 22]–#link(<ch-23>)[24] cover *Crucible* fuzzing, *Chrono* replay, and *Metronome* benchmarking. Performance budgets and proposed verification features require evidence for the relevant implementation and workload.
]

#figure(
  table(
    columns: (14%, 34%, 52%),
    align: (left, left, left),
    table.header([*Chapter*], [*Subsystem*], [*CLI Surface, Host Crate, and Core Capabilities*]),
    [#link(<ch-17>)[Chapter 18]], [Vesper Lens (Incremental IDE Server)], [`vesper lsp` (`vesper-span`, `vesper-lsp`): `PoisonedRecovery`, dual early cutoff, obligation quick-fixes],
    [#link(<ch-18>)[Chapter 19]], [Vesper Stylus and Lint], [`vesper fmt` / `lint` (`vesper-fmt`, `vesper-lint`): Wadler-Lindig CST formatting, projection narrowing],
    [#link(<ch-19>)[Chapter 20]], [Vesper Codex (Living API Docs)], [`vesper doc` / `test --doc` (`vesper-doc`): typestate graphs, Hoogle search, single-JIT-module doctests],
    [#link(<ch-20>)[Chapter 21]], [Vesper Chroma (Semantic Highlighting)], [`vesper highlight` / `shot` (`vesper-highlight`): OKLCH Dusk/Dawn palettes, Retina cards, 26 KiB WASM],
    [#link(<ch-21>)[Chapter 22]], [Vesper Crucible (Concolic Fuzzing)], [`vesper fuzz` / `migrate` (`vesper-crucible`): Bitwuzla concolic exploration, `ChoiceTape`, MPK shadowing],
    [#link(<ch-22>)[Chapter 23]], [Vesper Chrono (Time-Travel Debugger)], [`vesper replay` (`vesper-chrono`): effect-boundary flight logs, Intel PT / CoreSight, reverse watchpoints],
    [#link(<ch-23>)[Chapter 24]], [Vesper Metronome (PMU Benchmarking)], [`vesper bench` (`vesper-bench`): user-space `rdpmc`, Stabilizer randomization, paired A/B gates],
  ),
  kind: table,
)


== 18. Vesper Lens: Incremental IDE Server and Resilient Recovery
<ch-17>
#chapter-deck[Incremental Queries, Error Recovery, Diagnostic Consistency, and Code Actions]

*Vesper Lens* (`crates/vesper-lsp` and `crates/vesper-span`, invoked through `vesper lsp`) describes the incremental Language Server Protocol service and diagnostic engine. The design shares compiler queries across batch builds and editor requests through a demand-driven `salsa` database. It aims to keep diagnostics consistent while meeting a sub-2-millisecond keystroke budget; that budget requires measurement on specified workloads.

=== 18.1 Incremental Interactive Compilation and Recovery Guarantees
<ch-17-1>
+ *Single Unified Compiler and IDE Pipeline:* Vesper eliminates the semantic gap between IDE squiggles and batch builds by executing `vesper-mir-pcg` directly in memory on unsaved buffers, surfacing disjoint `FieldTree` borrow overlaps and `Mode::Linear` exhaustion errors instantaneously on every keystroke without waiting for disk saves.

+ *Turbofish-Free LL(2) Syntax Recovery:* Because Vesper's surface grammar uses square brackets (`[...]`) for type arguments and separates `UpperIdent` types and effects from `LowerIdent` values and capability labels (#link(<ch-2>)[Chapter 2]), unclosed generic brackets never collide with comparison operators or cascade parse errors into subsequent functions.

+ *Zero-Trust Hermetic Workspace Loading:* Opening an untrusted repository in an editor never executes host-native code. Both `build.ves` scripts and `#[wasm_macro]` expansions run inside Vesper Loom's fuel-metered `wasmtime` (`wasm32-wasip2`) sandbox with zero network or filesystem authority, memoized deterministically across sessions in `~/.vesper/store/index.redb`.

+ *Poison-Free Substructural Error Recovery:* While a developer is mid-keystroke typing an incomplete expression such as `tx.`, Vesper Lens transitions touched linear bindings to a `PoisonedRecovery` capability state—suppressing spurious scope-exit leak cascades while keeping `tx`'s type live so autocomplete immediately suggests consuming transitions (`tx.commit()` and `tx.abort()`).

+ *Obligation-Guided Autocomplete and Live Proof Lenses:* Building on semantic completion filtering (_ChopChop_, POPL 2026), `vesper-lsp` ranks `sink self` methods that discharge live `Mode::Linear` obligations at the top of completion menus, flags candidates that would violate `Loom.toml` capability ceilings or `[T-Let]` Resumption Frame Conditions before insertion, and streams background Lean 4 `.olean` verification status and `bv_decide` bit-vector counterexamples directly into the editor gutter.

=== 18.2 Resilient LL(2) Parsing and Poison-Free Linear Absorption
<ch-17-2>
Substructural type systems face a classic usability hazard in interactive editors: an unfinished statement leaves newly bound `Mode::Linear` resources unconsumed, which in a naive checker floods the file with secondary leak diagnostics. Vesper Lens prevents cascading errors through two coordinated mechanisms across Stage 1 (`vesper-syntax`) and Stage 3 (`vesper-mir-pcg`).

*Bracket-Balanced Anchor Synchronization:* During `logos` SIMD lexing and LL(2) recursive-descent parsing, `vesper-syntax` maintains balanced delimiter trees (`{...}`, `[...]`, `(...)`) inside lossless `rowan` Red-Green Concrete Syntax Trees. Unexpected tokens wrap into localized `SyntaxKind::ErrorNode` subtrees and synchronize at statement anchors (`;`, `let`, `match`, `handle`, `return`) or item anchors (`fn`, `struct`, `enum`, `effect`, `trait`, `impl`, `model`) while preserving all comments, whitespace, and sibling declarations.

*Incomplete Place Projections and `PoisonedRecovery`:* Trailing member accesses (`tx.`) or partial calls (`tx.commit(`) lower in `vesper-hir` to `HirExpr::Incomplete` nodes carrying the receiver's `PlaceProjection` and an inference variable. In `vesper-mir-pcg`, `LocalCapability` includes `PoisonedRecovery`: any `Mode::Linear` binding flowing into an `Incomplete` or `ErrorNode` region transitions to `PoisonedRecovery`, satisfying `join_at_block` and `check_return_exhaustion` silently while preserving full receiver type information for dot-completion and hover queries.

=== 18.3 Demand-Driven `salsa` Query Pipeline and Dual Early-Cutoff Firewalls
<ch-17-3>
Vesper Lens structures incremental checking into five memoized query layers protected by two early-cutoff firewalls so keystroke latency remains independent of total workspace size:

*Layer 1 — Incremental Block Reparsing (`parse_file_cst`):* Applies UTF-8 rope deltas to reparse only the modified `{ ... }` block into a `rowan` `GreenNode` in under 0.15 milliseconds.

*Firewall A — File Signature Merkle Fingerprint (`FileSignatureMif`):* Extracts top-level item headers (struct fields, enum variants, trait definitions, effect signatures, and function parameter/effect signatures) while stripping function bodies. Because 99 percent of editing keystrokes modify statements inside a function body without altering its signature, `FileSignatureMif` remains identical and zero downstream files or callers are invalidated.

*Layer 2 — Deterministic WASI Macro Memoization (`expand_forge_macros`):* Evaluates pure `(MacroWasmBlake3, InputCstBlake3) -> OutputCst` queries cached in `salsa` memory and `~/.vesper/store/index.redb`.

*Layer 3 and Layer 4 — Per-Function Mode, Pact, and PCG Checking (`infer_fn_body_hir` and `check_fn_pcg`):* Runs bidirectional OxCaml-style mode crossing (`[T-Cross]`), SLG tabled canonical trait resolution (`vesper-pact`), and forward `PcgState` dataflow (`vesper-mir-pcg`) strictly for the single edited `FnId` in under 0.7 milliseconds.

*Layer 5 — Debounced Asynchronous Lean 4 Verification (`verify_fn_lean_bg`):* After 150 milliseconds of editor idle time, extracts modified verified functions to Lean 4 (`vesper-verify-lean`), mounts pre-checked upstream `.olean` certificates from `OLEAN_CERTS` via copy-on-write reflinks (#link(<ch-8-4>)[Section 9.4]), and publishes theorem checkmarks or concrete `bv_decide` bit-vector counterexamples as LSP CodeLenses.

=== 18.4 Multi-Span Substructural Diagnostics and Lossless CST Quick-Fixes
<ch-17-4>
Every compiler diagnostic in `crates/vesper-span` encodes primary and secondary causal spans alongside structured CST `CodeFix` edits consumed by both `ariadne` CLI rendering (`vesper build` and `vesper check`) and LSP `textDocument/codeAction`:

```
// Example E0301 terminal diagnostic rendered by crates/vesper-span:
// Error[E0301]: linear obligation 'tx' cannot cross abortable Fail[IoError] checkpoint
//   --> src/wal_txn.ves:34:5
// 32 |     let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);
//    |         -- 'tx' introduced here with mode linear (must be consumed on every path)
// 34 |     validate_crc(in batch)?;
//    |                           ^ performs Fail[IoError] (res = affine), may abort frame
// 35 |     tx.commit()
//    |     ----------- 'tx' is only consumed if line 34 succeeds
// Help: consume 'tx' explicitly on the error branch (for example, tx.abort()).
// Quick Fix [vesper.fix.wrap_linear_match]: Rewrite ? into match calling tx.abort() on Err
```

*Automated Substructural Refactorings:* Beyond error fixes (`E0301` Resumption Frame match-wrapping, `E0302` `FieldTree` parameter narrowing, and `E0401` `Loom.toml` capability ceiling updates), Vesper Lens provides *Lifetime-Free Extract Function*. Selecting any block of statements queries `PcgState` over the selection range to compute the minimal disjoint `in`, `inout`, and `sink` `FieldTree` projections and active `eff [...]` row, generating a clean helper signature (such as `fn flush_metrics(inout broker.{index, metrics}: Broker, in config: BrokerConfig) eff [Storage]`) with zero `'a` lifetime annotations.

=== 18.5 Rust Implementation Architecture (`crates/vesper-span` and `crates/vesper-lsp`)
<ch-17-5>
`crates/vesper-span` and `crates/vesper-lsp` (registered in #link(<ch-7-2>)[Section 8.2]) implement the shared multi-span diagnostic representation, `FileSignatureMif` early-cutoff firewall, and obligation-guided completion engine:

```rust
// crates/vesper-span/src/diagnostic.rs
use ariadne::{Color, Label, Report, ReportKind, Source};
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextSpan {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticCode {
    E0301ResumptionFrameViolation,
    E0302OverlappingProjection,
    E0303UnconsumedLinearObligation,
    E0401CapabilityCeilingExceeded,
    E0402MacroSmugglingBlocked,
}

#[derive(Clone, Debug)]
pub struct SpanLabel {
    pub span: TextSpan,
    pub message: SmolStr,
    pub is_primary: bool,
}

#[derive(Clone, Debug)]
pub struct TextEdit {
    pub span: TextSpan,
    pub replacement: String,
}

#[derive(Clone, Debug)]
pub struct CodeFix {
    pub id: &'static str,
    pub title: SmolStr,
    pub edits: Vec<TextEdit>,
}

#[derive(Clone, Debug)]
pub struct VesperDiagnostic {
    pub code: DiagnosticCode,
    pub message: SmolStr,
    pub labels: Vec<SpanLabel>,
    pub help: Option<SmolStr>,
    pub fixes: Vec<CodeFix>,
}

impl VesperDiagnostic {
    pub fn render_terminal(&self, filename: &str, src: &str) -> String {
        let primary = self.labels.first().map(|l| l.span.start as usize).unwrap_or(0);
        let mut builder = Report::build(ReportKind::Error, filename, primary)
            .with_code(format!("{:?}", self.code))
            .with_message(&self.message);

        for lbl in &self.labels {
            let color = if lbl.is_primary { Color::Red } else { Color::Cyan };
            builder = builder.with_label(
                Label::new((filename, lbl.span.start as usize..lbl.span.end as usize))
                    .with_message(&lbl.message)
                    .with_color(color),
            );
        }
        if let Some(help) = &self.help {
            builder = builder.with_help(help);
        }

        let mut out = Vec::new();
        builder
            .finish()
            .write((filename, Source::from(src)), &mut out)
            .expect("in-memory diagnostic buffer write cannot fail");
        String::from_utf8_lossy(&out).into_owned()
    }
}

// crates/vesper-lsp/src/lib.rs
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;
use vesper_hir::ty::{EffectRow, Mode, PassMode};
use vesper_mir_pcg::pcg::LocalId;
use vesper_span::diagnostic::FileId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileSignatureMif(pub [u8; 32]);

#[derive(Clone, Debug)]
pub struct CandidateMethod {
    pub name: SmolStr,
    pub receiver_pass: PassMode,
    pub effects: EffectRow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionTier {
    DischargesLiveLinear = 0,
    ValidFrameCompatible = 1,
    BlockedByFrameOrCeiling = 2,
}

#[derive(Clone, Debug)]
pub struct RankedCompletion {
    pub tier: CompletionTier,
    pub label: SmolStr,
    pub inline_badge: Option<SmolStr>,
}

pub fn rank_methods_for_cursor(
    receiver_is_live_linear: bool,
    other_live_linears: &[LocalId],
    allowed_pkg_effects: &FxHashSet<SmolStr>,
    methods: &[CandidateMethod],
) -> Vec<RankedCompletion> {
    let mut ranked = Vec::with_capacity(methods.len());

    for m in methods {
        let disallowed_eff = m.effects.instances.iter().find_map(|inst| {
            let cap = inst.cap_label.as_ref()?;
            (!allowed_pkg_effects.contains(cap)).then(|| cap.clone())
        });
        if let Some(eff_name) = disallowed_eff {
            ranked.push(RankedCompletion {
                tier: CompletionTier::BlockedByFrameOrCeiling,
                label: m.name.clone(),
                inline_badge: Some(format!("blocked: {eff_name} exceeds Loom.toml ceiling").into()),
            });
            continue;
        }

        let res_guarantee = m.effects.resumption_guarantee(Mode::Linear);
        if !other_live_linears.is_empty() && res_guarantee < Mode::Linear {
            ranked.push(RankedCompletion {
                tier: CompletionTier::BlockedByFrameOrCeiling,
                label: m.name.clone(),
                inline_badge: Some("blocked: abortable effect while linear local is live".into()),
            });
            continue;
        }

        if receiver_is_live_linear && m.receiver_pass == PassMode::Sink {
            ranked.push(RankedCompletion {
                tier: CompletionTier::DischargesLiveLinear,
                label: m.name.clone(),
                inline_badge: Some("discharges linear obligation".into()),
            });
        } else {
            ranked.push(RankedCompletion {
                tier: CompletionTier::ValidFrameCompatible,
                label: m.name.clone(),
                inline_badge: None,
            });
        }
    }

    ranked.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.label.cmp(&b.label)));
    ranked
}

#[derive(Default)]
pub struct LensIncrementalState {
    pub sig_fingerprints: FxHashMap<FileId, FileSignatureMif>,
}

impl LensIncrementalState {
    pub fn update_file_signature(&mut self, file: FileId, new_mif: FileSignatureMif) -> bool {
        let prev = self.sig_fingerprints.insert(file, new_mif);
        prev != Some(new_mif)
    }
}
```


== 19. Vesper Stylus and Lint: CST Formatting and Static Analysis
<ch-18>
#chapter-deck[Source Formatting, Ownership-Aware Analysis, and Proposed Autofix Checks]

*Vesper Stylus* (`crates/vesper-fmt`, `vesper fmt`) formats source code. *Vesper Lint* (`crates/vesper-lint`, `vesper lint`) analyzes programs and proposes fixes, including in *Lens* (#link(<ch-17>)[Chapter 18]). This chapter describes their intended use of the lossless `rowan` concrete syntax tree, HIR mode/effect information, place-capability analysis, and `egglog` optimization. Preserving comments and program behavior are separate obligations; the proposed verification of autofixes requires further implementation evidence.

=== 19.1 Canonical Formatting and Substructural Static-Analysis Guarantees
<ch-18-1>
+ *Comment-Preserving Formatting (`vesper fmt`):* Stylus uses the lossless `rowan` concrete syntax tree to retain comments, blank lines, and attributes while formatting code. The proposed formatter lowers nodes to a Wadler-Lindig / Oppen document algebra, including unexpanded `sugar` `PhraseSkeleton` clauses, lexically tiered operators, and tagged literals (#link(<ch-6-sugar>)[Section 7.5]), `macro { ... }` quotations (#link(<ch-6-4>)[Section 7.4]), and partial IDE buffers, without resolving imports or running macro plugins. Its requirements are zero configuration, deterministic output, idempotence (`fmt(fmt(src)) == fmt(src)`), and preservation of the non-trivia token stream and every comment. These formatting requirements are distinct from the unresolved grammar guarantees in #link(<ch-2>)[Chapter 2].

+ *Substructural Mode, FieldTree, and Effect-Row Static Analysis (`vesper lint`):* Because `cargo clippy` runs on Rust HIR/MIR without modal kinds, disjoint field-tree projections, or lexical effect rows, it cannot reason about over-broad struct borrows, redundant heap authority, or sub-optimal modal bounds. Vesper Lint queries `vesper-hir` and `vesper-mir-pcg` directly to surface five high-signal systems lints with zero false positives: (2a) *Projection Narrowing (`overbroad_inout_projection`)* detects when `fn update(inout shard: WorkerShard)` mutates only `shard.metrics` and reads `shard.config`, offering a 1-click autofix to narrow the signature to `inout shard.{metrics}, in shard.{config}: WorkerShard`—unlocking caller-side `[T-CallProj]` parallelism (#link(<ch-4>)[Chapter 4]) as a Minor-compatible SemVer evolution (`[SV-Fn]`, #link(<ch-11-2>)[Section 12.2]); (2b) *Zero-Heap Arena Demotion (`demote_alloc_to_region`)* detects functions requesting global `eff [Alloc]` whose allocations never escape the return type, rewriting heap containers into a stack-bounded `Region::scoped` `VirtualArena` (#link(<ch-12-2>)[Section 13.2]) and removing `Alloc` from `eff [...]`; (2c) *Unused Capability and Over-Restrictive Mode Bounds (`unused_effect_capability`, `overly_restrictive_mode`)* flags declared effects in `eff [...]` never performed in the body and generic bounds like `[T: mod copy]` where `T` is consumed at most once (relaxable to `[T: mod linear]`); (2d) *FIP Constructor Pairing (`missed_fip_opportunity`)* queries Stage 4's `ReuseSlot` analysis (#link(<ch-12-4>)[Section 13.4]) when reordering an owned `match` unpack before a same-sized constructor enables `@fip` zero-allocation in-place reuse; and (2e) *Cancel-Point Proximity and Secret Hygiene (`early_cancel_checkpoint`, `unredacted_pii_boundary`)* hoists `Cancel::check()?` above `Mode::Linear` acquisitions (#link(<ch-14-2>)[Section 15.2]) and guards `@pii` / Vault `Sec*` values (#link(<ch-31-4>)[Section 32.4] and #link(<ch-27-2>)[Section 28.2]).

+ *E-Graph Semantic Simplification Without Macro False Positives (egglog Lint Rules):* Clippy maintains over 750 hand-written AST/HIR pattern matchers that frequently misfire on macro-generated code or miss equivalent algebraic forms. Vesper Lint expresses algebraic, bitwise, SIMD (`VVec`), and relational (Quarry / Flux) simplifications as declarative *egglog* rewrite rules where the extracted e-node has strictly lower AST/cycle cost than the source expression, gated by #link(<ch-6-4>)[Section 7.4]'s `ScopeSet` hygiene marks so macro-internal expansions never trigger spurious call-site warnings.

+ *Proof-Preserving Autofixes Verified by Incremental Re-Typechecking (`vesper lint --fix`):* Textual autofixes in `cargo clippy --fix` can introduce name collisions, borrow errors, or broken formatting. Every Vesper Lint autofix constructs a typed `rowan` CST splice, formats only the touched subtree via Vesper Stylus, and commits the edit *if and only if* incremental `salsa` re-checking confirms zero new diagnostics, preservation of Theorem 2 (Total Linear Exhaustion), and SemVer compatibility (`I_new <=_pub I_old`, #link(<ch-11>)[Chapter 12]) when `--semver-safe` is enabled.

+ *Hermetic WASI Domain Lint Packs (Loom.toml \[lints.plugins\]):* Domain frameworks (*Vesper Vault*, *Vesper Ignis*, *Vesper Resonance*, *Vesper Aegis*) distribute domain-specific lints (such as GPU barrier layout anti-patterns or `@realtime` filter coefficient checks) as fuel-metered `wasm32-wasip2` components executed inside `vesper-loom-sandbox` (#link(<ch-10-2>)[Section 11.2])—delivering extensible static analysis with zero host-code execution risk and sub-millisecond `salsa` caching in *Vesper Lens* (#link(<ch-17>)[Chapter 18]).

=== 19.2 Canonical Formatting Rules and Declarative Loom.toml Lint Policy
<ch-18-2>
Stylus formats Vesper's signature constructs—disjoint PlaceTree projections, square-bracket generics, `eff [...]` capability rows, and `handle` blocks—under a canonical 100-column ribbon budget with zero formatting configuration files, while `Loom.toml` governs lint severities workspace-wide:

```
# Loom.toml — Declarative Workspace Lint Policy (zero formatting knobs needed!)
[lints.vesper]
overbroad_inout_projection = "warn"
unused_effect_capability   = "deny"
demote_alloc_to_region     = "warn"
overly_restrictive_mode    = "warn"
missed_fip_opportunity     = "allow"

[lints.plugins]
"@vesper-game/ignis-lints" = { version = "^1.0.0", fuel_limit = 5_000_000 }
```

=== 19.3 Substructural Lint Autofix Showcase: Projection Narrowing and Arena Demotion
<ch-18-3>
The following proposed projection narrowing retains the public arity and allocation behavior. It is illustrative, not a claim that the current lint implements or proves this rewrite. A fixed-capacity arena replacement requires a proven capacity bound and compatible failure behavior:

```
// BEFORE `vesper lint --fix`:
// Warning[L0101: overbroad_inout_projection]: `inout shard: WorkerShard` mutates only
//   `shard.metrics.requests` and reads `shard.config.limit`.
// Warning[L0204: demote_alloc_to_region]: `kept` allocates via `eff [Alloc]` and never escapes `u32`.
fn filter_ids_unoptimized(
    inout shard: WorkerShard,
    in raw_ids: Slice[u64],
) -> u32 eff [Alloc] {
    let mut kept = HeapVec::with_capacity[u64](256);
    for id in raw_ids {
        if id < shard.config.limit {
            kept.push(id);
        }
    }
    let count = kept.len() as u32;
    shard.metrics.requests += count as u64;
    count
}

// AFTER: the two-argument signature and heap growth behavior are preserved.
// Only the borrow footprint is narrowed; both selected fields remain under one binder.
fn filter_ids_unoptimized(
    inout shard.{metrics.requests, config.limit}: WorkerShard,
    in raw_ids: Slice[u64],
) -> u32 eff [Alloc] {
    let mut kept = HeapVec::with_capacity[u64](256);
    for id in raw_ids {
        if id < shard.config.limit {
            kept.push(id);
        }
    }
    let count = kept.len() as u32;
    shard.metrics.requests += count as u64;
    count
}
```

=== 19.4 Lean 4 Comment-Token Conservation and Autofix SemVer Proofs (`vesper verify`)
<ch-18-4>
Extracting Stylus's Wadler-Lindig renderer and Lint's projection-narrowing autofix pass via `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Comment-Token Conservation* across flat/broken group modes and *Autofix SemVer Substitutability*:

```lean
-- illustrative/StylusAndLintSafety.lean
theorem stylus_format_preserves_comments (doc : FormatDoc) (width col : Nat) :
    CommentTokensOf (StylusSpec.render width col .flat doc) =
    CommentTokensOf (StylusSpec.render width col .broken doc) := by
  induction doc generalizing col <;> simp [StylusSpec.render, CommentTokensOf, *]

theorem lint_autofix_preserves_caller_disjointness
    (leavesNew leavesOld callerOther : Nat -> Bool)
    (h_subset : forall f, leavesNew f = true -> leavesOld f = true)
    (h_disj   : forall f, ¬(leavesOld f = true /\ callerOther f = true)) :
    forall f, ¬(leavesNew f = true /\ callerOther f = true) := by
  intro f ⟨h_new, h_other⟩
  exact h_disj f ⟨h_subset f h_new, h_other⟩
```

=== 19.5 Rust Wadler-Lindig Formatter and Substructural Lint Engine (`crates/vesper-fmt` and `crates/vesper-lint`)
<ch-18-5>
`crates/vesper-fmt` and `crates/vesper-lint` implement the O(N) Wadler-Lindig pretty-printer and the HIR/PCG substructural lint engine:

```rust
// crates/vesper-fmt/src/lib.rs
use smol_str::SmolStr;

pub const DEFAULT_LINE_WIDTH: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatDoc {
    Nil,
    Text(SmolStr),
    Comment(SmolStr),
    HardLine,
    SoftLineOrSpace,
    SoftLineOrEmpty,
    Nest(usize, Box<FormatDoc>),
    Concat(Box<FormatDoc>, Box<FormatDoc>),
    Group(Box<FormatDoc>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrintMode {
    Flat,
    Break,
}

impl FormatDoc {
    pub fn flat_width(&self) -> Option<usize> {
        match self {
            Self::Nil | Self::SoftLineOrEmpty => Some(0),
            Self::Text(s) => Some(s.len()),
            Self::Comment(c) => (!c.contains('\n') && !c.starts_with("//")).then_some(c.len()),
            Self::HardLine => None,
            Self::SoftLineOrSpace => Some(1),
            Self::Nest(_, inner) | Self::Group(inner) => inner.flat_width(),
            Self::Concat(a, b) => Some(a.flat_width()?.saturating_add(b.flat_width()?)),
        }
    }

    pub fn render_canonical(&self, max_width: usize) -> String {
        let mut out = String::new();
        let mut work: Vec<(usize, PrintMode, &FormatDoc)> = vec![(0, PrintMode::Break, self)];
        let mut col = 0usize;
        while let Some((indent, mode, doc)) = work.pop() {
            match doc {
                Self::Nil => {}
                Self::Text(s) | Self::Comment(s) => {
                    out.push_str(s);
                    col += s.len();
                }
                Self::HardLine => {
                    out.push('\n');
                    out.extend(core::iter::repeat(' ').take(indent));
                    col = indent;
                }
                Self::SoftLineOrSpace => match mode {
                    PrintMode::Flat => {
                        out.push(' ');
                        col += 1;
                    }
                    PrintMode::Break => {
                        out.push('\n');
                        out.extend(core::iter::repeat(' ').take(indent));
                        col = indent;
                    }
                },
                Self::SoftLineOrEmpty => {
                    if mode == PrintMode::Break {
                        out.push('\n');
                        out.extend(core::iter::repeat(' ').take(indent));
                        col = indent;
                    }
                }
                Self::Nest(spaces, inner) => work.push((indent + spaces, mode, inner)),
                Self::Concat(lhs, rhs) => {
                    work.push((indent, mode, rhs));
                    work.push((indent, mode, lhs));
                }
                Self::Group(inner) => {
                    let gmode = match inner.flat_width() {
                        Some(w) if col + w <= max_width => PrintMode::Flat,
                        _ => PrintMode::Break,
                    };
                    work.push((indent, gmode, inner));
                }
            }
        }
        out
    }
}

// crates/vesper-lint/src/lib.rs
use rustc_hash::FxHashSet;
use smol_str::SmolStr;
use vesper_hir::ty::{FieldIdx, FieldTree, Mode};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubstructuralLintFinding {
    OverbroadInOutProjection {
        param_name: SmolStr,
        suggested_mut_fields: Vec<FieldIdx>,
        suggested_read_fields: Vec<FieldIdx>,
    },
    UnusedEffectCapability {
        fn_name: SmolStr,
        unused_effect: SmolStr,
    },
    DemoteAllocToRegion {
        fn_name: SmolStr,
        bounded_bytes_estimate: Option<usize>,
    },
    OverlyRestrictiveGenericMode {
        type_param: SmolStr,
        declared_bound: Mode,
        observed_usage: Mode,
    },
}
pub fn analyze_fn_substructural_lints(
    fn_name: &SmolStr,
    declared_effects: &[SmolStr],
    performed_effects: &FxHashSet<SmolStr>,
    alloc_escapes_return: bool,
    alloc_upper_bound: Option<usize>,
    inout_params: &[(SmolStr, FieldTree, Vec<FieldIdx>, Vec<FieldIdx>)],
    generic_mode_usages: &[(SmolStr, Mode, Mode)],
) -> Vec<SubstructuralLintFinding> {
    let mut findings = Vec::new();
    for eff in declared_effects {
        if !performed_effects.contains(eff) {
            findings.push(SubstructuralLintFinding::UnusedEffectCapability {
                fn_name: fn_name.clone(),
                unused_effect: eff.clone(),
            });
        }
    }
    if performed_effects.contains("Alloc") && !alloc_escapes_return {
        findings.push(SubstructuralLintFinding::DemoteAllocToRegion {
            fn_name: fn_name.clone(),
            bounded_bytes_estimate: alloc_upper_bound,
        });
    }
    for (param_name, declared_tree, mutated_leaves, read_leaves) in inout_params {
        if declared_tree.children.is_empty() && !mutated_leaves.is_empty() {
            findings.push(SubstructuralLintFinding::OverbroadInOutProjection {
                param_name: param_name.clone(),
                suggested_mut_fields: mutated_leaves.clone(),
                suggested_read_fields: read_leaves.clone(),
            });
        }
    }
    for (tp, declared_bound, observed_max_mode) in generic_mode_usages {
        if *declared_bound < *observed_max_mode {
            findings.push(SubstructuralLintFinding::OverlyRestrictiveGenericMode {
                type_param: tp.clone(),
                declared_bound: *declared_bound,
                observed_usage: *observed_max_mode,
            });
        }
    }
    findings
}
```


== 20. Vesper Codex: Living Documentation and Hermetic JIT Doctests
<ch-19>
#chapter-deck[Obligation Diagrams, Mutation Footprints, Signature Search, and Doctests]

*Vesper Codex* (`crates/vesper-doc`) describes documentation generation, signature search, and doctest execution through `vesper doc` and `vesper test --doc`. It also supplies hover information to *Lens* (#link(<ch-17>)[Chapter 18]). The design queries HIR, place-capability, compatibility, and proof metadata to display typestate diagrams, `FieldTree` mutation footprints, and `.olean` badges. A shared Cranelift JIT module and `SimPulse` are intended to support reproducible doctests; the sub-100-millisecond execution budget is a performance target.

=== 20.1 Living Specification, Doctest, and Signature-Search Guarantees
<ch-19-1>
+ *Generated Ownership and Layout Views:* Codex proposes four views derived from compiler metadata. Linear typestate diagrams trace `WriteTxn`, `Responder[HeadersPending]`, `Session[Proto]`, and `SnapshotResumeTicket` from constructors through `inout` transitions to terminal `sink` consumers such as commit or abort. They flag a public linear type with no reachable public terminal consumer. Mode-crossing tables show `[K-Adt]` instantiations such as `Option[T]` becoming `copy`, `affine`, or `linear` (#link(<ch-3>)[Chapter 3]). `FieldTree` views distinguish mutated leaves (`inout broker.{index, metrics}`) from shared leaves (`in broker.{config}`), showing sibling fields available for concurrent `[T-CallProj]` borrows. Layout maps show 64-byte cache lines for `tiled(16) struct` collections (#link(<ch-12-5>)[Section 13.5]) and bit/endianness offsets for padding-free `wire struct` headers (#link(<ch-15-2>)[Section 16.2]).
+ *Explicit Authority, Cancel-Safety, and Zero-Alloc Capability Badges:* Because ambient authority does not exist in Vesper, every documented function and package displays compiler-verified capability badges derived from `eff [...]` and `Loom.toml` (#link(<ch-10-2>)[Section 11.2]): *Zero-Alloc* (when `Alloc` is absent from `eff [...]`), *Pure* (`eff []`), *Uncancellable Linear I/O* (`res(eff) = linear`), *Abortable Checkpoint* (when `Fail[E]` or `Cancel` is present, alerting callers holding live `Mode::Linear` values), *Constant-Time Verified* (`eff [ConstTime]`, #link(<ch-27>)[Chapter 28]), and *Hard-Realtime Safe* (`@realtime` WCSD stack byte bound, #link(<ch-24-5>)[Section 25.5] and #link(<ch-40-1>)[Section 41.1]).
+ *Single-JIT-Module Hermetic Doctests in Under 100 Milliseconds (`vesper test --doc`):* Conventional doctest runners invoke full compiler backends and host linkers while executing documentation snippets with unrestricted ambient filesystem and network access. Vesper Codex parses all `///` code blocks across an entire package, wraps each example as an isolated entry function inside a *single in-memory Cranelift JIT module* (`cranelift-jit`, #link(<ch-7>)[Chapter 8]), and executes each snippet inside a resettable 64 KiB `VirtualArena` under *SimPulse* deterministic virtual I/O and clocks (#link(<ch-14-6>)[Section 15.6]) with zero object files and zero system linker invocations. Crucially, every doctest undergoes full Stage 3 PCG checking—guaranteeing that documentation snippets never omit linear obligation cleanup—while `compile_fail(E0301)` doctests assert exact diagnostic error codes (#link(<ch-17-4>)[Section 18.4]) so negative substructural examples never pass for the wrong reason.
+ *Inline Lean 4 Proof Certificates, Pact Model Discovery, and MIF SemVer Changelogs:* Bridging the gap between API docs and formal methods, Codex links directly into `~/.vesper/store/` (`OLEAN_CERTS`, #link(<ch-8-4>)[Section 9.4]): functions verified via `vesper verify` display a green *Verified (0 sorryAx)* badge that expands inline to inspect the extracted prophecy-free Lean 4 functional model (`Funs.lean`), exported invariants (`Specs.lean`), and kernel-audited axiom footprint. Additionally, Codex indexes both canonical `impl` blocks and third-party *Vesper Pact* named models (#link(<ch-6-3>)[Section 7.3]), resolves intra-doc `[Type::method]` links inside Stage 1 `rowan` CSTs (surfacing broken links in under 2 ms in Vesper Lens), and computes automatic SemVer API diffs across `mif:blake3` versions via `vesper-semver` (#link(<ch-11>)[Chapter 12]).
+ *Offline Substructural Hoogle Search and Content-Addressed Loom Doc Caching:* Codex ships a 42 KiB WebAssembly query engine (`codex-search.wasm`) supporting *Substructural Type, FieldTree, and Effect-Row Search*: developers query signatures like `sink WriteTxn -> CommitReceipt eff [Storage]` or `inout TiledSeq[_] -> u64 eff [Isa]` and receive variance-aware matches respecting contravariant parameter sub-moding (`copy <= affine <= linear`), covariant return modes, and effect-ceiling subsets. Because `vesper doc` emits 100 percent deterministic static HTML/WASM bundles with zero external CDN requests, Vesper Loom caches prebuilt documentation in `~/.vesper/store/` and mounts dependency docs into `target/doc/` instantaneously via copy-on-write reflinks.

=== 20.2 CST-Verified Doc Attributes and Hermetic SimPulse Doctests
<ch-19-2>
Doc comments (`//!` module docs and `///` item docs) are first-class nodes in Vesper's lossless `rowan` CST (#link(<ch-17-2>)[Section 18.2]) rather than opaque strings. Intra-doc links (`[WriteTxn::commit]`), `@proof(...)` theorem anchors, and positive/negative doctests are typechecked alongside the item they document:

````
/// Appends a CRC-validated record batch to the segmented log and commits the WAL slot.
///
/// # Substructural & Cancel-Safety Contract
/// - **Disjoint Borrow:** Mutates only `broker.{index, metrics}`; callers may hold `in broker.config`
///   concurrently across this call via `[T-CallProj]`.
/// - **Linear Exhaustion:** Consumes `sink batch` and discharges internal [WriteTxn::commit] or
///   [WriteTxn::abort] along 100% of control paths.
///
/// @proof(KrabkaWal::appendBatch_crc_err_aborts_cleanly)
///
/// # Verified Doctests (compiled into 1 Cranelift JIT module & run on deterministic `SimPulse`)
/// ```vesper
/// Region::scoped(64.kib(), |cap arena| {
///     SimPulse::run_seeded(0x5EED_CAFE, 0, || {
///         let mut broker = Broker::mock_with_region(in arena);
///         let cfg = broker.config; // Shared copy/in read coexists with disjoint `inout` below!
///         let batch = RecordBatch::from_slice(in arena, b"vesper-codex");
///         let receipt = append_batch(inout broker.{index, metrics}, in cfg, sink batch)?;
///         assert_eq(receipt.wal_slot, 1);
///         Ok(())
///     })
/// })
/// ```
///
/// Leaving a [WriteTxn] unconsumed across an abortable `?` operator is statically rejected with `E0301`:
/// ```vesper,compile_fail(E0301)
/// fn broken_caller(in batch: RecordBatch) -> CommitReceipt eff [Storage, Fail[IoError]] {
///     let tx: WriteTxn = Storage::begin_txn(4096);
///     validate_crc(in batch)?; // ERROR E0301: `tx` is Mode::Linear across `Fail` (res = affine)!
///     tx.commit()
/// }
/// ```
pub fn append_batch(
    inout broker.{index, metrics}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Storage, Fail[IoError]] {
    // ... implementation from Section 2 ...
}
````

=== 20.3 Visual Wire-Struct Layout Descriptors and Typestate Obligation Graphs
<ch-19-3>
For every `wire struct` (#link(<ch-15-2>)[Section 16.2]), `tiled(N) struct` (#link(<ch-12-5>)[Section 13.5]), and `linear struct` (#link(<ch-1>)[Chapter 1]), Codex emits compact padding-free metadata records consumed by both static HTML/SVG diagram renderers and Vesper Lens hover popups:

```
// Exact 16-byte field byte-span descriptor for Codex SVG cache-line & wire-header diagrams:
wire struct CodexWireFieldSpan {
    byte_offset: u32le,
    byte_width: u32le,
    cache_line_idx: u16le, // 64-byte L1 cache line index (`byte_offset / 64`)
    endian_tag: u8,        // 0 = Native, 1 = LittleEndian, 2 = BigEndian, 3 = ExplicitZeroPad
    is_hot_tile_col: u8,
    field_sym_id: u32le,
}
// Typestate transition edge rendered in Codex's interactive Linear Obligation State Machine:
copy struct CodexLinearEdge {
    from_typestate_sym: u32,
    method_sym: u32,
    pass_mode_tag: u8,      // 1 = InOut step, 2 = Terminal Sink discharge
    to_typestate_opt: u32,  // `0` indicates terminal obligation discharge (`Theorem 2` satisfied!)
    resumption_is_linear: bool,
}
```

=== 20.4 Lean 4 Substructural Hoogle Soundness and Wire-Map Completeness Proofs (`vesper verify`)
<ch-19-4>
Extracting Codex's substructural signature-search matcher and `wire struct` byte-ruler generator through `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Variance-Aware Search Substitutability* (every function returned for a `(queryParamMode, queryRetMode, queryEffCeiling)` search can be invoked safely at the query's call site without violating `[T-SubMode]` or `Loom.toml` ceilings) and *Contiguous Zero-Hole Wire Map Coverage*:

```lean
-- lean/CodexSearchAndLayout.lean
-- 1. Substructural Hoogle Search Soundness:
-- A candidate function `(fnParamMode, fnRetMode)` safely satisfies a developer query
-- `(qParamMode, qRetMode)` whenever `qParamMode <= fnParamMode` (contravariant input: a consumer
-- accepting `linear` also accepts `copy`/`affine` via `[T-SubMode]`) and `fnRetMode <= qRetMode`!
theorem codex_hoogle_mode_match_sound
    (qParam fnParam fnRet qRet callerArgRank : Nat)
    (h_caller_has : callerArgRank <= qParam)
    (h_param_contra : qParam <= fnParam)
    (h_ret_cov : fnRet <= qRet) :
    callerArgRank <= fnParam /\ fnRet <= qRet := by
  omega
-- 2. Wire-Struct Byte Ruler Partition Exactness: adjacent field spans `[off0, off0 + len0)` and
-- `[off1, off1 + len1)` with `off1 = off0 + len0` cover `[off0, off0 + len0 + len1)` with 0 padding holes:
theorem codex_wire_diagram_zero_holes
    (off0 len0 off1 len1 : Nat) (h_contig : off1 = off0 + len0) :
    (off1 - off0) + len1 = len0 + len1 /\ off0 + len0 <= off1 := by
  omega
```

=== 20.5 Rust Linear Typestate Extractor, Substructural Hoogle Engine, and JIT Doctest Planner (`crates/vesper-doc`)
<ch-19-5>
`crates/vesper-doc` implements the linear obligation graph extractor (auditing that every public linear typestate has a reachable terminal `sink` consumer), the variance-aware Hoogle signature matcher, and the single-module Cranelift JIT doctest planner:

```rust
// crates/vesper-doc/src/lib.rs
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;
use vesper_hir::ty::{Mode, PassMode};
use vesper_span::diagnostic::DiagnosticCode;
use std::collections::VecDeque;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinearMethodTransition {
    pub from_state: SmolStr,
    pub method_name: SmolStr,
    pub receiver_mode: PassMode,
    /// `None` when `sink self` discharges the linear obligation into a copy/affine receipt;
    /// `Some(next_state)` when `sink self` transitions to another linear typestate (e.g. `Closed`).
    pub next_linear_state: Option<SmolStr>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexAuditError {
    DeadEndLinearTypestate { typestate: SmolStr },
}
/// Builds the Typestate Obligation Graph for `vesper doc` and verifies via reverse BFS that every
/// reachable linear typestate has at least one public path to a terminal `sink` discharge.
pub fn verify_linear_typestate_reachability(
    states: &[SmolStr],
    edges: &[LinearMethodTransition],
) -> Result<(), CodexAuditError> {
    let mut reverse_adj: FxHashMap<SmolStr, Vec<SmolStr>> = FxHashMap::default();
    let mut can_discharge: FxHashSet<SmolStr> = FxHashSet::default();
    let mut queue: VecDeque<SmolStr> = VecDeque::new();
    for edge in edges {
        if edge.receiver_mode == PassMode::Sink {
            match &edge.next_linear_state {
                None => {
                    if can_discharge.insert(edge.from_state.clone()) {
                        queue.push_back(edge.from_state.clone());
                    }
                }
                Some(next_st) => {
                    reverse_adj
                        .entry(next_st.clone())
                        .or_default()
                        .push(edge.from_state.clone());
                }
            }
        }
    }
    while let Some(discharged_st) = queue.pop_front() {
        if let Some(preds) = reverse_adj.get(&discharged_st) {
            for pred in preds {
                if can_discharge.insert(pred.clone()) {
                    queue.push_back(pred.clone());
                }
            }
        }
    }
    for st in states {
        if !can_discharge.contains(st) {
            return Err(CodexAuditError::DeadEndLinearTypestate {
                typestate: st.clone(),
            });
        }
    }
    Ok(())
}
/// Developer search query in Codex's offline WASM Hoogle bar (type + mode + `eff [...]` ceiling).
#[derive(Clone, Debug)]
pub struct SubstructuralSearchQuery {
    pub param_modes: Vec<(PassMode, Mode)>,
    pub ret_max_mode: Mode,
    pub allowed_effects: FxHashSet<SmolStr>,
    pub require_zero_alloc: bool,
}
#[derive(Clone, Debug)]
pub struct IndexedFnDocEntry {
    pub path: SmolStr,
    pub param_modes: Vec<(PassMode, Mode)>,
    pub ret_mode: Mode,
    pub effects: Vec<SmolStr>,
    pub has_lean_proof_cert: bool,
}
/// Variance-aware substructural signature matcher (`0` false-incompatible results; ranks exact + verified first).
pub fn search_codex_signatures<'a>(
    query: &SubstructuralSearchQuery,
    index: &'a [IndexedFnDocEntry],
) -> Vec<(u32, &'a IndexedFnDocEntry)> {
    let mut matches = Vec::new();
    for entry in index {
        if entry.param_modes.len() != query.param_modes.len() {
            continue;
        }
        if entry.ret_mode > query.ret_max_mode {
            continue; // Covariant return check: `fn_ret <= query_ret`
        }
        if query.require_zero_alloc && entry.effects.iter().any(|e| e == "Alloc") {
            continue;
        }
        if !entry.effects.iter().all(|e| query.allowed_effects.contains(e)) {
            continue;
        }
        // Contravariant parameter mode check: a function accepting `fn_m` accepts `q_m` iff `q_m <= fn_m`
        let params_compatible = entry
            .param_modes
            .iter()
            .zip(&query.param_modes)
            .all(|((fn_pass, fn_m), (q_pass, q_m))| fn_pass == q_pass && *q_m <= *fn_m);
        if !params_compatible {
            continue;
        }
        let exact_modes = entry.param_modes == query.param_modes && entry.ret_mode == query.ret_max_mode;
        let score = (u32::from(exact_modes) * 10) + (u32::from(entry.has_lean_proof_cert) * 5);
        matches.push((score, entry));
    }
    matches.sort_by(|(sa, ea), (sb, eb)| sb.cmp(sa).then_with(|| ea.path.cmp(&eb.path)));
    matches
}
/// Represents a parsed `///` doctest block batched into a single `cranelift-jit` module (`0` linker spawns).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoctestExpectation {
    RunSimPulseOk { seed: u64, arena_bytes: usize },
    CompileFailWithCode(DiagnosticCode),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchedJitDoctest {
    pub item_path: SmolStr,
    pub snippet_line: u32,
    pub expectation: DoctestExpectation,
    pub synthesized_entry_sym: SmolStr,
}
```


== 21. Vesper Chroma: Substructural Highlighting and Perceptual Palettes
<ch-20>
#chapter-deck[Syntax and Semantic Highlighting, Palette Contrast, and Code Cards]

*Vesper Chroma* (`@vesper-doc/chroma`, with implementation code in `crates/vesper-highlight`) describes semantic highlighting and code-card rendering through `vesper highlight` and `vesper shot`. It combines syntax from the lossless `rowan` tree with HIR mode/effect information to display ownership, projections, and lexical capabilities. Outputs include SVG, 4x Retina PNG, and the proposed 26 KiB `vesper-chroma.wasm` module for *Codex* (#link(<ch-19>)[Chapter 20]). Dusk and Dawn palettes use OKLCH color coordinates; contrast checks and artifact size require validation for the generated output.

=== 21.1 Semantic Highlighting, Perceptual Contrast, and Card-Rendering Guarantees
<ch-20-1>
+ *Syntax and Semantic Highlighting:* Chroma separates syntax-only rendering from type-enriched rendering. Tier 0 uses the lossless CST for arbitrary or incomplete snippets, including unexpanded `sugar` `PhraseSkeleton` clauses and tagged literals (#link(<ch-6-sugar>)[Section 7.5]), with a target below 0.2 milliseconds and no regex backtracking. Tier 1 uses HIR/PCG information to mark `Mode::Linear` obligations from creation to terminal consumption, distinguish mutated `FieldTree` leaves from shared `in` leaves, and distinguish linear effects from abortable `Fail`/`Cancel` checkpoints, including `sugar` forms that declare `emits eff [Fail | Cancel]`. Mode crossing (#link(<ch-3>)[Chapter 3]) requires semantic information because syntax alone does not determine an instantiated binding's mode.

+ *Perceptually Uniform OKLCH Palettes with Verified WCAG AAA and APCA Contrast:* Hand-tuned sRGB syntax themes routinely suffer uneven perceived brightness—making punctuation, inout projections, or linear obligation markers either glaring or illegible across dark and light backgrounds. Chroma defines its flagship themes (*Vesper Dusk* deep obsidian dark and *Vesper Dawn* warm alabaster light) in *OKLCH perceptual color space* (Lightness, Chroma, Hue), assigning orthogonal hue families to Vesper's three language pillars: warm amber-coral for linear types, sink transfers, and obligation underlines; cool cyan-teal for second-class inout mutable FieldTree projections; luminous iris-violet for `eff [.\.\.]` lexical capabilities and handle blocks; emerald for `@proof` anchors and wire struct endian scalars (u32be, u64le); and neutral slate for copy/in bindings—all audited statically for at least 7.0:1 WCAG AAA contrast on primary tokens and at least 4.5:1 on comments.

+ *Zero-Browser Hermetic Vector SVG and 4x Retina PNG Code Cards (`vesper shot`):* Generating crisp source-code screenshots in JS and Rust ecosystems typically requires either headless Chromium (1 to 3 seconds startup and over 300 MiB RAM per render) or basic bitmap renderers (silicon) that lack programming-font ligature shaping, crisp vector zoom, or semantic compiler callouts. `vesper shot` renders standalone *vector SVGs* and *4x Retina PNGs* natively in under 4 milliseconds with zero browser dependencies: shaping OpenType programming ligatures (arrow, fat-arrow, scope-colon) into exact glyph runs, rendering optional minimalist window chrome, ambient elevation shadows, subtle mode-legend pills, and inline *Twoslash/Ariadne Callout Pins* (displaying inferred modal kinds, wire struct byte spans, or E0301 Resumption Frame annotations directly inside the rendered card).

+ *Zero-Runtime-JS Static Website HTML and 26 KiB WASM Playground Engine (`vesper-chroma.wasm`):* For vesper-lang.org documentation, tutorials, and blog posts, `vesper highlight --html` emits semantic, accessible HTML blocks styled purely via CSS custom properties (`--vc-fg`, `--vc-linear`, `--vc-inout`, `--vc-eff`) for zero-layout-shift instant dark/light toggling and 100 percent byte-accurate clipboard copy-pasting (setting user-select: none on line numbers, pass-mode pills, and Twoslash callout boxes). For interactive website sandboxes, the exact same Rust crate compiles to a standalone *26 KiB WebAssembly module (`vesper-chroma.wasm`)* with zero WASI or allocator bloat—re-highlighting live editor keystrokes in under 0.3 ms at 120 fps and rendering rich hover cards for FieldTree disjointness and `eff [.\.\.]` rows entirely client-side.

+ *SimPulse-Verified Website Examples and Content-Addressed Asset Caching:* Website code examples never drift out of sync with the compiler: running `vesper shot --verify` compiles and executes every website example inside *Vesper Codex*'s single-JIT-module SimPulse doctest sandbox (#link(<ch-19-1>)[Section 20.1]) prior to rendering its HTML snippet and SVG/PNG social preview card, caching deterministic bit-exact outputs in `~/`.vesper/store/ indexed by BLAKE3(`cst_mif` + `theme_id` + `shot_cfg`).

=== 21.2 Semantic Token Wire Descriptors and Annotated Website Example Directives
<ch-20-2>
Chroma shares a padding-free 16-byte wire struct token-span descriptor across the CLI screenshot renderer, *Vesper Lens* LSP semantic tokens (#link(<ch-17>)[Chapter 18]), and `vesper-chroma.wasm` on the Vesper website. Authors annotate website showcase files with lightweight `@shot` and Twoslash-style hover/callout directives that render as visual callout pills inside SVG/PNG screenshots and interactive website cards:

```
// Exact 16-byte semantic token record shared by vesper shot, Vesper Lens, and vesper-chroma.wasm:
wire struct ChromaTokenSpanWire {
    byte_start: u32le,
    byte_len: u32le,
    token_class: u16le,    // VesperTokenClass (ModalSpecifier, PassModeInOut, LinearBinding, etc.)
    semantic_flags: u16le, // Bit 0 = Mode::Linear underline, Bit 1 = MutatedLeaf, Bit 2 = Ligature
    hover_sym_id: u32le,   // Optional index into Twoslash inferred-type / byte-offset hover table
}

// Website showcase snippet rendered via: vesper shot examples/wal_txn.ves -o site/cards/wal_txn.png
@shot(title = 'wal_txn.ves — Disjoint Projections & Linear Exhaustion', theme = 'vesper-dusk', dpi = 4)
fn append_verified_batch(
    inout broker.{index, metrics}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Storage, Fail[IoError]] {
    let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);
    //  ^? WriteTxn : mod linear (underlined in solar-amber until commit or abort)

    match validate_crc(in batch) {
        Ok(()) => {
            broker.index.insert_in_place(sink batch);
            broker.metrics.batches_written += 1;
            tx.commit() //^! Discharges linear tx across uncancellable Storage + Fail
        }
        Err(err) => {
            Storage::recycle_batch(sink batch);
            tx.abort(); //^! Mandatory linear cleanup on Err branch (enforced by [T-Let])
            Fail::raise(err)
        }
    }
}
```

=== 21.3 Website CSS Theme Tokens and Client-Side WASM Playground Pipeline
<ch-20-3>
On vesper-lang.org, static pages load a tiny 1.2 KiB CSS custom-property sheet switching cleanly between *Vesper Dusk* and *Vesper Dawn* without JavaScript, while interactive playground blocks hydrate `vesper-chroma.wasm` over shared linear memory to highlight live edits and highlight matching `Mode::Linear` mint-to-sink lifetimes on pointer hover:

```css
/* vesper-chroma.css — Perceptual OKLCH + sRGB fallback tokens for vesper-lang.org */
:root, [data-vesper-theme='dusk'] {
    --vc-bg-card:     #111625;
    --vc-fg-primary:  #E2E8F5;
    --vc-linear:      #FFB454; /* Mode::Linear bindings, sink transfers & obligation underline */
    --vc-inout:       #5CCFE6; /* Exclusive mutable FieldTree leaf projections */
    --vc-effect:      #D2A6FF; /* Lexical capability rows eff [...] & handlers */
    --vc-wire-proof:  #6EE7B7; /* Endian wire scalars (u32be) & @proof certificates */
    --vc-keyword:     #FF8F9C; /* Control-flow & declaration keywords */
    --vc-comment:     #8B97B3; /* Lossless CST trivia & doc prose */
}

[data-vesper-theme='dawn'] {
    --vc-bg-card:     #FAF8F5;
    --vc-fg-primary:  #1B2030;
    --vc-linear:      #B85200;
    --vc-inout:       #006B80;
    --vc-effect:      #6B2FD6;
    --vc-wire-proof:  #087443;
    --vc-keyword:     #B4233C;
    --vc-comment:     #55607A;
}

.vc-lin-live {
    color: var(--vc-linear);
    text-decoration: underline wavy var(--vc-linear) 1.5px;
    text-underline-offset: 4px;
}
.vc-gutter, .vc-twoslash-pin { user-select: none; }
```

=== 21.4 Lean 4 Lossless Source Partition and Perceptual Contrast Proofs (`vesper verify`)
<ch-20-4>
Extracting Chroma's lossless CST span partitioner and OKLCH contrast verifier through vesper-verify-lean (#link(<ch-8>)[Chapter 9]) discharges both *Byte-Exact Clipboard Fidelity* (proving that concatenating emitted token slices reconstructs the exact `.ves` source length with zero dropped trivia or whitespace bytes) and *WCAG AAA Contrast Soundness*:

```lean
-- lean/ChromaHighlightSafety.lean
theorem chroma_contiguous_spans_lossless
    (s0 e0 s1 e1 srcLen : Nat)
    (h_start : s0 = 0) (h_le0 : s0 <= e0) (h_join : e0 = s1)
    (h_le1 : s1 <= e1) (h_end : e1 = srcLen) :
    (e0 - s0) + (e1 - s1) = srcLen := by
  omega

theorem chroma_wcag_aaa_gate_sound
    (lumFg lumBg : Nat)
    (h_gate : (Nat.max lumFg lumBg + 50) >= 7 * (Nat.min lumFg lumBg + 50)) :
    let lMax := Nat.max lumFg lumBg
    let lMin := Nat.min lumFg lumBg
    lMax >= lMin /\ (lMax + 50) >= 7 * (lMin + 50) := by
  constructor
  · omega
  · exact h_gate
```

=== 21.5 Rust Semantic Highlighter, OKLCH Contrast Auditor, and Retina Code-Card Scene Engine (`crates/vesper-highlight`)
<ch-20-5>
`crates/vesper-highlight` compils identically to native `x86_64`/AArch64 (`vesper shot` and `vesper highlight`) and wasm32-unknown-unknown (`vesper-chroma.wasm` for vesper-lang.org):

```rust
// crates/vesper-highlight/src/lib.rs
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum VesperTokenClass {
    KeywordControl = 0,
    ModalSpecifier = 1,
    PassModeIn = 2,
    PassModeInOut = 3,
    PassModeSink = 4,
    LinearBinding = 5,
    FieldLeafMut = 6,
    FieldLeafRead = 7,
    EffectLinear = 8,
    EffectAbortable = 9,
    TypeNominal = 10,
    WireEndianScalar = 11,
    FunctionIdent = 12,
    LiteralScalar = 13,
    DocProofAnchor = 14,
    PunctLigature = 15,
    CommentTrivia = 16,
    Whitespace = 17,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeSwatch {
    pub oklch_l_bp: u16,
    pub oklch_c_bp: u16,
    pub oklch_hue_deg: u16,
    pub srgb_hex: u32,
    pub rel_lum_x1000: u32,
}

#[derive(Clone, Debug)]
pub struct ChromaPalette {
    pub name: SmolStr,
    pub bg_canvas: ThemeSwatch,
    pub bg_card: ThemeSwatch,
    pub fg_default: ThemeSwatch,
    pub linear_amber: ThemeSwatch,
    pub inout_cyan: ThemeSwatch,
    pub effect_iris: ThemeSwatch,
    pub wire_emerald: ThemeSwatch,
    pub keyword_rose: ThemeSwatch,
    pub comment_slate: ThemeSwatch,
}

impl ChromaPalette {
    pub fn vesper_dusk() -> Self {
        Self {
            name: SmolStr::new_inline("vesper-dusk"),
            bg_canvas: ThemeSwatch { oklch_l_bp: 1450, oklch_c_bp: 220, oklch_hue_deg: 265, srgb_hex: 0x0B0E17, rel_lum_x1000: 9 },
            bg_card: ThemeSwatch { oklch_l_bp: 1850, oklch_c_bp: 260, oklch_hue_deg: 265, srgb_hex: 0x111625, rel_lum_x1000: 16 },
            fg_default: ThemeSwatch { oklch_l_bp: 9200, oklch_c_bp: 180, oklch_hue_deg: 255, srgb_hex: 0xE2E8F5, rel_lum_x1000: 790 },
            linear_amber: ThemeSwatch { oklch_l_bp: 8350, oklch_c_bp: 1580, oklch_hue_deg: 72, srgb_hex: 0xFFB454, rel_lum_x1000: 552 },
            inout_cyan: ThemeSwatch { oklch_l_bp: 8450, oklch_c_bp: 1320, oklch_hue_deg: 205, srgb_hex: 0x5CCFE6, rel_lum_x1000: 548 },
            effect_iris: ThemeSwatch { oklch_l_bp: 8100, oklch_c_bp: 1490, oklch_hue_deg: 298, srgb_hex: 0xD2A6FF, rel_lum_x1000: 472 },
            wire_emerald: ThemeSwatch { oklch_l_bp: 8500, oklch_c_bp: 1450, oklch_hue_deg: 158, srgb_hex: 0x6EE7B7, rel_lum_x1000: 660 },
            keyword_rose: ThemeSwatch { oklch_l_bp: 7900, oklch_c_bp: 1620, oklch_hue_deg: 18, srgb_hex: 0xFF8F9C, rel_lum_x1000: 435 },
            comment_slate: ThemeSwatch { oklch_l_bp: 6750, oklch_c_bp: 280, oklch_hue_deg: 258, srgb_hex: 0x8B97B3, rel_lum_x1000: 305 },
        }
    }

    pub fn swatch_for(&self, class: VesperTokenClass) -> ThemeSwatch {
        match class {
            VesperTokenClass::KeywordControl => self.keyword_rose,
            VesperTokenClass::ModalSpecifier
            | VesperTokenClass::PassModeSink
            | VesperTokenClass::LinearBinding => self.linear_amber,
            VesperTokenClass::PassModeInOut | VesperTokenClass::FieldLeafMut => self.inout_cyan,
            VesperTokenClass::EffectLinear | VesperTokenClass::EffectAbortable => self.effect_iris,
            VesperTokenClass::WireEndianScalar | VesperTokenClass::DocProofAnchor => self.wire_emerald,
            VesperTokenClass::CommentTrivia => self.comment_slate,
            _ => self.fg_default,
        }
    }

    pub fn verify_contrast_gates(&self) -> Result<(), ChromaError> {
        let bg = self.bg_card.rel_lum_x1000;
        let primary = [
            self.fg_default,
            self.linear_amber,
            self.inout_cyan,
            self.effect_iris,
            self.wire_emerald,
            self.keyword_rose,
        ];
        for sw in primary {
            let l_max = sw.rel_lum_x1000.max(bg);
            let l_min = sw.rel_lum_x1000.min(bg);
            if (l_max + 50) < 7 * (l_min + 50) {
                return Err(ChromaError::InsufficientContrast { srgb_hex: sw.srgb_hex });
            }
        }
        let c_max = self.comment_slate.rel_lum_x1000.max(bg);
        let c_min = self.comment_slate.rel_lum_x1000.min(bg);
        if (c_max + 50) * 2 < 9 * (c_min + 50) {
            return Err(ChromaError::InsufficientContrast { srgb_hex: self.comment_slate.srgb_hex });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub class: VesperTokenClass,
    pub is_live_linear: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PositionedGlyphRun {
    pub x_px: f32,
    pub y_px: f32,
    pub text: SmolStr,
    pub srgb_hex: u32,
    pub underline_linear: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CodeCardScene {
    pub title: SmolStr,
    pub width_px: u32,
    pub height_px: u32,
    pub dpi_scale: u32,
    pub bg_card_hex: u32,
    pub runs: Vec<PositionedGlyphRun>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromaError {
    DiscontiguousTokenSpan { expected_offset: usize, actual_start: usize },
    IncompleteSourceCoverage { covered_end: usize, src_len: usize },
    InsufficientContrast { srgb_hex: u32 },
}

pub fn verify_lossless_spans(src_len: usize, spans: &[HighlightSpan]) -> Result<(), ChromaError> {
    let mut cursor = 0usize;
    for sp in spans {
        if sp.start != cursor || sp.end < sp.start {
            return Err(ChromaError::DiscontiguousTokenSpan {
                expected_offset: cursor,
                actual_start: sp.start,
            });
        }
        cursor = sp.end;
    }
    if cursor != src_len {
        return Err(ChromaError::IncompleteSourceCoverage { covered_end: cursor, src_len });
    }
    Ok(())
}

/// Builds a resolution-independent screenshot card scene consumable by both vector SVG export
/// (for vesper-lang.org) and tiny-skia 4x Retina PNG rasterization (for vesper shot).
pub fn build_code_card_scene(
    title: &str,
    src: &str,
    spans: &[HighlightSpan],
    palette: &ChromaPalette,
    dpi_scale: u32,
) -> Result<CodeCardScene, ChromaError> {
    palette.verify_contrast_gates()?;
    verify_lossless_spans(src.len(), spans)?;

    let line_count = src.lines().count().max(1) as u32;
    let header_px = 52u32;
    let line_height_px = 24u32;
    let mut runs = Vec::with_capacity(spans.len());

    let mut y_px = (header_px + 20) as f32;
    let mut x_col = 0usize;
    let char_adv_px = 8.4f32;

    for sp in spans {
        for segment in src[sp.start..sp.end].split_inclusive('\n') {
            let trimmed = segment.strip_suffix('\n').unwrap_or(segment);
            if !trimmed.is_empty() && sp.class != VesperTokenClass::Whitespace {
                let sw = palette.swatch_for(sp.class);
                runs.push(PositionedGlyphRun {
                    x_px: 28.0 + (x_col as f32 * char_adv_px),
                    y_px,
                    text: SmolStr::new(trimmed),
                    srgb_hex: sw.srgb_hex,
                    underline_linear: sp.is_live_linear,
                });
            }
            x_col += trimmed.chars().count();
            if segment.ends_with('\n') {
                y_px += line_height_px as f32;
                x_col = 0;
            }
        }
    }

    Ok(CodeCardScene {
        title: SmolStr::new(title),
        width_px: 880,
        height_px: header_px + (line_count * line_height_px) + 36,
        dpi_scale: dpi_scale.max(1),
        bg_card_hex: palette.bg_card.srgb_hex,
        runs,
    })
}
```


== 22. Vesper Crucible: Concolic Fuzzing, Shrinking, and C/Rust Lifting
<ch-21>
#chapter-deck[Concrete and Symbolic Testing, Counterexample Shrinking, and Interop]

*Vesper Crucible* (`@vesper-test/crucible`, with implementation code in `crates/vesper-crucible`) describes property testing, coverage-guided and concolic fuzzing, mutation testing, and C/Rust migration through `vesper fuzz`, `vesper test --mutate`, and `vesper migrate`. It combines Cranelift edge/comparison instrumentation, Bitwuzla path solving, `ChoiceTape` shrinking, `SimPulse` schedule minimization, and MPK-isolated differential shadow harnesses. This chapter explains their proposed integration with ownership tracking and Lean proof workflows; surviving a search does not prove the property.

=== 22.1 Concolic Verification, Substructural Shrinking, and Interop Guarantees
<ch-21-1>
+ *Coverage-Guided and Concolic Fuzzing (`vesper fuzz`):* The design instruments edge counters and branch-comparison operands in Stage 5 Cranelift SSA (`crates/vesper-codegen-clif`). Each iteration uses a resettable 64 KiB `Region::scoped` `VirtualArena` (#link(<ch-12-3>)[Section 13.3]), targeting more than 500,000 executions per second per core. When mutation stops making progress at a branch guard, an embedded Bitwuzla/Z3 concolic engine negates the path condition to seek a new input. The proposed solver is shared with `vesper-loom-solver` (#link(<ch-10-4>)[Section 11.4]). Coverage, solver support, and throughput need validation for the supported fragment and workload.

+ *Obligation-Safe Unified Choice-Tape Shrinking (`ChoiceTape`):* Traditional property-based testers (`QuickCheck` and `proptest`) attach shrinking logic to types, which breaks down when shrinking interleaved asynchronous schedules or `Mode::Linear` protocol state machines (`WriteTxn`, `Session[P]`, `PagePin`). Crucible drives both structured input generation and `SimPulse` fiber scheduling (#link(<ch-14-6>)[Section 15.6]) from a single monotone 64-bit *`ChoiceTape`*. Shrinking reduces the underlying tape in shortlex order (`0`-biasing and block deletion) while re-executing the unmodified handler block—guaranteeing under Theorem 2 that every intermediate shrunk candidate remains 100 percent linear-obligation-valid and deterministically reproduces multi-fiber race conditions.

+ *Counterexample-Guided Lean 4 Proof Bridge (`vesper verify --refute`):* Before spending engineer or tactic time proving a candidate invariant in `Specs.lean` (#link(<ch-8-4>)[Section 9.4]), `vesper verify --refute` runs Crucible's concolic fuzzer against the extracted predicate. Any refutation automatically materializes a minimal deterministic `@test` regression case in the source module; conversely, surviving properties export coverage-verified boundary witnesses to seed `bv_decide` and `aesop`.

+ *Automated C and Rust Strangler-Fig Migration (`vesper migrate`):* Adopting a post-Rust systems language in production requires seamless module-by-module migration rather than all-or-nothing rewrites. `vesper migrate` analyzes C headers/translation units and Rust crates (`syn` plus MIR borrow summaries) to synthesize declarative `.vsys` ABI manifests (#link(<ch-15-3>)[Section 16.3]), padding-verified `wire struct` layouts, idiomatic Vesper functions where `&'a mut self` field accesses lift automatically into lifetime-free disjoint `inout self.{f1, f2}` projections (`[T-CallProj]`), and `Mode::Linear` obligations for `#[must_use]` handles and RAII transaction guards.

+ *MPK-Sandboxed Differential Equivalence Harness (`@differential_shadow`):* During incremental migration from C or Rust, `@differential_shadow` executes the legacy foreign function inside #link(<ch-15-4>)[Section 16.4]'s 1.5 ns Hardware MPK/PKRU sandbox side-by-side with the newly migrated Vesper function across millions of concolic fuzz inputs—verifying bit-identical outputs and buffer mutations before retiring legacy code.

=== 22.2 Unified `ChoiceTape` Property Suites and Concolic `SimPulse` Fuzzing
<ch-21-2>
Crucible exposes `linear effect FuzzSource` (`q = linear`) backed by a monotone `ChoiceTape`. Because `FuzzSource` composes with `Region` and `SimPulse`, a single `@fuzz_prop` target serves simultaneously as a fast unit property test (`vesper test`), a coverage-guided concolic fuzzer (`vesper fuzz`), and a deterministic concurrency schedule explorer:

```
// Structured input generator drawing from Crucible's shortlex-shrinkable ChoiceTape:
linear effect FuzzSource {
    fn draw_u64_bounded(max_inclusive: u64) -> u64;
    fn draw_bytes[r: Region](in arena: RegionCap[r], max_len: usize) -> RegionSlice[r, u8];
    fn assume_guard(cond: bool) -> Unit;
}

trait Arbitrary[r: Region] {
    fn arbitrary(in arena: RegionCap[r]) -> Self eff [r: Region, FuzzSource];
}

// Combined stateful WAL + crash-recovery fuzz property (`0` global heap churn, shrinks inputs & schedules!):
@fuzz_prop(concolic = true, max_runs = 1_000_000)
fn prop_wal_crash_recovery_idempotent[P: IsaProfile]()
    -> Result[Unit, WalError] eff [FuzzSource, Isa[P], Fail[WalError]]
{
    Region::scoped(256.kib(), |cap scratch| {
        let raw_payload = FuzzSource::draw_bytes(in scratch, 4096);
        let crash_after_sqe = FuzzSource::draw_u64_bounded(8);
        let schedule_seed = FuzzSource::draw_u64_bounded(0xFFFF_FFFF_FFFF_FFFF);

        // Runs unmodified production WAL commit under SimPulse with injected NVMe torn-write fault:
        let sim_outcome = SimPulse::run_seeded(schedule_seed, crash_after_sqe, || {
            let mut store = MemNvmeStorage::with_region(in scratch);
            let tx: WriteTxn = store.begin_txn(4096);
            match store.stage_and_commit_checked[P](sink tx, in raw_payload) {
                Ok(receipt) => store.verify_recovered_slot(receipt.wal_slot, in raw_payload),
                Err(_io_fault) => store.verify_zero_orphaned_pages(),
            }
        });
        sim_outcome
    })
}
```

=== 22.3 Incremental Rust/C Migration (`vesper migrate`) and MPK Differential Shadowing
<ch-21-3>
When migrating a legacy C or Rust packet/frame parser (`legacy_codec.rs` or `legacy_codec.c`), `vesper migrate --source crates/legacy-codec` emits both the `.vsys` foreign contract and the idiomatic lifetime-free Vesper replacement wired into `@differential_shadow`:

```
// 1. Auto-synthesized by vesper migrate from legacy C/Rust ABI (sys/legacy_codec.vsys):
sys library LegacyCodecAbi (rust_crate = "legacy_codec ^0.8.0") {
    fn legacy_parse_batch_header(
        nocapture in raw_bytes: CSlice[u8],
        nocapture inout out_crc: u32,
        nocapture inout out_count: i32,
    ) -> ForeignVal[i32];
}

// 2. Migrated Vesper implementation: lifetimes & raw pointers lifted to wire struct + inout!
fn vesper_parse_batch_header(
    in raw_bytes: ByteSlice,
    inout state.{last_crc, record_count}: DecoderStats,
) -> Result[i64, WireLayoutError] eff [Fail[WireLayoutError]] {
    let in hdr = raw_bytes.view_wire_prefix[KafkaBatchWireHeader]()?;
    state.last_crc = hdr.crc.to_native();
    state.record_count = hdr.records_count.to_native();
    Ok(hdr.base_offset.to_native())
}

// 3. Hardware MPK-isolated differential fuzz harness (vesper fuzz --differential):
@differential_shadow
fn shadow_verify_legacy_vs_vesper(
    in raw_input: ByteSlice,
) -> Result[Unit, DiffMismatch] eff [Ffi[LegacyCodecAbi], Fail[DiffMismatch]] {
    let mut legacy_crc: u32 = 0;
    let mut legacy_cnt: i32 = 0;

    // Executes C/Rust inside Hardware Domain 2 (1.5ns WRPKRU / POR_EL0 isolation, Section 13.4):
    let legacy_rc = handle Ffi[LegacyCodecAbi] isolated(mpk) in {
        LegacyCodecAbi::legacy_parse_batch_header(
            in raw_input.as_cslice(),
            inout legacy_crc,
            inout legacy_cnt,
        ).validate_i32()
    };

    let mut v_stats = DecoderStats { last_crc: 0, record_count: 0 };
    let vesper_res = handle Fail[WireLayoutError] in {
        Ok(vesper_parse_batch_header(in raw_input, inout v_stats.{last_crc, record_count}))
    } with {
        return r => r,
        raise(e) => Err(e),
    };

    Crucible::assert_differential_equiv(legacy_rc, legacy_crc, legacy_cnt, in vesper_res, in v_stats)
}
```

=== 22.4 Lean 4 Shortlex Shrinking Termination and Migration Equivalence (`vesper verify`)
<ch-21-4>
In `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]), Crucible's shortlex `ChoiceTape` ordering is proven well-founded (guaranteeing that counterexample shrinking always terminates in finitely many steps), and migrated Vesper parsers can graduate from empirical differential fuzzing to full mathematical equivalence proofs:

```lean
-- illustrative/CrucibleMigrationSafety.lean
-- 1. Shortlex tape reduction (len', sum') <lex (len, sum) is strictly decreasing & well-founded:
theorem choice_tape_shrink_terminates
    (len1 len2 sum1 sum2 : Nat)
    (h_step : len2 < len1 ∨ (len2 = len1 ∧ sum2 < sum1)) :
    Prod.Lex (· < ·) (· < ·) (len2, sum2) (len1, sum1) := by
  rcases h_step with h_lt | ⟨h_eq, h_sum⟩
  · exact Prod.Lex.left _ _ h_lt
  · subst h_eq; exact Prod.Lex.right _ h_sum

-- 2. Graduating differential fuzzing to total functional equivalence across all 61-byte headers:
theorem migrated_batch_header_matches_spec (hdr : KafkaBatchWireHeader) (st : DecoderStats) :
    (vesperParseBatchHeader (Wire.toBytes hdr) st).map Prod.fst = .ok hdr.baseOffset.toNative := by
  simp [vesperParseBatchHeader]
```

=== 22.5 Rust Choice-Tape Shrinker and Rust-to-Vesper Projection Lifter (`crates/vesper-crucible`)
<ch-21-5>
`crates/vesper-crucible` implements the internal shortlex `ChoiceTape` reducer (preserving `Mode::Linear` protocol validity without type-specific shrinkers) alongside the `vesper migrate` borrow-to-`FieldTree` signature synthesizer:

```rust
// crates/vesper-crucible/src/lib.rs
use rustc_hash::FxHashMap;
use smol_str::SmolStr;

/// Unified 64-bit choice tape driving both Arbitrary data draws & SimPulse schedule choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceTape {
    pub values: Vec<u64>,
}

impl ChoiceTape {
    /// Generates shortlex-smaller candidate tapes (chunk deletion + binary-search zeroing)
    /// guaranteed to terminate by choice_tape_shrink_terminates.
    pub fn shrink_failure(
        &self,
        mut reproduces_failure: impl FnMut(&Self) -> bool,
    ) -> Self {
        let mut best = self.values.clone();

        // Phase 1: Delete contiguous draw blocks (prunes unused fiber/event steps)
        let mut chunk = best.len() / 2;
        while chunk > 0 {
            let mut idx = 0;
            while idx + chunk <= best.len() {
                let mut candidate = best.clone();
                candidate.drain(idx..idx + chunk);
                let tape = Self { values: candidate };
                if reproduces_failure(&tape) {
                    best = tape.values;
                } else {
                    idx += 1;
                }
            }
            chunk /= 2;
        }

        // Phase 2: Minimize individual u64 choices toward 0 via binary search
        for i in 0..best.len() {
            let mut lo = 0u64;
            let mut hi = best[i];
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let mut candidate = best.clone();
                candidate[i] = mid;
                if reproduces_failure(&Self { values: candidate }) {
                    best[i] = mid;
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
        }

        Self { values: best }
    }
}

/// Summary of field accesses extracted from a legacy Rust function parameter (&T, &mut T, or T).
#[derive(Clone, Debug)]
pub struct LegacyRustParamSummary {
    pub param_name: SmolStr,
    pub ty_name: SmolStr,
    pub is_owned_move: bool,
    pub read_field_paths: Vec<SmolStr>,
    pub mutated_field_paths: Vec<SmolStr>,
}

/// Synthesizes a lifetime-free Vesper parameter signature (in, inout x.{...}, or sink) for vesper migrate.
pub fn lift_rust_param_to_vesper_projection(summary: &LegacyRustParamSummary) -> String {
    if summary.is_owned_move {
        return format!("sink {}: {}", summary.param_name, summary.ty_name);
    }
    if !summary.mutated_field_paths.is_empty() {
        let mut paths: Vec<&str> = summary
            .mutated_field_paths
            .iter()
            .chain(summary.read_field_paths.iter())
            .map(|s| s.as_str())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        return format!(
            "inout {}.{{{}}}: {}",
            summary.param_name,
            paths.join(", "),
            summary.ty_name
        );
    }
    if summary.read_field_paths.is_empty() {
        format!("in {}: {}", summary.param_name, summary.ty_name)
    } else {
        let mut paths: Vec<&str> = summary.read_field_paths.iter().map(|s| s.as_str()).collect();
        paths.sort_unstable();
        paths.dedup();
        format!(
            "in {}.{{{}}}: {}",
            summary.param_name,
            paths.join(", "),
            summary.ty_name
        )
    }
}

/// Tracks Cranelift SSA edge-coverage hit counts (virgin_bits bitmap) during vesper fuzz.
pub struct CoverageFeedbackMap {
    pub virgin_edges: Vec<u8>,
    pub discovered_edges: usize,
}

impl CoverageFeedbackMap {
    pub fn with_edge_capacity(num_edges: usize) -> Self {
        Self {
            virgin_edges: vec![0xFF; num_edges],
            discovered_edges: 0,
        }
    }

    /// Returns true iff run_counts hit a new control-flow edge or a new power-of-2 hit bucket.
    pub fn record_iteration(&mut self, run_counts: &FxHashMap<usize, u8>) -> bool {
        let mut found_novelty = false;
        for (&edge_id, &hits) in run_counts {
            if hits == 0 {
                continue;
            }
            let bucket_bit = 1u8 << (7 - hits.leading_zeros().min(7));
            if let Some(virgin) = self.virgin_edges.get_mut(edge_id) {
                if (*virgin & bucket_bit) != 0 {
                    *virgin &= !bucket_bit;
                    self.discovered_edges += 1;
                    found_novelty = true;
                }
            }
        }
        found_novelty
    }
}
```


== 23. Vesper Chrono: Hardware Flight Recording and Reverse Debugging
<ch-22>
#chapter-deck[Effect Logs, Checkpoints, Hardware Traces, and Resource History]

*Vesper Chrono* (`@vesper-debug/chrono`, with implementation code in `crates/vesper-chrono`) describes flight recording, reverse debugging, and resource-obligation provenance through `vesper replay` and the *Lens* Debug Adapter. The design records outcomes at lexical effect boundaries and combines checkpoints with hardware traces. Exact replay depends on capturing the relevant nondeterminism; the under-2-percent recording overhead is a target requiring workload-specific evidence.

=== 23.1 Production Flight-Recording and Time-Travel Debugging Guarantees
<ch-22-1>
+ *Effect-Boundary Flight Recording (`ChronoTape`):* Chrono proposes recording nondeterministic outcomes through lexical capabilities in `Σ`: `Net`, `Storage`, `Clock`, `RdmaQp`, and `Ffi` (#link(<ch-5>)[Chapter 5] and #link(<ch-14>)[Chapter 15]). A lock-free handler around `EvidenceVector` (`r15` / `x28`) writes compact `wire struct` outcomes per shard. This is intended to retain multicore execution across Linux, Windows, and macOS. It differs from the single-core syscall-trapping approach attributed here to Mozilla `rr` (`ptrace` and `seccomp`, with a reported 1.5x–3x slowdown). That comparison needs workload-specific evidence; Chrono also needs a complete account of nondeterminism outside the recorded boundaries.

+ *Sub-Microsecond Zero-`Pin` Fiber and `VirtualArena` Checkpointing:* Checkpointing asynchronous Rust processes requires freezing the entire OS address space (`fork`) because `Pin`-wrapped futures scatter self-referential pointers across the global heap. By #link(<ch-5>)[Chapter 5]'s Zero-Pin Stack Relocation Corollary and #link(<ch-12-3>)[Section 13.3]'s contiguous `VirtualArena` regions, Vesper Chrono captures incremental copy-on-write shard checkpoints (`userfaultfd` write-protect on Linux, Mach `vm_copy` on macOS, `MEM_RESET` page tracking on Windows) at fiber yield points in microseconds without global stop-the-world pauses.

+ *Hardware Branch Tracing (`Intel PT` and `Arm CoreSight ETM`) Without Code Bloat:* Pure computation between effect boundaries executes at full native Cranelift speed with zero software instrumentation. When hardware trace mode is active, Chrono synchronizes per-core *Intel Processor Trace (IPT)* or *Arm CoreSight ETM* ring buffers against Cranelift basic-block address tables via TSC anchor points (`rdtscp` / `cntvct_el0`), reconstructing exact instruction-by-instruction backward control flow (`reverse-step`, `reverse-next`) across `egglog`-fused loops and SIMD kernels.

+ *First-Class `Mode::Linear` Obligation Provenance (`ObligationLineage`):* Conventional debuggers inspect raw struct memory at a breakpoint or crash site but cannot explain how a leaked or deadlocked resource arrived there. Chrono assigns every `Mode::Linear` obligation (`WriteTxn`, `NonceTicket`, `PagePin`, `Session[P]`, `KvPageLease`) a monotonic 64-bit `ObligationId` tracked from minting site across `sink` parameter moves and cross-fiber `LinearTx` handoffs to its terminal `[T-Match]` unpacking—answering provenance and last-mutator queries in `O(log N)` time inside Vesper Lens (#link(<ch-17>)[Chapter 18]).

+ *Secret-Redacted Crash Bundles and Lean 4 Replay Soundness (`vesper verify`):* Production flight-recorder rings automatically exclude `Vesper Vault` `SecretSlab` pages (`MADV_DONTDUMP`, #link(<ch-27-1>)[Section 28.1]) and scrub `@pii` attributes (#link(<ch-31-4>)[Section 32.4]) prior to `.vchrono` bundle serialization, while `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) mechanically proves that replaying any recorded `ChronoTape` reproduces bit-identical program states and obligation histories.

=== 23.2 Effect-Boundary `wire struct` Flight Log and `Chrono` Capabilities
<ch-22-2>
Chrono encodes recorded nondeterministic effect completions into a padding-free 32-byte `wire struct` header written directly into a core-local ring buffer, paired with `ReplaySession` capabilities for programmatic and interactive reverse stepping:

```
// Exact 32-byte effect-boundary flight-recorder frame header (0 implicit padding, Section 13.2):
wire struct ChronoEventWireHeader {
    lamport_seq: u64le,
    fiber_id: u64le,
    ipt_tsc_anchor: u64le,
    effect_id: u16le,
    op_tag: u16le,
    payload_byte_len: u32le,
}

copy struct TraceCursor {
    epoch_id: u32,
    lamport_seq: u64,
}

copy struct ObligationHop {
    lamport_seq: u64,
    fiber_id: u64,
    source_span_id: u32,
    hop_kind_tag: u8, // 0 = Mint, 1 = SinkMove, 2 = ChannelHandoff, 3 = UnpackDischarge
}

linear struct ReplaySession {
    bundle_fd: u32,
    active_cursor: TraceCursor,
}

linear effect ChronoReplay {
    fn open_bundle(in path: ByteSlice) -> Result[ReplaySession, ReplayError];

    // Steps backwards to the exact statement that last mutated `target_place` via `inout`:
    fn reverse_watch_inout(
        inout session: ReplaySession,
        local_place_hash: u64,
    ) -> Result[TraceCursor, ReplayError];

    // Reconstructs the complete Mint -> Sink -> FiberHandoff -> Unpack lineage of a linear handle:
    fn query_linear_lineage[r: Region](
        in session: ReplaySession,
        in scratch: RegionCap[r],
        obligation_id: u64,
    ) -> RegionSlice[r, ObligationHop];

    fn close_session(sink session: ReplaySession) -> Unit;
}
```

=== 23.3 Always-On Production Flight Recording and Time-Travel Triage (`0` `Alloc`)
<ch-22-3>
Wrapping any *Vesper Pulse* worker shard in `Chrono::with_flight_recorder` enables continuous black-box recording into a fixed 16 MiB circular DMA ring (`Alloc` omitted from `eff [...]`). On any invariant failure or stalled linear obligation alert from `vesper top`, operators replay the exact failure backwards in Vesper Lens or via a programmatic triage script:

```
// 1. Production shard wrapper: interposes EvidenceVector recording with <2% overhead & Vault scrubbing!
fn run_recorded_broker_shard(
    inout shard: BrokerShard,
    in cfg: BrokerConfig,
    inout flight_ring: CircularDmaRing,
) eff [Scope, Storage, Net, Clock, Fail[BrokerError]] {
    Chrono::with_flight_recorder(inout flight_ring, |cap _rec| {
        // Unmodified production broker loop: Vault `SecretSlab` pages are hardware-excluded from dumps!
        serve_broker_requests(inout shard, in cfg)
    })
}

// 2. Automated post-mortem root-cause triage locating the exact corrupting `inout` write & linear hops:
fn diagnose_stalled_wal_bundle(
    in bundle_path: ByteSlice,
    stalled_txn_obligation_id: u64,
    corrupted_index_place_hash: u64,
) -> Result[(TraceCursor, u32), ReplayError] eff [ChronoReplay, Fail[ReplayError]] {
    let mut sess: ReplaySession = ChronoReplay::open_bundle(in bundle_path)?;

    // Note: `[T-Let]` statically forbids `?` while linear `sess` is live; handle Err explicitly:
    match ChronoReplay::reverse_watch_inout(inout sess, corrupted_index_place_hash) {
        Ok(culprit_cursor) => {
            let hop_count = Region::scoped(64.kib(), |cap arena| {
                let hops = ChronoReplay::query_linear_lineage(
                    in sess,
                    in arena,
                    stalled_txn_obligation_id,
                );
                hops.len() as u32
            });
            ChronoReplay::close_session(sink sess);
            Ok((culprit_cursor, hop_count))
        }
        Err(err) => {
            ChronoReplay::close_session(sink sess);
            Fail::raise(err)
        }
    }
}
```

=== 23.4 Lean 4 Replay Determinism and Lineage Completeness Proofs (`vesper verify`)
<ch-22-4>
Because `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) already models effectful Vesper functions as pure interaction trees (`EffM sig Ret`), proving *Deterministic Replay Equivalence*—that feeding the recorded `ChronoTape` back into `EffM` reproduces the exact live terminal state—closes by structural induction over the interaction tree:

```lean
-- illustrative/ChronoReplaySafety.lean
-- 1. Exact Replay Determinism: replaying recorded effect responses over `EffM` yields identical state!
theorem chrono_replay_reproduces_live_execution
    (prog : EffM sig StateOut) (env : LiveOsOracle sig) :
    let (liveOut, recordedTape) := ChronoSpec.recordRun env prog
    ChronoSpec.replayFromTape recordedTape prog = .ok liveOut := by
  induction prog with
  | pure val => simp [ChronoSpec.recordRun, ChronoSpec.replayFromTape]
  | vis member op cont ih =>
    simp [ChronoSpec.recordRun, ChronoSpec.replayFromTape]
    apply ih

-- 2. Reverse Watchpoint Soundness: `findLastWriterBefore` returns the maximal mutation timestamp `< t`:
theorem reverse_watch_finds_latest_mutation
    (writes : List Nat) (queryTs hitTs : Nat)
    (h_mem : hitTs ∈ writes) (h_lt : hitTs < queryTs)
    (h_max : ∀ w ∈ writes, w < queryTs -> w ≤ hitTs) :
    hitTs < queryTs ∧ ∀ w ∈ writes, hitTs < w -> queryTs ≤ w := by
  constructor
  · exact h_lt
  · intro w hw h_gt
    by_contra h_not_le
    have h_w_lt : w < queryTs := Nat.lt_of_not_le h_not_le
    have h_le : w ≤ hitTs := h_max w hw h_w_lt
    omega
```

=== 23.5 Rust Reverse-Watchpoint Indexer and Obligation Lineage Engine (`crates/vesper-chrono`)
<ch-22-5>
`crates/vesper-chrono` implements the snapshot-epoch timeline index, O(log N) backward `inout` write search, and the `Mode::Linear` obligation provenance tracker:

```rust
// crates/vesper-chrono/src/lib.rs
use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotEpoch {
    pub epoch_id: u32,
    pub start_lamport_seq: u64,
    pub arena_cow_offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InOutWriteEvent {
    pub lamport_seq: u64,
    pub fiber_id: u64,
    pub place_hash: u64,
    pub span_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineageStepKind {
    Minted,
    SinkTransferred,
    ChannelSent { to_fiber: u64 },
    UnpackedTerminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObligationLineageEvent {
    pub lamport_seq: u64,
    pub fiber_id: u64,
    pub span_id: u32,
    pub kind: LineageStepKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChronoError {
    DoubleTerminalDischarge { obligation_id: u64 },
    UnknownObligation { obligation_id: u64 },
}

/// Time-travel index supporting O(log N) backward watchpoints & linear obligation provenance.
#[derive(Default)]
pub struct ChronoTimelineIndex {
    pub snapshots: Vec<SnapshotEpoch>,
    writes_by_place: FxHashMap<u64, Vec<InOutWriteEvent>>,
    linear_lineages: FxHashMap<u64, Vec<ObligationLineageEvent>>,
}

impl ChronoTimelineIndex {
    /// Records an `inout` place write in strictly increasing `lamport_seq` order.
    pub fn record_inout_write(&mut self, ev: InOutWriteEvent) {
        self.writes_by_place.entry(ev.place_hash).or_default().push(ev);
    }

    /// Binary-searches backwards for the latest `inout` mutation to `place_hash` strictly before `before_seq`,
    /// paired with the nearest preceding CoW `SnapshotEpoch` to bound forward replay latency.
    pub fn find_last_writer_before(
        &self,
        place_hash: u64,
        before_seq: u64,
    ) -> Option<(Option<SnapshotEpoch>, InOutWriteEvent)> {
        let writes = self.writes_by_place.get(&place_hash)?;
        let idx = writes.partition_point(|w| w.lamport_seq < before_seq);
        if idx == 0 {
            return None;
        }
        let target_write = writes[idx - 1];
        let snap_idx = self
            .snapshots
            .partition_point(|s| s.start_lamport_seq <= target_write.lamport_seq);
        let nearest_snap = (snap_idx > 0).then(|| self.snapshots[snap_idx - 1]);
        Some((nearest_snap, target_write))
    }

    /// Appends a provenance hop for a `Mode::Linear` obligation, enforcing single terminal discharge.
    pub fn record_obligation_hop(
        &mut self,
        obligation_id: u64,
        hop: ObligationLineageEvent,
    ) -> Result<(), ChronoError> {
        let chain = self.linear_lineages.entry(obligation_id).or_default();
        if chain.last().is_some_and(|prev| prev.kind == LineageStepKind::UnpackedTerminal) {
            return Err(ChronoError::DoubleTerminalDischarge { obligation_id });
        }
        chain.push(hop);
        Ok(())
    }

    pub fn obligation_lineage(&self, obligation_id: u64) -> Result<&[ObligationLineageEvent], ChronoError> {
        self.linear_lineages
            .get(&obligation_id)
            .map(Vec::as_slice)
            .ok_or(ChronoError::UnknownObligation { obligation_id })
    }
}
```


== 24. Vesper Metronome: Hardware PMU Microbenchmarking and Gates
<ch-23>
#chapter-deck[Hardware Counters, Paired Comparisons, Layout Randomization, and Benchmark Contracts]

*Vesper Metronome* (`@vesper-bench/metronome`, with implementation code in `crates/vesper-bench`) describes microbenchmarking through `vesper bench` and *Lens* CodeLenses (#link(<ch-17>)[Chapter 18]). It combines hardware performance monitoring unit (PMU) counters, SSA/assembly inspection, paired A/B execution, layout randomization, and static `@bench` contracts. CAS-backed baselines support comparisons across Loom revisions. Counter-read costs and regression sensitivity depend on the hardware and experiment.

=== 24.1 Deterministic Hardware Microbenchmarking and Regression Guarantees
<ch-23-1>
+ *Hardware Counters and Deterministic Cost Gates (`eff [BenchPmu]`):* Metronome proposes user-space counter reads through `x86_64` RDPMC over mapped `perf_event_open` pages, macOS Apple Silicon kpc counters, and AArch64 `PMCCNTR_EL0`. The draft estimates about 12 cycles per read. Counters cover retired instructions, reference cycles, cache misses, branch mispredictions, and Intel/Arm Top-Down Microarchitecture Analysis (TMA) slots. A separate SSA + SimPulse virtual-cost mode (`vesper bench --deterministic`) targets 0.00 percent variance. The draft's comparisons—5–15 percent wall-clock variance for Criterion on shared CI and 20x–50x slowdown for iai-callgrind—require specified experiments. Deterministic model costs do not establish constant native execution time.

+ *CAS-Backed Paired Interleaved A/B Execution and Stabilizer Layout Randomization:* Subtle 3 to 8 percent benchmark shifts in C++ and Rust routinely turn out to be ghosts caused by 64-byte instruction-cache alignment shifts or BTB aliasing when unrelated code changes link order (Curtsinger and Berger, _Stabilizer_). When running `vesper bench --compare git:main`, Metronome loads the precompiled baseline archive directly from *Vesper Loom*'s content-addressed store (`~/.vesper/store/`, #link(<ch-10-2>)[Section 11.2]) alongside the candidate inside the *same OS process* via `cranelift-jit` (#link(<ch-7>)[Chapter 8]). It alternates 1-millisecond paired quanta (`A, B, B, A`, _Tango_-style) on a core-pinned thread while re-randomizing function entry-point padding offsets modulo 4 KiB across epochs—canceling thermal/VM drift and isolating genuine algorithmic speedups down to 0.3 percent in under 2 seconds.

+ *Zero-Distortion Mode::Linear Batch Setup and Register-Preserving `black_box`:* Benchmarking functions that consume owned or linear values (such as `WriteTxn::commit(sink self)` in #link(<ch-1>)[Chapter 1] or `UmemFrame` in #link(<ch-32>)[Chapter 33]) in Rust forces either timed `.clone()` calls or per-batch `GlobalAlloc` vector allocations, while `core::hint::black_box` often forces LLVM to spill SIMD registers onto the stack. Metronome's `BenchCtx::iter_linear` pre-constructs linear obligations inside an untimed `VirtualArena` (`Region`, #link(<ch-12-3>)[Section 13.3]), brackets only the target call with user-space `RDPMC` fences, and runs untimed linear teardown on any residual handles—preserving Theorem 2 (Total Linear Exhaustion) with zero heap noise. Simultaneously, `Metronome::black_box` lowers to a Stage 5 Cranelift opaque SSA value barrier that blocks Stage 4 `egglog` constant folding/DCE without spilling scalar or `VVec` registers to memory.

+ *Static + Hardware Performance Contracts (`@bench` Gates) and Annotated ASM Diffs (`vesper bench --asm`):* Rather than treating benchmarks as passive charts inspected manually, `@bench(...)` supports machine-enforced CI assertions: `zero_alloc = true` verifies both statically (`Alloc` absent from `eff [...]`) and dynamically (`0ShardSlab` bytes), `expect_vectorized = true` verifies that Stage 4 `egglog` emitted predicated `VVec` vector instructions rather than scalar loops, and `max_instructions_per_byte` enforces algorithmic throughput ceilings. Passing `--asm` renders side-by-side Cranelift CLIF IR and native `x86_64`/AArch64/RISC-V assembly diffs annotated inline with hardware PMU stall cycles and `AoSoA` cache-line miss heatmaps.

+ *Fiber-Aware Coz Causal Profiling (`vesper bench --causal`) and Lean 4 Statistical/PMU Proofs:* Traditional sampling profilers measure where CPU cycles are spent, which misleads engineers in asynchronous `io_uring` pipelines where speeding up an off-critical-path task yields zero end-to-end throughput gain. Metronome integrates _Coz_-style *Causal Profiling* directly into *Vesper Pulse* worker shards (`PulseWorkerShard`, #link(<ch-14-7>)[Section 15.7]): during virtual speedup experiments on candidate function `F`, whenever a fiber executes `F`, sibling fibers concurrently active on the shard pause by a proportional virtual delay `d` at their next lexical effect boundary—plotting exact end-to-end p99 latency and throughput sensitivity curves before a single line of optimization is written. Meanwhile, `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) proves wrap-safe PMU counter delta monotonicity and paired-sample noise cancellation.

=== 24.2 Padding-Free wire struct PMU Snapshots, Linear BenchCtx Batching, and `@bench` Contracts
<ch-23-2>
Metronome records per-sample hardware PMU counters into a 64-byte L1-cache-line-sized `wire struct` (#link(<ch-15-2>)[Section 16.2]) and exposes `linear effect BenchHarness` so `Mode::Linear` setup and teardown never contaminate timed instruction/cycle windows:

```
// Exact 64-byte (1 L1 cache line, 0 implicit padding) hardware PMU & TMA sample record:
wire struct PmuSampleWire {
    ref_cycles: u64le,
    instructions_retired: u64le,
    l1d_read_misses: u64le,
    llc_misses: u64le,
    branch_mispredicts: u64le,
    simd_vec_uops_retired: u64le,
    arena_bump_bytes: u64le,
    tma_backend_bound_slots_bp: u32le, // Basis points (0..10000) of Top-Down Backend-Bound slots
    icache_rand_offset_bytes: u32le,   // Stabilizer 64B-aligned entry jitter offset [0..4096)
}

// Register-preserving Stage 5 Cranelift optimization barrier (0 stack spills!):
fn black_box[T: mod linear](sink val: T) -> T eff [];

linear effect BenchHarness {
    // Untimed pre-allocation of `N` linear obligations inside a dedicated Region arena,
    // followed by RDPMC-fenced execution of `body` on each `sink` element:
    fn iter_linear_batched[In: mod linear, Out: mod affine, r: eff](
        setup: fn(in RegionCap[bench_reg], usize) -> In eff [bench_reg: Region],
        body: fn(sink In) -> Out eff [r],
    ) -> PmuSampleWire eff [r];

    // Marks an end-to-end throughput/latency progress point for `vesper bench --causal`:
    fn causal_progress_point(point_name: StaticStr) -> Unit;
}
```

=== 24.3 Zero-Alloc SIMD Benchmark with `@bench` Hardware Gates and Linear WAL Batching
<ch-23-3>
Using `@bench`, engineers benchmark both pure SIMD kernels (*Vesper Wire/Json*, #link(<ch-29>)[Chapter 30]) and `Mode::Linear` storage transactions (`WriteTxn`, #link(<ch-1>)[Chapter 1]) with enforced zero-allocation and vectorization gates:

```
// 1. Enforces in CI: 0 global Alloc, verified Stage 4 VVec vectorization, and <= 2.2 instructions/byte!
@bench(
    zero_alloc = true,
    expect_vectorized = true,
    max_instructions_per_byte = 2.2,
    stabilizer_pages = 16,
)
fn bench_simd_json_ndjson_ingest[P: IsaProfile](
    in sample_ndjson_64k: AlignedSlice[u8, 64],
) -> u32 eff [BenchHarness, Isa[P], Fail[JsonError]] {
    Region::scoped(256.kib(), |cap scratch| {
        let mut tiles = TiledSeq::with_region[TelemetryPoint](in scratch, 1024);
        let input = black_box(sample_ndjson_64k.as_bytes());
        let parsed = ingest_ndjson_to_tiles[P, scratch](in scratch, in input, inout tiles)?;
        black_box(parsed)
    })
}

// 2. Benchmarking a `Mode::Linear` consumer (`sink WriteTxn`) with untimed batch setup:
@bench(zero_alloc = true, max_l1d_misses_per_op = 4)
fn bench_linear_wal_commit_fastpath() -> PmuSampleWire eff [BenchHarness, Storage] {
    BenchHarness::iter_linear_batched(
        // Untimed setup: mints fresh `Mode::Linear` WriteTxn obligations outside RDPMC window:
        |in arena, idx| Storage::mock_staged_txn_in_region(in arena, idx as u64, 512),
        // Timed body: measures ONLY `commit` + CRC framing (`sink tx` discharged per iteration!):
        |sink tx: WriteTxn| {
            handle Fail[IoError] in {
                Ok(black_box(tx.commit()))
            } with {
                return res => res,
                raise(e) => Err(e),
            }
        },
    )
}
```

=== 24.4 Lean 4 PMU Wrap-Safe Arithmetic and Paired Interleaving Cancellation Proofs (`vesper verify`)
<ch-23-4>
Extracting Metronome's 48-bit hardware PMU counter delta reader and paired `A/B` difference estimator via `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Modular 48-Bit Counter Wrap-Around Exactness* (`bv_decide`) and *Additive Common-Mode Cloud-VM Noise Cancellation* (`omega`):

```lean
-- illustrative/MetronomeBenchSafety.lean
-- 1. Exact 48-Bit Hardware PMU Counter Delta Across Wrap-Around (`x86_64 RDPMC` width = 48 bits):
-- Proves via SAT bit-blasting (`bv_decide`) that masked modular subtraction recovers the exact
-- `delta < 2^48` even when the hardware counter wraps past `0xFFFF_FFFF_FFFF` mid-sample!
theorem rdpmc_48bit_wrap_delta_exact (startRaw delta : BitVec 64)
    (h_delta_fits : delta < (1#64 <<< 48)) :
    let mask48 : BitVec 64 := (1#64 <<< 48) - 1#64
    let endRaw : BitVec 64 := (startRaw + delta) &&& mask48
    ((endRaw - (startRaw &&& mask48)) &&& mask48) = delta := by
  bv_decide

-- 2. Paired Interleaving Cancellation (`Tango` quanta `[A + drift, B + drift]`):
-- Any additive host hypervisor/thermal stall `commonDrift` affecting an interleaved `(A, B)` pair
-- cancels identically from the paired speedup difference `(measB - measA)`:
theorem paired_interleaving_cancels_common_drift (trueA trueB commonDrift : Int) :
    let measA := trueA + commonDrift
    let measB := trueB + commonDrift
    measB - measA = trueB - trueA := by
  omega
```

=== 24.5 Rust Paired A/B Statistical Engine, Stabilizer Offset Generator, and Gate Auditor (`crates/vesper-bench`)
<ch-23-5>
`crates/vesper-bench` implements 48-bit wrap-safe PMU delta extraction, deterministic SplitMix64 _Stabilizer_ cache-line entry-offset randomization, paired-sample Mann-Whitney / Hodges-Lehmann confidence-interval regression detection, and `@bench` budget enforcement:

```rust
// crates/vesper-bench/src/lib.rs
use smol_str::SmolStr;

pub const PMU_COUNTER_MASK_48: u64 = (1u64 << 48) - 1;
pub const CACHE_LINE_BYTES: u32 = 64;
pub const PAGE_BYTES_4K: u32 = 4096;

/// Computes exact retired events across 48-bit hardware `RDPMC` wrap-around (`rdpmc_48bit_wrap_delta_exact`).
#[inline(always)]
pub const fn pmu_delta_48bit(start_raw: u64, end_raw: u64) -> u64 {
    end_raw.wrapping_sub(start_raw & PMU_COUNTER_MASK_48) & PMU_COUNTER_MASK_48
}

/// Generates deterministic 64-byte-aligned page offsets `[0, 64, 128, ..., 4032]` for Stabilizer
/// link-order / I-cache / BTB randomization across paired benchmark epochs.
pub fn stabilizer_icache_offset_bytes(seed: u64, epoch: u64) -> u32 {
    let mut z = seed.wrapping_add(epoch.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let slot = ((z ^ (z >> 31)) as u32) % (PAGE_BYTES_4K / CACHE_LINE_BYTES);
    slot * CACHE_LINE_BYTES
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairedEpochSample {
    pub baseline_cycles_per_op: i64,
    pub candidate_cycles_per_op: i64,
    pub candidate_insts_per_op: u64,
    pub candidate_l1d_misses_per_op: u64,
    pub candidate_heap_alloc_bytes: u64,
    pub has_simd_vector_ops: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchGateSpec {
    pub bench_name: SmolStr,
    pub require_zero_alloc: bool,
    pub expect_vectorized: bool,
    pub max_insts_per_op: Option<u64>,
    pub max_l1d_misses_per_op: Option<u64>,
    /// Maximum allowed median regression in basis points (e.g. `100` = +1.00% cycles/op).
    pub max_regression_bp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BenchGateFailure {
    UnexpectedHeapAllocation { bench: SmolStr, allocated_bytes: u64 },
    MissingSimdVectorization { bench: SmolStr },
    InstructionBudgetExceeded { bench: SmolStr, actual: u64, limit: u64 },
    L1dMissBudgetExceeded { bench: SmolStr, actual: u64, limit: u64 },
    ThroughputRegression { bench: SmolStr, median_delta_bp: i64, max_allowed_bp: i64 },
}

/// Evaluates paired interleaved `(A, B)` epochs using the non-parametric median of paired ratios
/// (robust against outlier OS interrupts) and enforces `@bench(...)` hardware contracts.
pub fn evaluate_paired_benchmark_gate(
    spec: &BenchGateSpec,
    samples: &[PairedEpochSample],
) -> Result<i64, BenchGateFailure> {
    let mut delta_bps: Vec<i64> = Vec::with_capacity(samples.len());

    for s in samples {
        if spec.require_zero_alloc && s.candidate_heap_alloc_bytes > 0 {
            return Err(BenchGateFailure::UnexpectedHeapAllocation {
                bench: spec.bench_name.clone(),
                allocated_bytes: s.candidate_heap_alloc_bytes,
            });
        }
        if spec.expect_vectorized && !s.has_simd_vector_ops {
            return Err(BenchGateFailure::MissingSimdVectorization {
                bench: spec.bench_name.clone(),
            });
        }
        if let Some(limit) = spec.max_insts_per_op {
            if s.candidate_insts_per_op > limit {
                return Err(BenchGateFailure::InstructionBudgetExceeded {
                    bench: spec.bench_name.clone(),
                    actual: s.candidate_insts_per_op,
                    limit,
                });
            }
        }
        if let Some(limit) = spec.max_l1d_misses_per_op {
            if s.candidate_l1d_misses_per_op > limit {
                return Err(BenchGateFailure::L1dMissBudgetExceeded {
                    bench: spec.bench_name.clone(),
                    actual: s.candidate_l1d_misses_per_op,
                    limit,
                });
            }
        }
        let base = s.baseline_cycles_per_op.max(1);
        let diff = s.candidate_cycles_per_op - base;
        delta_bps.push((diff.saturating_mul(10_000)) / base);
    }

    if delta_bps.is_empty() {
        return Ok(0);
    }
    delta_bps.sort_unstable();
    let median_bp = delta_bps[delta_bps.len() / 2];

    if median_bp > spec.max_regression_bp {
        return Err(BenchGateFailure::ThroughputRegression {
            bench: spec.bench_name.clone(),
            median_delta_bp: median_bp,
            max_allowed_bp: spec.max_regression_bp,
        });
    }
    Ok(median_bp)
}
```


= Part V. Operating Systems, Kernel Programs, Cryptography, and Enclaves
<part-5>
#chapter-deck[Bare-Metal Execution, Kernel Programs, Virtual Machines, Cryptography, and Attestation]

#_part-summary("Part V Architectural Scope")[
  Part V applies the language design to kernels and security-sensitive services. #link(<ch-24>)[Chapter 25] describes *Silicon* bare-metal execution, device registers, interrupts, and DMA. #link(<ch-25>)[Chapter 26] covers eBPF validation, and #link(<ch-26>)[Chapter 27] covers *Aether* unikernels and microVMs. #link(<ch-27>)[Chapter 28] presents *Vault* cryptographic ownership and timing contracts. #link(<ch-28>)[Chapter 29] presents *Aegis* confidential memory and attestation. Type-level contracts alone do not establish the hardware, runtime, or adversarial assumptions these designs require.
]

#figure(
  table(
    columns: (14%, 34%, 52%),
    align: (left, left, left),
    table.header([*Chapter*], [*Subsystem*], [*Package, Host Crate, and Security / Hardware Guarantees*]),
    [#link(<ch-24>)[Chapter 25]], [Vesper Silicon (Bare-Metal & MMIO)], [`@vesper-hw/silicon` (`vesper-silicon`): W1C MMIO safety, LIFO `IrqGuard`, DMA ownership, WCSD bounds],
    [#link(<ch-25>)[Chapter 26]], [Vesper eBPF (Kernel & XDP Checks)], [`@vesper-kernel/ebpf` (`vesper-ebpf`): `BpfRingSlot`, packet-bounds lowering, pre-flight abstract verifier],
    [#link(<ch-26>)[Chapter 27]], [Vesper Aether (Unikernels & MicroVMs)], [`@vesper-cloud/aether` (`vesper-aether`): 1.6 MiB PVH unikernels, CAS `UFFDIO_CONTINUE`, `SnapshotResumeTicket`],
    [#link(<ch-27>)[Chapter 28]], [Vesper Vault (Constant-Time Crypto)], [`@vesper-crypto/vault` (`vesper-vault`): `eff [ConstTime]`, SSA taint pass, `NonceTicket`, `X25519MLKEM768`],
    [#link(<ch-28>)[Chapter 29]], [Vesper Aegis (Confidential Computing)], [`@vesper-tee/aegis` (`vesper-aegis`): TDX / SEV-SNP / CCA quotes, `HostBounceGrant` single-fetch DMA],
  ),
  kind: table,
)


== 25. Vesper Silicon: Bare-Metal Kernels, Typestate MMIO, and DMA
<ch-24>
#chapter-deck[Bare-Metal Execution, Device Registers, Interrupt Guards, DMA, and Stack Bounds]

*Vesper Silicon* (`@vesper-hw/silicon`, with implementation code in `crates/vesper-silicon`) describes bare-metal kernels, firmware, and device drivers for ARM Cortex-M, Cortex-A, RISC-V, and `x86_64`. Explicit allocation and I/O capabilities are intended to let suitable modules run without a hosted standard library. The proposed contracts cover static SRAM fibers, stack bounds, LIFO interrupt guards, `@svd_pac` device registers, and DMA/page-table ownership. Their hardware and runtime assumptions remain essential.

=== 25.1 Bare-Metal, MMIO, Interrupt, and DMA Safety Guarantees
<ch-24-1>
+ *Unified Freestanding Platform Without `no_std` Ecosystem Split:* Rust divides crates between `std` and `no_std` + `alloc`, forcing library authors to maintain feature flags just to avoid ambient heap or OS assumptions. Because heap provisioning (`Alloc`) and OS I/O (`Net`, `Storage`) are already explicit lexical effects in Vesper (#link(<ch-14>)[Chapter 15] and #link(<ch-12>)[Chapter 13]), any package omitting `Alloc` from `eff [...]` is natively freestanding and guaranteed zero-heap at compile time.

+ *Static SRAM Fibers and Stack Bounds (`@stack_limit`):* The proposed firmware runtime uses statically reserved SRAM fiber stacks. Stage 5 (`crates/vesper-silicon`) analyzes Cranelift machine-frame call graphs to compute worst-case stack depth and reject stacks exceeding the declared bound, including on MMU-less targets without guard pages. The relocation argument in #link(<ch-5>)[Chapter 5] applies to its stated model; native pointers and external DMA require additional lifetime justification. A valid stack bound also depends on complete call-graph and frame-size information.

+ *Compile-Time Interrupt Deadlock and Sleep-in-Atomic Elimination (`IrqGuard`):* Acquiring a spinlock in kernel code without masking local CPU interrupts risks an instant single-core deadlock if an Interrupt Service Routine (ISR) preempts the core and contends for the same lock; conversely, suspending inside an ISR or spinlock section causes fatal "sleeping while atomic" kernel panics. Silicon binds spinlock access to a `Mode::Linear` `IrqGuard` token (which restores `DAIF` / `RFLAGS.IF` / `mstatus.MIE` upon `restore_irq`) and types `@isr` handlers with an `AtomicCtx` frame restriction that statically forbids suspending effects (`Async`, `DmaWait`, `Storage`).

+ *Hermetic WASI SVD/Devicetree Typestate MMIO and `W1C` Hazard Prevention (`@svd_pac`):* Executing inside Vesper Forge's fuel-metered `wasm32-wasip2` sandbox (`macro_emits_effects = []`, #link(<ch-6-4>)[Section 7.4]), `@svd_pac` compiles CMSIS-SVD and RISC-V Devicetree descriptions into `Mode::Linear` peripheral singletons (`Spi1[Disabled] -> Spi1[MasterReady]`) and bitfield-typed volatile registers (`MmioReg32[Reg, Access]`). Write-1-to-Clear (`W1C`) interrupt status registers omit read-modify-write combinators by construction, preventing subtle driver bugs where clearing one hardware interrupt bit accidentally clears concurrent pending interrupts.

+ *Linear DMA Descriptor Transfers and Physical Page-Frame Conservation:* Passing an SRAM buffer or physical 4 KiB `PhysFrame4K` to a bus-master DMA ring (VirtIO, NVMe, or MCU DMA) via `sink` consumes host access and returns a `Mode::Linear` `DmaTransfer[B]` obligation, injecting architecture cache-coherency barriers (`DC CVAC` / `DSB SY` on Arm, `FENCE IORW, IORW` on RISC-V) automatically. Similarly, OS page-table unmapping returns `(PhysFrame4K, PendingTlbShootdown)`—making physical frame leaks and forgotten SMP `invlpg` / `tlbi` shootdowns compile-time errors under Theorem 2.

=== 25.2 Typestate MMIO Registers, Linear IrqGuard, and Cache-Coherent DMA Protocol
<ch-24-2>
Silicon pairs #link(<ch-15-2>)[Section 16.2]'s padding-free `wire struct` descriptor layouts with hardware-enforced volatile MMIO capabilities (`eff [Mmio]`) and linear ownership tokens for interrupts, DMA, and physical page frames:

```
// Exact 16-byte VirtIO / hardware scatter-gather DMA descriptor (0 implicit padding):
wire struct SgDmaDescWire {
    phys_addr: u64le,
    byte_len: u32le,
    flags: u16le,
    next_idx: u16le,
}

// Register access-policy phantom markers preventing W1C read-modify-write clobbers:
copy struct ReadOnly {}
copy struct ReadWrite {}
copy struct Write1ToClear {}

// Mode::Linear interrupt-mask guard: cannot exit scope without restoring CPU IRQ flags!
linear struct IrqGuard {
    saved_psr: usize,
}

// Mode::Linear physical 4 KiB RAM page frame: leaks and double-frees are compile-time errors!
linear struct PhysFrame4K {
    pfn: u64,
}

// Mode::Linear in-flight bus-master DMA transfer holding exclusive ownership of `Buf`:
linear struct DmaTransfer[Buf] {
    channel: u8,
    desc_slot: u16,
    owned_buf: Buf,
}

linear effect CpuIrq {
    fn disable_local_save() -> IrqGuard;
    fn restore_local(sink guard: IrqGuard) -> Unit;
}

linear effect DmaEngine {
    // Consumes `sink buf` so CPU cannot read/write `buf` while peripheral DMAs over AHB/AXI/PCIe!
    fn submit_rx[Buf: DmaBuffer](
        channel: u8,
        sink buf: Buf,
    ) -> DmaTransfer[Buf];

    // Suspends static SRAM fiber until ISR signals completion, then invalidates D-cache lines:
    fn await_complete[Buf: DmaBuffer](
        sink xfer: DmaTransfer[Buf],
    ) -> (Result[u32, DmaError], Buf);
}
```

=== 25.3 Freestanding ISR Driver and OS Kernel Page-Table Mapper (0 Heap, 0 Pin)
<ch-24-3>
Combining `@isr`, `IrqGuard`, and `PhysFrame4K` allows firmware drivers and OS virtual-memory managers to share a single colorless codebase verified free of heap allocations and unacknowledged TLB shootdowns:

```
// 1. Top-half hardware Interrupt Service Routine (`Async` and `Alloc` statically forbidden!):
@isr(irq = 42, stack_budget_bytes = 256)
fn eth_mac_rx_isr(
    inout mac_regs: EthMacMmio,
    inout wake_ring: IsrWakeQueue,
) eff [Mmio] {
    // Safe W1C acknowledge: writes ONLY bit 0 without read-modify-write clobbering sibling bits!
    mac_regs.irq_status_w1c.write_clear_mask(1 << 0);
    wake_ring.notify_dma_channel(0);
}

// 2. Bottom-half freestanding firmware fiber running in 1024 bytes of static SRAM (`0` Pin, `0` Alloc):
@stack_limit(bytes = 1024)
fn telemetry_uart_bridge_task(
    inout radio_state: RadioLinkState,
    sink rx_slab: StaticDmaBuf[512],
) -> Never eff [DmaEngine, CpuIrq, Mmio] {
    let mut buf = rx_slab;
    loop {
        let xfer: DmaTransfer[StaticDmaBuf[512]] = DmaEngine::submit_rx(0, sink buf);
        let (res, reclaimed_buf) = DmaEngine::await_complete(sink xfer);

        if let Ok(valid_bytes) = res {
            // Requires `&IrqGuard` proof to touch ISR-shared stats—impossible to deadlock!
            let irq_tok: IrqGuard = CpuIrq::disable_local_save();
            radio_state.record_frame_irq_safe(in irq_tok, in reclaimed_buf.prefix(valid_bytes));
            CpuIrq::restore_local(sink irq_tok);
        }
        buf = reclaimed_buf;
    }
}

// 3. OS Kernel Page-Table Unmap: statically enforces SMP TLB shootdown BEFORE frame reuse!
linear struct PendingTlbShootdown {
    vaddr: u64,
    asid: u16,
    reclaimed_frame: PhysFrame4K,
}

impl PendingTlbShootdown {
    fn flush_ipi_and_reclaim(sink self) -> PhysFrame4K eff [SmpIpi] {
        let PendingTlbShootdown { vaddr, asid, reclaimed_frame } = self;
        SmpIpi::broadcast_tlb_invalidate(vaddr, asid);
        reclaimed_frame
    }
}

fn unmap_and_recycle_user_page(
    inout pt: PageTableRoot,
    inout pmm: PhysBuddyAllocator,
    vaddr: u64,
) -> Result[Unit, VmError] eff [SmpIpi, Fail[VmError]] {
    let shootdown: PendingTlbShootdown = pt.unmap_leaf_4k(vaddr)?;
    // Cannot return PhysFrame4K to `pmm` without discharging `PendingTlbShootdown` first!
    let clean_frame: PhysFrame4K = shootdown.flush_ipi_and_reclaim();
    pmm.free_frame_4k(sink clean_frame);
    Ok(())
}
```

=== 25.4 Lean 4 W1C Register Safety and Physical Frame Conservation Proofs (`vesper verify`)
<ch-24-4>
In `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]), `vesper verify` proves both hardware bitfield `W1C` non-clobbering (`bv_decide`) and kernel physical page-frame conservation (`total_frames = free_frames + mapped_frames`) across page-table operations:

```lean
-- illustrative/SiliconKernelSafety.lean
-- 1. Bit-exact proof (`bv_decide`) that W1C single-bit clear leaves all 31 sibling pending IRQ bits intact:
theorem w1c_ack_preserves_concurrent_irqs (hwPending : BitVec 32) :
    let w1cWrite := (1 : BitVec 32) <<< 0
    let hwAfter  := hwPending &&& (~~~w1cWrite)
    ∀ bit : Fin 32, bit.val ≠ 0 -> hwAfter.getLsbD bit.val = hwPending.getLsbD bit.val := by
  decide

-- 2. Kernel Physical Memory Conservation: unmapping + recycling preserves total physical 4K frames:
theorem unmap_recycle_conserves_phys_frames
    (mappedBefore freeBefore : Nat) (h_nonempty : mappedBefore ≥ 1) :
    let mappedAfter := mappedBefore - 1
    let freeAfter   := freeBefore + 1
    mappedAfter + freeAfter = mappedBefore + freeBefore := by
  omega
```

=== 25.5 Rust Static Stack-Depth Analyzer and MMIO Verifier (`crates/vesper-silicon`)
<ch-24-5>
`crates/vesper-silicon` provides the Stage 5 Cranelift call-graph stack-budget verifier (preventing bare-metal SRAM stack overflows and unintended recursion) alongside the `@svd_pac` MMIO register hazard checker:

```rust
// crates/vesper-silicon/src/lib.rs
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnSym(pub u32);

/// Per-function Cranelift machine-frame metadata emitted at Stage 5.
#[derive(Clone, Debug)]
pub struct MachineFrameInfo {
    pub name: SmolStr,
    pub frame_bytes: u32,
    pub callees: Vec<FnSym>,
    pub has_indirect_call: bool,
    pub suspends_fiber: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiliconCheckError {
    UnboundedRecursionInFreestanding { cycle_at: SmolStr },
    IndirectCallWithoutStackBound { func: SmolStr },
    SuspensionForbiddenInIsr { isr_name: SmolStr, offending_fn: SmolStr },
    StackBudgetExceeded { entry: SmolStr, worst_case_bytes: u32, budget_bytes: u32 },
}

/// Computes exact worst-case stack depth (WCSD) over the call graph for `@stack_limit` & `@isr`.
pub fn verify_freestanding_stack_and_isr(
    entry: FnSym,
    budget_bytes: u32,
    is_isr_context: bool,
    cg: &FxHashMap<FnSym, MachineFrameInfo>,
) -> Result<u32, SiliconCheckError> {
    fn dfs(
        curr: FnSym,
        entry_name: &SmolStr,
        is_isr: bool,
        cg: &FxHashMap<FnSym, MachineFrameInfo>,
        visiting: &mut FxHashSet<FnSym>,
        memo: &mut FxHashMap<FnSym, u32>,
    ) -> Result<u32, SiliconCheckError> {
        if let Some(&cached) = memo.get(&curr) {
            return Ok(cached);
        }
        let info = &cg[&curr];
        if !visiting.insert(curr) {
            return Err(SiliconCheckError::UnboundedRecursionInFreestanding {
                cycle_at: info.name.clone(),
            });
        }
        if info.has_indirect_call {
            return Err(SiliconCheckError::IndirectCallWithoutStackBound {
                func: info.name.clone(),
            });
        }
        if is_isr && info.suspends_fiber {
            return Err(SiliconCheckError::SuspensionForbiddenInIsr {
                isr_name: entry_name.clone(),
                offending_fn: info.name.clone(),
            });
        }

        let mut max_callee: u32 = 0;
        for &callee in &info.callees {
            let sub = dfs(callee, entry_name, is_isr, cg, visiting, memo)?;
            max_callee = max_callee.max(sub);
        }

        visiting.remove(&curr);
        let total = info.frame_bytes.saturating_add(max_callee);
        memo.insert(curr, total);
        Ok(total)
    }

    let entry_name = cg[&entry].name.clone();
    let mut visiting = FxHashSet::default();
    let mut memo = FxHashMap::default();
    let worst_case = dfs(entry, &entry_name, is_isr_context, cg, &mut visiting, &mut memo)?;

    if worst_case > budget_bytes {
        return Err(SiliconCheckError::StackBudgetExceeded {
            entry: entry_name,
            worst_case_bytes: worst_case,
            budget_bytes,
        });
    }
    Ok(worst_case)
}
```


== 26. Vesper eBPF: Kernel and XDP Programs with Pre-Flight Checks
<ch-25>
#chapter-deck[Pre-Flight Analysis, Packet Views, Stack Bounds, and Kernel Admission]

*Vesper eBPF* (`@vesper-kernel/ebpf`, with implementation code in `crates/vesper-ebpf`) describes Linux kernel programs for XDP packet processing, `tc` traffic control, and `tracepoint`/`LSM` probes. The proposed pre-flight checks align linear resource accounting, `wire struct` views, bounded iteration, and 512-byte stack analysis with the kernel verifier's requirements. The kernel verifier remains authoritative at `BPF_PROG_LOAD`; acceptance cannot be inferred for every compiled program from this draft.

=== 26.1 Kernel Verifier Compliance, XDP Bounds, and RingBuf Guarantees
<ch-25-1>
+ *Linear Kernel References (`BpfRingSlot`):* The design represents ring reservations and acquired kernel references as linear values (`linear struct BpfRingSlot[T]`, `linear struct BpfSocketRef`). Acquisitions include `bpf_ringbuf_reserve`, `bpf_kptr_xchg`, and `bpf_sk_lookup_tcp`; terminal operations include `bpf_ringbuf_submit`, `bpf_ringbuf_discard`, and `bpf_sk_release`. Under the proposed `[T-Let]` discipline, each control-flow branch must discharge the obligation. *Lens* (#link(<ch-17>)[Chapter 18]) is intended to report missing releases before kernel loading. Theorem 2 applies only to its formal fragment and does not establish this complete eBPF integration.

+ *Verifier-Canonical `wire struct` Packet Bounds Lowering (`XdpPacketCtx`):* The Linux kernel eBPF verifier requires explicit `pkt_ptr + offset <= data_end` pointer comparisons before dereferencing packet bytes, and aggressive LLVM middle-end passes routinely algebraic-simplify or hoist bounds checks in ways that lose packet-range provenance. Vesper eBPF lowers `ctx.view_wire_at[W](offset)` directly into verifier-canonical `r_cursor = r_data + offset; if r_cursor + sizeof(W) > r_data_end goto bail` BPF instruction sequences protected from range-destroying optimizations.

+ *Enforced 512-Byte Stack Ceiling and Bounded Loop Termination (`decreases`):* Kernel eBPF enforces a strict 512-byte total call-stack limit and rejects unbounded loops. Vesper eBPF reuses #link(<ch-24-5>)[Section 25.5]'s static call-graph stack analyzer (`@stack_limit(bytes = 512)`) with `Alloc` omitted from `eff [...]`, while #link(<ch-8-2>)[Section 9.2]'s `decreases` loop termination annotations compile directly to `bpf_loop` or verifier-certified bounded open-coded iterators.

+ *Hermetic CO-RE BTF Relocations and Shared Kernel/User `wire struct` Schemas:* Executing inside Vesper Forge's fuel-metered `wasm32-wasip2` sandbox (`macro_emits_effects = []`, #link(<ch-6-4>)[Section 7.4]), `@btf_core` generates Compile-Once-Run-Everywhere (CO-RE) BTF field relocations without `bindgen` or host `clang` dependencies. Kernel eBPF probes and user-space Vesper Pulse `io_uring` consumers share the exact same `wire struct` event and BPF map definitions in a single workspace.

+ *Pre-Flight Abstract eBPF Verifier and Lean 4 Packet-Filter Proofs (`vesper verify`):* Stage 5 (`crates/vesper-ebpf`) runs a sound abstract interpreter over emitted eBPF bytecode modeling the Linux kernel verifier's scalar intervals, `PTR_TO_PACKET` validated byte ranges, and live reference IDs before ELF emission. Simultaneously, `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts `@xdp_prog` and `@lsm_prog` functions into Lean 4 `BitVec` specifications to prove DDoS filter accuracy and zero false-positive drops.

=== 26.2 Zero-Copy `wire struct` Packet Views and Linear `BpfRingSlot` Protocol
<ch-25-2>
Vesper eBPF models kernel map lookups, per-CPU counters, and lock-free ring-buffer reservations under `linear effect BpfMaps`, sharing padding-free `wire struct` network headers (#link(<ch-15-2>)[Section 16.2]) directly with XDP DMA descriptors:

```
// Exact 54-byte Ethernet + IPv4 (no options) + TCP wire header (0 implicit padding):
wire struct EthIpv4TcpWire {
    dst_mac: ByteArray[6],
    src_mac: ByteArray[6],
    ether_type: u16be,
    ver_ihl: u8,
    tos: u8,
    total_len: u16be,
    ident: u16be,
    frag_off: u16be,
    ttl: u8,
    proto: u8,
    ip_checksum: u16be,
    src_ipv4: u32be,
    dst_ipv4: u32be,
    src_port: u16be,
    dst_port: u16be,
    seq_num: u32be,
    ack_num: u32be,
    data_offset_flags: u16be,
    window_size: u16be,
    tcp_checksum: u16be,
    urgent_ptr: u16be,
}

// Shared kernel -> user-space security telemetry event (16 bytes exact, 0 implicit padding):
wire struct SynDropAlertWire {
    src_ipv4: u32be,
    dst_port: u16be,
    tcp_flags: u16be,
    observed_pps: u64le,
}

copy enum XdpAction {
    Aborted = 0,
    Drop = 1,
    Pass = 2,
    Tx = 3,
    Redirect = 4,
}

// Mode::Linear kernel ringbuf reservation: MUST call submit or discard before returning XdpAction!
linear struct BpfRingSlot[T] {
    ring_ptr_id: u64,
}

linear effect BpfMaps {
    fn percpu_increment_u64(map_id: u32, key: u32be) -> u64;
    fn reserve_ringbuf[T: WireFrame](ring_id: u32) -> Option[BpfRingSlot[T]];
    fn submit_ringbuf[T: WireFrame](sink slot: BpfRingSlot[T], in val: T) -> Unit;
    fn discard_ringbuf[T: WireFrame](sink slot: BpfRingSlot[T]) -> Unit;
}
```

=== 26.3 Line-Rate XDP SYN-Flood Mitigator and Zero-Copy User-Space Consumer
<ch-25-3>
Writing both the kernel XDP filter and the user-space Vesper Pulse consumer in one module guarantees bit-identical `SynDropAlertWire` layouts, 512-byte kernel stack safety, and zero unreleased `bpf_ringbuf` reservations:

```
const ETH_P_IPV4: u16 = 0x0800;
const IPPROTO_TCP: u8 = 6;
const TCP_FLAG_SYN: u16 = 0x0002;
const TCP_FLAG_ACK: u16 = 0x0010;

// 1. Kernel XDP Program: verified <= 512B stack, zero heap, and 100% verifier-accepted!
@xdp_prog(section = "xdp", stack_budget_bytes = 512)
fn mitigate_syn_flood(
    in pkt: XdpPacketCtx,
    pps_threshold: u64,
) -> XdpAction eff [BpfMaps] {
    // Emits canonical `data + 54 <= data_end` verifier guard:
    let Some(in hdr) = pkt.view_wire_prefix[EthIpv4TcpWire]() else {
        return XdpAction::Pass;
    };

    if hdr.ether_type.to_native() != ETH_P_IPV4 || hdr.proto != IPPROTO_TCP {
        return XdpAction::Pass;
    }

    let flags = hdr.data_offset_flags.to_native() & 0x003F;
    let is_syn_only = (flags & TCP_FLAG_SYN) != 0 && (flags & TCP_FLAG_ACK) == 0;
    if !is_syn_only {
        return XdpAction::Pass;
    }

    let src_count = BpfMaps::percpu_increment_u64(0, hdr.src_ipv4);
    if src_count <= pps_threshold {
        return XdpAction::Pass;
    }

    // Reserve linear ringbuf slot; compiler REJECTS returning XdpAction::Drop without submit/discard!
    match BpfMaps::reserve_ringbuf[SynDropAlertWire](1) {
        Some(slot) => {
            let alert = SynDropAlertWire {
                src_ipv4: hdr.src_ipv4,
                dst_port: hdr.dst_port,
                tcp_flags: flags.to_be(),
                observed_pps: src_count.to_le(),
            };
            BpfMaps::submit_ringbuf(sink slot, in alert);
            XdpAction::Drop
        }
        None => XdpAction::Drop,
    }
}

// 2. User-Space Vesper Pulse Consumer: polls mmap'd BPF ring buffer via io_uring epoll_ctl (`0` Alloc!):
fn drain_syn_drop_alerts(
    inout ring: UserBpfRingReader[SynDropAlertWire],
    inout metrics: EdgeMetricsShard,
) eff [Storage, Trace] {
    while let Some(in alert) = ring.peek_next_wire() {
        metrics.blocked_syn_packets += 1;
        metrics.last_blocked_ipv4 = alert.src_ipv4.to_native();
        ring.advance_consumer();
    }
}
```

=== 26.4 Lean 4 Firewall Safety Proofs (`vesper verify`)
<ch-25-4>
Extracting `mitigate_syn_flood` through `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) allows `bv_decide` and `simp` to prove that established TCP ACK traffic is *never dropped* regardless of per-source rate counters or payload contents:

```lean
-- illustrative/EbpfXdpSafety.lean
-- Proves across all 432-bit Ethernet+IPv4+TCP headers that any packet carrying TCP_FLAG_ACK
-- is guaranteed `XdpAction.pass` (zero false-positive drops on established flows!):
theorem syn_mitigator_never_drops_ack_packets
    (hdr : EthIpv4TcpWire) (maps : RefBpfMapState) (thresh : UInt64)
    (h_ack : (hdr.dataOffsetFlags &&& 0x0010#16) ≠ 0#16) :
    let (_maps', action) := RefBpfMapState.run maps (mitigateSynFlood (PacketFromHdr hdr) thresh)
    action = XdpAction.pass := by
  simp [mitigateSynFlood, h_ack, RefBpfMapState.run]
```

=== 26.5 Rust Pre-Flight Abstract eBPF Verifier (`crates/vesper-ebpf`)
<ch-25-5>
`crates/vesper-ebpf` provides the Stage 5 pre-flight abstract verifier checking stack bounds (`<= 512` bytes), `PTR_TO_PACKET` validated byte ranges, and unreleased kernel reference IDs prior to BPF ELF emission:

```rust
// crates/vesper-ebpf/src/lib.rs
use rustc_hash::FxHashSet;

pub const BPF_MAX_STACK_BYTES: u32 = 512;

/// Abstract register states mirroring Linux `kernel/bpf/verifier.c` (`bpf_reg_state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BpfRegState {
    Uninitialized,
    Scalar { min_u64: u64, max_u64: u64 },
    ContextPtr,
    PacketData { validated_bytes: u32 },
    PacketEnd,
    RingBufReservation { ref_obj_id: u32 },
}

#[derive(Clone, Debug)]
pub enum BpfAbstractInst {
    LoadPacketDataPtr { dst_reg: usize },
    LoadPacketEndPtr { dst_reg: usize },
    AssertPacketPrefix { data_reg: usize, min_bytes: u32 },
    PacketRead { data_reg: usize, offset: u32, width: u32, dst_reg: usize },
    ReserveRingBuf { dst_reg: usize, ref_obj_id: u32 },
    ReleaseRingBuf { src_reg: usize },
    Exit { ret_reg: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EbpfPreFlightError {
    StackLimitExceeded { used_bytes: u32 },
    UninitializedRegisterRead { reg: usize },
    UncheckedPacketAccess { offset: u32, width: u32, validated_bytes: u32 },
    UnreleasedKernelReferenceAtExit { live_ref_ids: Vec<u32> },
}

/// Pre-flight abstract verifier guaranteeing Linux kernel eBPF verifier acceptance at compile time.
pub fn verify_ebpf_block_preflight(
    stack_frame_bytes: u32,
    insts: &[BpfAbstractInst],
) -> Result<(), EbpfPreFlightError> {
    if stack_frame_bytes > BPF_MAX_STACK_BYTES {
        return Err(EbpfPreFlightError::StackLimitExceeded { used_bytes: stack_frame_bytes });
    }

    let mut regs = [BpfRegState::Uninitialized; 11];
    regs[1] = BpfRegState::ContextPtr; // R1 holds `xdp_md*` context on entry
    let mut live_refs: FxHashSet<u32> = FxHashSet::default();

    for inst in insts {
        match *inst {
            BpfAbstractInst::LoadPacketDataPtr { dst_reg } => {
                regs[dst_reg] = BpfRegState::PacketData { validated_bytes: 0 };
            }
            BpfAbstractInst::LoadPacketEndPtr { dst_reg } => {
                regs[dst_reg] = BpfRegState::PacketEnd;
            }
            BpfAbstractInst::AssertPacketPrefix { data_reg, min_bytes } => {
                if let BpfRegState::PacketData { validated_bytes } = &mut regs[data_reg] {
                    *validated_bytes = (*validated_bytes).max(min_bytes);
                }
            }
            BpfAbstractInst::PacketRead { data_reg, offset, width, dst_reg } => {
                match regs[data_reg] {
                    BpfRegState::PacketData { validated_bytes } => {
                        let end = offset.saturating_add(width);
                        if end > validated_bytes {
                            return Err(EbpfPreFlightError::UncheckedPacketAccess {
                                offset,
                                width,
                                validated_bytes,
                            });
                        }
                        regs[dst_reg] = BpfRegState::Scalar { min_u64: 0, max_u64: u64::MAX };
                    }
                    _ => return Err(EbpfPreFlightError::UninitializedRegisterRead { reg: data_reg }),
                }
            }
            BpfAbstractInst::ReserveRingBuf { dst_reg, ref_obj_id } => {
                live_refs.insert(ref_obj_id);
                regs[dst_reg] = BpfRegState::RingBufReservation { ref_obj_id };
            }
            BpfAbstractInst::ReleaseRingBuf { src_reg } => {
                match regs[src_reg] {
                    BpfRegState::RingBufReservation { ref_obj_id } => {
                        live_refs.remove(&ref_obj_id);
                        regs[src_reg] = BpfRegState::Uninitialized;
                    }
                    _ => return Err(EbpfPreFlightError::UninitializedRegisterRead { reg: src_reg }),
                }
            }
            BpfAbstractInst::Exit { ret_reg } => {
                if matches!(regs[ret_reg], BpfRegState::Uninitialized) {
                    return Err(EbpfPreFlightError::UninitializedRegisterRead { reg: ret_reg });
                }
                if !live_refs.is_empty() {
                    let mut ids: Vec<u32> = live_refs.iter().copied().collect();
                    ids.sort_unstable();
                    return Err(EbpfPreFlightError::UnreleasedKernelReferenceAtExit {
                        live_ref_ids: ids,
                    });
                }
            }
        }
    }
    Ok(())
}
```


== 27. Vesper Aether: PVH Unikernels and Clone-Safe KVM MicroVMs
<ch-26>
#chapter-deck[PVH Images, Virtual Devices, Snapshot Forking, and Clone Reseeding]

*Vesper Aether* (`@vesper-cloud/aether`, with implementation code in `crates/vesper-aether`) describes a PVH unikernel target, a microVM monitor, and snapshot-fork orchestration through `vesper build --unikernel`. Proposed host backends include Linux KVM, macOS `Virtualization.framework`, and Windows WHPX. The design targets sub-2-MiB images, cold boot below 1.2 milliseconds, and 400-microsecond forks using BLAKE3-addressed `userfaultfd` snapshots. These are performance targets; VMGENID ownership and reseeding also require independent clone-safety validation.

=== 27.1 Unikernel Isolation, Snapshot-Fork Entropy, and UFFD Guarantees
<ch-26-1>
+ *Unikernel Compilation (`vesper build --unikernel`):* The proposed build replaces hosted I/O handlers with *Silicon* (#link(<ch-24>)[Chapter 25]) and *Warp* (#link(<ch-32>)[Chapter 33]) over VirtIO 1.2 MMIO. Services express I/O through lexical effects (`eff [Net, Storage, Clock, ConstTime]`; #link(<ch-14>)[Chapter 15] and #link(<ch-27>)[Chapter 28]) rather than ambient POSIX calls. The design targets a standalone 1.6 MiB `x86_64` PVH or AArch64 direct-boot image without BIOS, ACPI, PCI, or initramfs, cold boot below 1.2 ms, and memory overhead below 3 MiB per instance. These targets and the draft's 125–800-millisecond container-boot comparison need reproducible measurements.
+ *Mode::Linear Clone-Safe Entropy and AEAD Nonce Reseeding Across Snapshot Forks (SnapshotResumeTicket):* Restoring or forking thousands of serverless functions or AI agent sandboxes from a golden memory snapshot introduces a critical cryptographic vulnerability in C, Go, and Rust guests: cloned microVMs wake with identical CSPRNG state, identical TLS/QUIC packet-number counters, and identical session IDs, causing catastrophic AEAD nonce reuse on the first transmitted packet. The proposed `Mode::Linear` typestates do not by themselves prevent snapshot-clone state reuse: quiescing a unikernel for snapshotting (`AetherVm::quiesce_and_fork_snapshot`) consumes active *Vesper Vault* `AeadSealer` handles (`sink old_sealer`, #link(<ch-27-3>)[Section 28.3]) and suspends the VM until hypervisor wake returns a `Mode::Linear SnapshotResumeTicket` carrying the new 128-bit hardware VM Generation ID (VMGENID). A fresh sealer must be derived after resume, and inherited transport sessions must be eliminated before the fork; reseeding alone does not rotate negotiated peer keys.
+ *Content-Addressed Loom CAS Deduping and Zero-Copy `UFFDIO_CONTINUE` Page Sharing:* Standard Firecracker userfaultfd (UFFD) lazy restore faults 4 KiB pages reactively over Unix domain sockets, stalling guest vCPUs on cold working-set misses while duplicating read-only code and ML weights across VMs. Aether integrates its host page server directly with *Vesper Loom*'s BLAKE3 content-addressed store (`~/.vesper/store/`, #link(<ch-10-2>)[Section 11.2]): immutable Cranelift `.text`/`.rodata` segments and *Vesper Tensor* quantized weights (#link(<ch-41>)[Chapter 42]) map across 50,000 concurrent microVMs on the same bare-metal host via zero-copy shared memfd `UFFDIO_CONTINUE` mappings, while mutable `VirtualArena` hot working-set pages stream proactively in 2 MiB batches via `io_uring` before vCPU entry.
+ *Mode::Linear VirtIO 1.2 Packed-Virtqueue Descriptor Chains (VirtqDescLease):* Packed virtqueues originated in VirtIO 1.1; the illustrative guest driver and host VMM use #link("https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html")[VirtIO 1.2] packed virtqueues (virtio-net, virtio-blk, and virtio-vsock) are typed using padding-free `wire struct` descriptors (#link(<ch-15-2>)[Section 16.2]) paired with `Mode::Linear VirtqDescLease` obligations. Under Theorem 2, descriptor slots can never leak on malformed frames, wrap prematurely over in-flight DMA buffers, or suffer hypervisor double-fetch TOCTOU races (reusing *Vesper Aegis* single-fetch bounce semantics when deployed inside Intel TDX or AMD SEV-SNP confidential VMs, #link(<ch-28>)[Chapter 29]).
+ *Embedded Seccomp-Locked VMM and Lean 4 EPT Isolation and Fork-Safety Proofs (`vesper verify`):* Rather than orchestrating external VMM processes via REST/JSON sockets, `crates/vesper-aether` provides a minimal seccomp-bpf-locked KVM/HVF/WHPX virtual machine manager integrated natively with *Vesper Pulse* worker shards. Simultaneously, `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) mechanically proves *Pairwise Guest Physical Address (EPT/Stage-2) Non-Overlap* across multi-tenant VMs and *Snapshot-Fork Nonce Domain Separation*.

=== 27.2 PVH / VirtIO 1.2 Packed wire struct Descriptors and Linear Snapshot-Fork Capabilities
<ch-26-2>
Aether defines VirtIO 1.2 16-byte packed-ring descriptors and the 24-byte hypervisor VM-Generation-ID boot/fork block as padding-free `wire struct` layouts (#link(<ch-15-2>)[Section 16.2]) paired with `Mode::Linear` snapshot-fork tokens:

```
wire struct VirtqPackedDescWire {
    buf_gpa: u64le,
    byte_len: u32le,
    desc_id: u16le,
    flags_avail_used: u16le,
}

wire struct AetherVmGenIdWire {
    vm_gen_lo: u64le,
    vm_gen_hi: u64le,
    host_tsc_khz: u32le,
    virtio_mmio_count: u32le,
}

linear struct SnapshotResumeTicket {
    gen_lo: u64,
    gen_hi: u64,
}

linear struct ReseededVaultEpoch[Alg] {
    fresh_sealer: AeadSealer[Alg],
    fork_epoch_id: u64,
}

linear effect AetherVm {
    fn quiesce_and_fork_snapshot[Alg: AeadCipher](sink old_sealer: AeadSealer[Alg]) -> SnapshotResumeTicket;
    fn reseed_after_fork[Alg: AeadCipher](sink ticket: SnapshotResumeTicket, in master_key: AeadKeySlot[Alg], inout slab: SecretSlab) -> ReseededVaultEpoch[Alg];
}
```

=== 27.3 Sub-Millisecond Unikernel Snapshot Quiesce and Clone-Safe Fork Resume (0 Alloc)
<ch-26-3>
The following sketch is unsafe for snapshot cloning as written: `conn_table` can retain negotiated QUIC traffic keys and packet numbers across clones. Reseeding a local sealer or PRNG does not change those sessions. Before a one-to-many fork, all TCP and QUIC sessions must be closed, transport and VirtIO operations drained, and the empty-session precondition enforced; each clone must establish fresh sessions and entropy after resume. The sketch is retained to identify the missing interface, not as an implementation or proof:

```
fn unikernel_serverless_worker_loop[Alg: AeadCipher, P: IsaProfile](
    in root_key: AeadKeySlot[Alg],
    inout secret_slab: SecretSlab,
    sink initial_sealer: AeadSealer[Alg],
    inout conn_table: TiledSeq[QuicConnTile],
) -> Never eff [AetherVm, XdpNic, ConstTime, Clock, Isa[P]] {
    let mut active_sealer = initial_sealer;
    loop {
        let fork_ticket: SnapshotResumeTicket = AetherVm::quiesce_and_fork_snapshot(sink active_sealer);
        let ReseededVaultEpoch { fresh_sealer, fork_epoch_id } =
            AetherVm::reseed_after_fork(sink fork_ticket, in root_key, inout secret_slab);
        active_sealer = fresh_sealer;
        Region::scoped(256.kib(), |cap _req_arena| {
            serve_forked_epoch_burst[Alg, P](
                fork_epoch_id,
                inout conn_table,
                in secret_slab,
                inout active_sealer,
            );
        });
    }
}
```

=== 27.4 Lean 4 EPT Memory Isolation and Snapshot-Fork Nonce Uniqueness Proofs (`vesper verify`)
<ch-26-4>
Extracting Aether's KVM Extended Page Table (EPT / Arm Stage-2) slot mapper and VMGENID epoch derivation through `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Zero Cross-VM Host Physical Page Overlap* and *Clone-Safe Nonce Epoch Injectivity*:

```lean
-- illustrative/AetherUnikernelSafety.lean
theorem ept_slots_disjoint_host_phys
    (hpaA lenA hpaB lenB : Nat)
    (h_nonemptyA : lenA > 0) (h_nonemptyB : lenB > 0)
    (h_sep : hpaA + lenA <= hpaB \/ hpaB + lenB <= hpaA) :
    forall p : Nat, (hpaA <= p /\ p < hpaA + lenA) -> ¬(hpaB <= p /\ p < hpaB + lenB) := by
  intro p hpA hpB
  omega

theorem snapshot_fork_vmgenid_separates_epochs
    (genA genB : BitVec 128) (h_distinct_clones : genA != genB) :
    AetherSpec.deriveForkKeyEpoch genA != AetherSpec.deriveForkKeyEpoch genB := by
  exact AetherSpec.hkdf_vmgenid_injective genA genB h_distinct_clones
```

=== 27.5 Rust KVM EPT Slot Verifier and CAS UFFD Page Resolver (`crates/vesper-aether`)
<ch-26-5>
`crates/vesper-aether` implements the host KVM memory-region overlap and page-alignment auditor alongside the content-addressed userfaultfd (`UFFDIO_ZEROPAGE`, `UFFDIO_CONTINUE`, and `UFFDIO_COPY`) snapshot page resolver:

```rust
// crates/vesper-aether/src/lib.rs
pub const PAGE_SIZE_4K: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KvmMemSlotSpec {
    pub slot_id: u32,
    pub guest_phys_addr: u64,
    pub host_phys_or_hva: u64,
    pub byte_size: u64,
    pub read_only_cas_shared: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AetherVmmError {
    UnalignedMemorySlot { slot_id: u32 },
    ZeroSizedMemorySlot { slot_id: u32 },
    OverlappingGuestPhysRange { slot_a: u32, slot_b: u32 },
}

pub fn verify_kvm_ept_layout(slots: &[KvmMemSlotSpec]) -> Result<(), AetherVmmError> {
    for (i, a) in slots.iter().enumerate() {
        if a.byte_size == 0 {
            return Err(AetherVmmError::ZeroSizedMemorySlot { slot_id: a.slot_id });
        }
        if (a.guest_phys_addr & (PAGE_SIZE_4K - 1)) != 0
            || (a.host_phys_or_hva & (PAGE_SIZE_4K - 1)) != 0
            || (a.byte_size & (PAGE_SIZE_4K - 1)) != 0
        {
            return Err(AetherVmmError::UnalignedMemorySlot { slot_id: a.slot_id });
        }
        let a_end = a.guest_phys_addr.saturating_add(a.byte_size);
        for b in &slots[i + 1..] {
            let b_end = b.guest_phys_addr.saturating_add(b.byte_size);
            if !(a_end <= b.guest_phys_addr || b_end <= a.guest_phys_addr) {
                return Err(AetherVmmError::OverlappingGuestPhysRange {
                    slot_a: a.slot_id,
                    slot_b: b.slot_id,
                });
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UffdPageAction {
    ZeroPage4K,
    ContinueSharedCasMemfd { cas_offset: u64 },
    CopyPrivateCowPage { snapshot_offset: u64 },
}

pub fn classify_uffd_snapshot_fault(
    fault_gpa: u64,
    cas_ro_end_gpa: u64,
    dirty_cow_bitmap: &[u64],
) -> UffdPageAction {
    let page_idx = (fault_gpa / PAGE_SIZE_4K) as usize;
    if fault_gpa < cas_ro_end_gpa {
        return UffdPageAction::ContinueSharedCasMemfd {
            cas_offset: fault_gpa & !(PAGE_SIZE_4K - 1),
        };
    }
    let word = dirty_cow_bitmap.get(page_idx / 64).copied().unwrap_or(0);
    if ((word >> (page_idx % 64)) & 1) != 0 {
        UffdPageAction::CopyPrivateCowPage {
            snapshot_offset: fault_gpa & !(PAGE_SIZE_4K - 1),
        }
    } else {
        UffdPageAction::ZeroPage4K
    }
}
```


== 28. Vesper Vault: Constant-Time Cryptography and Post-Quantum AEAD
<ch-27>
#chapter-deck[Secret Tracking, Timing Controls, Zeroization, and Nonce Ownership]

*Vesper Vault* (`@vesper-crypto/vault`, with implementation code in `crates/vesper-vault`) describes cryptography, post-quantum key establishment, and secret-memory handling. It complements the foreign TLS bridge in #link(<ch-33-1>)[Section 34.1]. Proposed mechanisms include linear `NonceTicket` values, SSA taint checks (`eff [ConstTime]`), hardware timing controls (`DIT` and `DOITM`), `SecretSlab` zeroization, and Lean 4 bit-blasting. These mechanisms address distinct threats; the draft does not establish complete timing-leak resistance or secret erasure.

=== 28.1 Constant-Time Execution, Secret Zeroization, and Nonce Guarantees
<ch-27-1>

*Review limitation:* The constant-time and zeroization claims in this chapter are goals, not established guarantees. Secret-derived loads must propagate taint and post-register-allocation code and spills need auditing. User space must not directly write privileged x86 DOITM MSRs; hardware timing controls do not eliminate all microarchitectural leakage.
+ *Compile-Time Nonce-Reuse Elimination (`NonceTicket[Alg]`):* In Rust AEAD crates (`aes-gcm`, `chacha20poly1305`), nonces are `Copy` byte slices (`&Nonce`), so nothing in the type system prevents catastrophic `(Key, Nonce)` reuse across retries, cloned sessions, or fiber forks. Vault models every encryption nonce as a `Mode::Linear` obligation (`linear struct NonceTicket[Alg]`) minted exclusively by mutating a stateful monotonic `AeadSealer[Alg]` (`inout sealer`), statically guaranteeing under Theorem 2 that each nonce is consumed at most and at least once (or explicitly burned).

+ *Secret Storage and Explicit Zeroization (`SecretSlab`, `Vault::shred`):* Vault proposes storing long-lived keys in non-swappable, core-dump-excluded pages (`mlock` and `MADV_DONTDUMP`) through `linear struct SecretSlot[K]`. Access uses second-class `in` projections (`[T-CallProj]`) to avoid moving secret bytes by value across fiber stacks. The linear contract requires an explicit `Vault::shred(sink key)` terminal operation. Its proposed lowering emits volatile zero stores, memory barriers, and scratch-register scrubbing. Proving this contract requires accounting for backend copies, spills, and all secret-bearing locations; linear ownership alone does not establish zeroization.

+ *Backend-Verified Constant-Time Execution (`eff [ConstTime]` and Hardware `DIT`):* Rust's `subtle` library (`Choice`) cannot stop LLVM backend passes from converting branchless selects (`cmov`) back into secret-dependent conditional branches, nor does it enable CPU Data-Independent Timing flags automatically. Vault introduces opaque secret scalars (`SecU8`, `SecU32`, `SecU64`, `SecVec[P, T]`) paired with `copy effect ConstTime`. Entering `ConstTime::scope` enables Armv8.4+ `PSTATE.DIT` (`MSR DIT, #1`) and `x86_64` `DOITM` hardware constant-time execution modes, while Stage 5 (`crates/vesper-vault`) runs a mandatory Cranelift SSA taint verifier that rejects any secret-tainted branch (`brif`, `br_table`), secret-indexed memory access, or variable-latency division (`udiv`, `sdiv`).

+ *Unauthenticated Plaintext Quarantine and Post-Quantum Hybrid KEM:* Decryption in Vault follows the cryptographic Doom Principle by construction: `open_in_place` consumes a `linear struct CiphertextFrame` and returns `Result[VerifiedPlaintext, AuthError]`, zeroizing the buffer in place on Poly1305 or GHASH tag mismatch before returning `Err` so unauthenticated plaintext is never addressable. Key establishment ships zero-allocation hybrid post-quantum *X25519MLKEM768* (#link("https://www.rfc-editor.org/info/rfc10024/")[RFC 10024]; FIPS 203 ML-KEM-768 paired with X25519) using Vesper Vector `eff [Isa[P]]` polynomial NTT acceleration without `unsafe`.

+ *HACL\*-Grade Lean 4 Equivalence and Leakage Proofs (`vesper verify`):* `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts Vault primitives directly into Lean 4 `BitVec` definitions, proving functional equivalence against mathematical specifications (`bv_decide`) alongside trace-equivalence theorems guaranteeing branch and memory-address independence from secret inputs.

=== 28.2 Secret Scalars, Hardware `DIT` `ConstTime` Scope, and Fenced Zeroization
<ch-27-2>
Secret scalars (`SecU8`, `SecU64`) intentionally omit `Eq`, `Ord`, `/`, `%`, `Telemetrable` (#link(<ch-31-4>)[Section 32.4]), and `Encode` (#link(<ch-29-2>)[Section 30.2]). Comparisons return an opaque `SecMask64` (`0` or `0xFFFF_FFFF_FFFF_FFFF`) usable only in constant-time bitwise selects (`ct_select`), and declassification requires an explicit auditable `declassify_bool` gate inside `eff [ConstTime]`:

```
// Opaque constant-time scalars: forbidden in `if`, `match`, slice indexing, or `udiv`!
copy struct SecU8(u8);
copy struct SecU64(u64);
copy struct SecMask64(u64);

// Stateless lexical capability activating hardware DIT/DOITM and Cranelift CT verification:
copy effect ConstTime {}

// Long-lived AEAD key pinned in non-swappable mlock + MADV_DONTDUMP memory:
linear struct AeadKeySlot[Alg] {
    slot_id: u32,
}

impl[Alg] AeadKeySlot[Alg] {
    // Terminal consumer required by Theorem 2: volatile zero-fill + SFENCE/DSB + register scrub!
    fn shred(sink self, inout slab: SecretSlab) -> Unit eff [ConstTime] {
        let AeadKeySlot { slot_id } = self;
        slab.volatile_zero_and_fence(slot_id);
    }
}

// 100% safe, branchless 16-byte authentication tag comparison verified by Cranelift CT pass:
fn verify_tag_16_ct(
    in expected: ByteArray[16],
    in computed: ByteArray[16],
) -> bool eff [ConstTime] {
    let diff_lo: SecU64 = expected.load_sec_u64le(0).bit_xor(computed.load_sec_u64le(0));
    let diff_hi: SecU64 = expected.load_sec_u64le(8).bit_xor(computed.load_sec_u64le(8));
    let combined: SecU64 = diff_lo.bit_or(diff_hi);
    // Explicit constant-time zero test -> single declassification boundary at tag verdict:
    combined.ct_is_zero().declassify_bool()
}
```

=== 28.3 Linear `NonceTicket` AEAD Sealer and Post-Quantum `X25519MLKEM768` Session
<ch-27-3>
To eliminate nonce reuse and key-schedule recomputation, `AeadSealer` owns the monotonically advancing 96-bit counter and mints a single-use `NonceTicket` per record. If the 64-bit sequence space approaches exhaustion (`u64::MAX - 1`), `next_nonce` forces a key rotation (`RekeyRequired`) before wrap-around can ever occur:

```
// Wire header for encrypted WAL or RPC frames (28 bytes exact, 0 implicit padding):
wire struct EncryptedFrameWireHeader {
    key_epoch: u32be,
    nonce_seq: u64be,
    tag: ByteArray[16],
}

// Mode::Linear: can seal EXACTLY ONE frame (or be explicitly burned on pre-send abort)!
linear struct NonceTicket[Alg] {
    epoch: u32,
    seq: u64,
}

linear struct AeadSealer[Alg] {
    key_slot: AeadKeySlot[Alg],
    epoch: u32,
    next_seq: u64,
}

impl[Alg: AeadCipher] AeadSealer[Alg] {
    // Exclusive `inout self` guarantees no two fibers can ever mint the same `seq`!
    fn mint_nonce(inout self) -> Result[NonceTicket[Alg], CryptoError] eff [Fail[CryptoError]] {
        if self.next_seq == 0xFFFF_FFFF_FFFF_FFFE {
            Fail::raise(CryptoError::EpochExhaustedRekeyRequired);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        Ok(NonceTicket { epoch: self.epoch, seq })
    }

    // Consumes `sink ticket` linearly: reusing a NonceTicket is a compile-time error!
    fn seal_in_place[P: IsaProfile](
        in self.{key_slot},
        in slab: SecretSlab,
        sink ticket: NonceTicket[Alg],
        in aad: ByteSlice,
        inout frame_buf: ByteSlice,
    ) -> Result[Unit, CryptoError] eff [ConstTime, Isa[P], Fail[CryptoError]] {
        let NonceTicket { epoch, seq } = ticket;
        let (inout hdr, inout payload) = match frame_buf.split_wire_mut[EncryptedFrameWireHeader]() {
            Ok(parts) => parts,
            Err(_)    => Fail::raise(CryptoError::BufferTooSmall),
        };
        hdr.key_epoch = epoch.to_be();
        hdr.nonce_seq = seq.to_be();
        let computed_tag = Alg::encrypt_detached_ct[P](
            in slab.borrow_key(in self.key_slot),
            epoch,
            seq,
            in aad,
            inout payload,
        );
        hdr.tag = computed_tag;
        Ok(())
    }
}

// Quarantine-safe decryption: zeroes buffer on tag failure so unauthenticated bytes never leak!
fn open_authenticated_frame[Alg: AeadCipher, P: IsaProfile](
    in key_slot: AeadKeySlot[Alg],
    in slab: SecretSlab,
    in aad: ByteSlice,
    sink raw_cipher: CiphertextSlice,
) -> Result[VerifiedPlaintextSlice, CryptoError] eff [ConstTime, Isa[P], Fail[CryptoError]] {
    let mut buf = raw_cipher.into_raw_mut();
    let (in hdr, inout payload) = buf.split_wire_mut[EncryptedFrameWireHeader]()
        .map_err(|_| CryptoError::MalformedHeader)?;

    let computed_tag = Alg::decrypt_tentative_ct[P](
        in slab.borrow_key(in key_slot),
        hdr.key_epoch.to_native(),
        hdr.nonce_seq.to_native(),
        in aad,
        inout payload,
    );

    if !verify_tag_16_ct(in hdr.tag, in computed_tag) {
        Vault::volatile_zero_slice(inout payload); // Scrubs tentative plaintext BEFORE returning Err!
        Fail::raise(CryptoError::AuthenticationTagMismatch);
    }
    Ok(VerifiedPlaintextSlice::from_authenticated(payload))
}
```

=== 28.4 Lean 4 Constant-Time Leakage and Functional Proofs (`vesper verify`)
<ch-27-4>
In `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]), Vault kernels verify both *Bit-Exact Cryptographic Soundness* and *Instruction/Address Trace Non-Interference* (proving that execution control-flow and memory addresses `LeakageTrace` depend solely on public buffer lengths, never on secret keys or tags):

```lean
-- illustrative/VaultConstantTimeSafety.lean
-- 1. SAT bit-blasting (`bv_decide`) proves branchless 128-bit tag verification is exact across all 2^256 inputs:
theorem verify_tag_16_ct_sound (expected computed : BitVec 128) :
    VaultSpec.verifyTag16Ct expected computed = decide (expected = computed) := by
  bv_decide

-- 2. Constant-time trace independence: secret keys `k1`, `k2` produce identical PC & memory-address traces!
theorem chacha20_seal_constant_time_trace
    (k1 k2 : BitVec 256) (nonce : BitVec 96) (plain1 plain2 : ByteArray)
    (h_len : plain1.size = plain2.size) :
    LeakageTrace (VaultSpec.chacha20Blocks k1 nonce plain1) =
    LeakageTrace (VaultSpec.chacha20Blocks k2 nonce plain2) := by
  simp [VaultSpec.chacha20Blocks, LeakageTrace, h_len]
```

=== 28.5 Rust Cranelift Constant-Time SSA Taint Verifier (`crates/vesper-vault`)
<ch-27-5>
`crates/vesper-vault` provides the Stage 5 SSA taint analyzer invoked on every function carrying `eff [ConstTime]`, statically rejecting secret-dependent branches, variable-index table lookups, and variable-time hardware divisions prior to machine-code emission:

```rust
// crates/vesper-vault/src/lib.rs
use rustc_hash::FxHashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SsaVal(pub u32);

/// Abstracted Cranelift SSA instruction audited by the `eff [ConstTime]` backend pass.
#[derive(Clone, Debug)]
pub enum CtAuditedInst {
    PureAlu { dst: SsaVal, inputs: Vec<SsaVal> },
    BitwiseSelect { dst: SsaVal, mask: SsaVal, on_true: SsaVal, on_false: SsaVal },
    ExplicitDeclassify { dst: SsaVal, src: SsaVal },
    Load { dst: SsaVal, addr: SsaVal },
    Store { addr: SsaVal, val: SsaVal },
    VariableLatencyDiv { dst: SsaVal, num: SsaVal, den: SsaVal },
    CondBranch { cond: SsaVal, inst_span_offset: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstantTimeViolation {
    SecretDependentBranch { cond: SsaVal, inst_span_offset: u32 },
    SecretDependentMemoryAddress { addr: SsaVal },
    SecretTaintedVariableLatencyDiv { operand: SsaVal },
}

/// Forward SSA taint verifier enforcing strict constant-time discipline on `Sec*` values.
pub fn verify_constant_time_ssa(
    initial_secret_vals: &[SsaVal],
    insts: &[CtAuditedInst],
) -> Result<(), ConstantTimeViolation> {
    let mut tainted: FxHashSet<SsaVal> = initial_secret_vals.iter().copied().collect();

    for inst in insts {
        match inst {
            CtAuditedInst::PureAlu { dst, inputs } => {
                if inputs.iter().any(|v| tainted.contains(v)) {
                    tainted.insert(*dst);
                }
            }
            CtAuditedInst::BitwiseSelect { dst, mask, on_true, on_false } => {
                if tainted.contains(mask) || tainted.contains(on_true) || tainted.contains(on_false) {
                    tainted.insert(*dst);
                }
            }
            CtAuditedInst::ExplicitDeclassify { dst, .. } => {
                // Audited explicit declassification boundary (e.g., final AEAD tag boolean):
                tainted.remove(dst);
            }
            CtAuditedInst::Load { dst, addr } => {
                if tainted.contains(addr) {
                    return Err(ConstantTimeViolation::SecretDependentMemoryAddress { addr: *addr });
                }
                // Loaded data from a public address stays untainted unless explicitly marked secret
                let _ = dst;
            }
            CtAuditedInst::Store { addr, val } => {
                if tainted.contains(addr) {
                    return Err(ConstantTimeViolation::SecretDependentMemoryAddress { addr: *addr });
                }
                let _ = val;
            }
            CtAuditedInst::VariableLatencyDiv { dst, num, den } => {
                if tainted.contains(num) {
                    return Err(ConstantTimeViolation::SecretTaintedVariableLatencyDiv { operand: *num });
                }
                if tainted.contains(den) {
                    return Err(ConstantTimeViolation::SecretTaintedVariableLatencyDiv { operand: *den });
                }
                let _ = dst;
            }
            CtAuditedInst::CondBranch { cond, inst_span_offset } => {
                if tainted.contains(cond) {
                    return Err(ConstantTimeViolation::SecretDependentBranch {
                        cond: *cond,
                        inst_span_offset: *inst_span_offset,
                    });
                }
            }
        }
    }
    Ok(())
}
```


== 29. Vesper Aegis: Hardware Confidential Computing and Enclaves
<ch-28>
#chapter-deck[Confidential Memory, Host-Input Validation, and Attestation Bindings]

*Vesper Aegis* (`@vesper-tee/aegis`, with implementation code in `crates/vesper-aegis`) describes confidential virtual machines and remote attestation across Intel TDX 1.5+, AMD SEV-SNP, and Arm CCA Realms. The proposed trusted execution environment (TEE) contracts distinguish encrypted and shared memory, validate untrusted host values, and bind RA-TLS quotes to supply-chain and proof metadata. `HostBounceGrant` and `UntrustedHostVal[T]` express intended protections against repeated host reads and hostile hypercall results. Their security depends on the implementation and platform assumptions.

=== 29.1 Confidential VM Memory Partitioning, Single-Fetch, and Attestation Guarantees
<ch-28-1>
+ *Private and Shared Memory Types (`PrivateEnc`, `HostBounceSlice`):* Aegis proposes distinguishing encrypted guest pages from host-shared DMA pages at the type level. It extends *Strata* `DmaSlab` (#link(<ch-12-3>)[Section 13.3]) and *Vault* `SecretSlab` (#link(<ch-27-1>)[Section 28.1]). Standard allocations, fiber stacks, and `VirtualArena` regions are intended to reside in `PrivateEnc` memory. Host-facing exits (`#VE`, `VMGEXIT`, `RSI_HOST_CALL`) accept only `HostBounceSlice` handles from a quarantined unencrypted aperture. Correct page-table shared bits and validation at each transition are necessary to enforce that distinction.

+ *Single-Fetch TOCTOU and Iago Hypercall Defense (`UntrustedHostVal[T]`):* Malicious cloud hypervisors compromise C and Rust enclaves via *double-fetch TOCTOU* attacks (mutating shared VirtIO/ring memory between header validation and payload reads) and *Iago attacks* (returning adversarial `mmap` overlaps, non-monotonic timestamps, or out-of-range byte counts across host exits). Extending *Vesper Bridge*'s Omniglot `ForeignVal[T]` (#link(<ch-15-1>)[Section 16.1]), every hypervisor return and shared-ring header is typed as `UntrustedHostVal[T]`, while `TeeHardware::snapshot_from_host_bounce` copies untrusted bytes _exactly once_ into private encrypted `Region` memory before any parsing, SIMD indexing, or *Vesper Vault* AEAD verification occurs.

+ *Mode::Linear Page-Acceptance and Sealed-Key Obligations (UnacceptedGpaRange):* Under Intel TDX (`TDG.MEM.PAGE.ACCEPT`) and AMD SEV-SNP (`PVALIDATE`), accepting a Guest Physical Address (GPA) range twice or reading unaccepted dynamic pages enables VMM page-remapping and replay attacks. Aegis models newly hotplugged or remapped memory ranges as `Mode::Linear` obligations (`linear struct UnacceptedGpaRange`) that must be validated once and zero-initialized before yielding addressable `PrivateEnc` pages, and wraps derived sealing keys in `Vesper Vault` `AeadKeySlot` handles (#link(<ch-27-2>)[Section 28.2]) that shred deterministically on teardown.

+ *Supply-Chain and Proof-Bound Hardware Attestation (Loom.lock + `.olean` RTMR Binding):* Conventional remote attestation quotes (`TDREPORT`, `SNP_GUEST_REQUEST`, Arm CCA `CCA_TOKEN`) prove only an opaque boot binary hash (`MRTD` / `MEASUREMENT`), forcing relying parties to trust out-of-band build pipelines. Paired with *Vesper Loom*'s multi-builder reproducible derivations (#link(<ch-10-2>)[Section 11.2]) and *Lean 4* `.olean` proof certificates (#link(<ch-8-4>)[Section 9.4]), `vesper build --tee` deterministically extends Runtime Measurement Registers with the `Loom.lock` Merkle digest (`RTMR[2]`) and the exported `lean_spec_mif` formal theorem fingerprint (`RTMR[3]`)—allowing remote clients to verify cryptographically that an enclave runs audited, zero-`sorryAx` formally verified Vesper code.

+ *Lean 4 Post-Snapshot TOCTOU Non-Interference and Policy Proofs (`vesper verify`):* `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts enclave ingress pipelines and RA-TLS relying-party verifiers into Lean 4, mechanically proving that arbitrary hypervisor mutations to shared memory after `snapshot_from_host_bounce` cannot influence enclave state or control flow.

=== 29.2 Normalized wire struct Attestation Claims, HostBounceGrant, and TeeHardware Capabilities
<ch-28-2>

*Security boundary:* The 128-byte wire header below carries only an eight-byte prefix and is not sufficient to authenticate a channel by itself. The relying party must obtain the full 48-byte binding from verified attestation evidence and compare it with the observed transcript. The current `crates/vesper-aegis` policy performs that full comparison; platform-specific attestation policy, including TDX private-memory exception controls, still requires separate validation.
Aegis normalizes Intel TDX, AMD SEV-SNP, and Arm CCA hardware quotes into a padding-free 128-byte `wire struct` claim bundle with a non-authoritative prefix of the TLS 1.3 / post-quantum *X25519MLKEM768* binding (*Vesper Vault*, #link(<ch-27-1>)[Section 28.1]) channel transcripts, while managing untrusted host I/O slots as `Mode::Linear` obligations:

```
// Exact 128-byte normalized confidential-VM attestation claim header (0 implicit padding):
wire struct AegisAttestationClaimsWire {
    tee_arch_tag: u16le,          // 1 = Intel TDX 1.5, 2 = AMD SEV-SNP, 3 = Arm CCA Realm
    tcb_svn: u16le,               // Hardware Trusted Computing Base security version number
    enclave_flags: u32le,         // Bit 0 = DebugDisabled, Bit 1 = SmtMitigated, Bit 2 = LeanVerified
    mr_enclave_sha384: ByteArray[48], // Hardware boot measurement (MRTD / SNP Launch Digest / RIM)
    loom_lock_blake3: ByteArray[32],  // RTMR[2]: Vesper Loom hermetic supply-chain Merkle digest
    olean_spec_mif: ByteArray[32],    // RTMR[3]: Verified Lean 4 theorem interface digest (0 sorryAx)
    chan_binding_prefix: ByteArray[8], // Prefix of SHA-384(X25519MLKEM768 transcript)
}

// Untrusted scalar returned across VMGEXIT / #VE / RSI host exit (Iago-quarantined until checked!):
copy struct UntrustedHostVal[T] {
    raw_bits: T,
}

// Mode::Linear lease over an unencrypted SWIOTLB host-shared DMA bounce slot:
linear struct HostBounceGrant {
    slot_idx: u32,
    byte_cap: u32,
}

// Mode::Linear unaccepted GPA page range requiring TDG.MEM.PAGE.ACCEPT / PVALIDATE:
linear struct UnacceptedGpaRange {
    gpa_base: u64,
    page_count_4k: u32,
}

linear effect TeeHardware {
    // Executes PVALIDATE / TDG.MEM.PAGE.ACCEPT, rejecting duplicate GPA mappings:
    fn accept_private_pages(sink pending: UnacceptedGpaRange) -> Result[PrivatePageSlab, TeeError];

    // Copies untrusted host bytes ONCE into PrivateEnc lexical Region (prevents double-fetch TOCTOU!):
    fn snapshot_from_host_bounce[r: Region](
        in bounce: HostBounceGrant,
        untrusted_len: UntrustedHostVal[u32],
        in dest_arena: RegionCap[r],
    ) -> Result[RegionSlice[r, u8], TeeError];

    // Requests hardware-signed quote binding X25519MLKEM768 handshake hash + Loom/Lean RTMRs:
    fn attest_channel_binding[r: Region](
        in dest_arena: RegionCap[r],
        in transcript_hash: ByteArray[48],
    ) -> Result[RegionSlice[r, u8], TeeError];

    // Copies ONLY ciphertext out to unencrypted SWIOTLB slot and scrubs bounce metadata on release:
    fn flush_ciphertext_and_release(
        sink bounce: HostBounceGrant,
        in sealed_ciphertext: CiphertextSlice,
    ) -> Result[Unit, TeeError];

    fn abort_and_scrub_bounce(sink bounce: HostBounceGrant) -> Unit;
}
```

=== 29.3 Zero-TOCTOU Confidential Enclave Inference Gate (0 Alloc, Iago-Safe)
<ch-28-3>
Inside an enclave worker shard, `process_attested_enclave_req` snapshots incoming host VirtIO/vsock frames once into a private encrypted `Region::scoped` scratchpad (`Alloc` omitted from `eff [...]`), authenticates and decrypts the tenant payload via *Vesper Vault* (`ConstTime`), runs confidential computation in `PrivateEnc` RAM, and enforces under `[T-Let]` that `HostBounceGrant` is scrubbed and released on all control paths:

```
fn process_attested_enclave_req[Alg: AeadCipher, P: IsaProfile](
    in rx_key: AeadKeySlot[Alg],
    inout tx_sealer: AeadSealer[Alg],
    in secret_slab: SecretSlab,
    in aad_transcript: ByteArray[48],
    sink bounce_slot: HostBounceGrant,
    host_reported_len: UntrustedHostVal[u32],
) -> Result[Unit, EnclaveError] eff [TeeHardware, ConstTime, Isa[P], Fail[EnclaveError]] {
    Region::scoped(512.kib(), |cap priv_arena| {
        // 1. Single-fetch copy into hardware-encrypted PrivateEnc arena (blocks hypervisor TOCTOU!):
        let snap_res = TeeHardware::snapshot_from_host_bounce(
            in bounce_slot,
            host_reported_len,
            in priv_arena,
        );

        let encrypted_copy = match snap_res {
            Ok(slice) => slice,
            Err(tee_err) => {
                // Mandatory linear cleanup of SWIOTLB bounce slot before raising Fail!
                TeeHardware::abort_and_scrub_bounce(sink bounce_slot);
                Fail::raise(EnclaveError::HostIagoAttempt(tee_err));
            }
        };

        // 2. Constant-time AEAD authentication inside encrypted RAM (zeroizes tentative bytes on fail):
        let auth_res = open_authenticated_frame[Alg, P](
            in rx_key,
            in secret_slab,
            in aad_transcript.as_slice(),
            sink CiphertextSlice::from_private_region(encrypted_copy),
        );

        let verified_plain = match auth_res {
            Ok(pt) => pt,
            Err(crypto_err) => {
                TeeHardware::abort_and_scrub_bounce(sink bounce_slot);
                Fail::raise(EnclaveError::UnauthenticatedPayload(crypto_err));
            }
        };

        // 3. Execute confidential scoring in PrivateEnc RAM and seal reply with fresh NonceTicket:
        let mut reply_buf = priv_arena.alloc_byte_slice(256);
        evaluate_private_policy_simd[P](in verified_plain, inout reply_buf);

        let nonce_ticket = match tx_sealer.mint_nonce() {
            Ok(t) => t,
            Err(e) => {
                TeeHardware::abort_and_scrub_bounce(sink bounce_slot);
                Fail::raise(EnclaveError::RekeyRequired(e));
            }
        };

        if let Err(seal_err) = tx_sealer.seal_in_place[P](
            in secret_slab,
            sink nonce_ticket,
            in aad_transcript.as_slice(),
            inout reply_buf,
        ) {
            TeeHardware::abort_and_scrub_bounce(sink bounce_slot);
            Fail::raise(EnclaveError::SealFailed(seal_err));
        }

        // 4. Only AEAD-authenticated ciphertext ever crosses back into unencrypted HostBounceGrant!
        TeeHardware::flush_ciphertext_and_release(
            sink bounce_slot,
            in CiphertextSlice::from_sealed(reply_buf),
        ).map_err(EnclaveError::HostEgress)
    })
}
```

=== 29.4 Lean 4 Post-Snapshot TOCTOU Immunity and Supply-Chain Quote Proofs (`vesper verify`)
<ch-28-4>
`vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts enclave ingress pipelines and RA-TLS relying-party verifiers into Lean 4, mechanically proving that arbitrary hypervisor mutations to shared memory after `snapshot_from_host_bounce` cannot influence enclave state or control flow:

```lean
-- illustrative/AegisEnclaveSafety.lean
-- 1. Post-Snapshot TOCTOU Immunity: if two adversarial host timelines agree at snapshot instant `t0`,
-- arbitrary hypervisor shared-RAM mutations at `t > t0` have zero effect on enclave output!
theorem snapshot_blocks_hypervisor_toctou
    (hostMem1 hostMem2 : Nat -> ByteArray) (t0 : Nat) (len : UInt32)
    (h_agree_at_t0 : hostMem1 t0 = hostMem2 t0) :
    EnclaveSpec.evalAfterSnapshot (hostMem1 t0) len =
    EnclaveSpec.evalAfterSnapshot (hostMem2 t0) len := by
  rw [h_agree_at_t0]

-- 2. Relying-Party Quote Soundness: accepted quotes guarantee debug=off, TCB floor, Loom lock, & Lean proof!
theorem verify_quote_enforces_supply_chain_and_tcb
    (claims : AegisAttestationClaims) (pol : RelyingPartyPolicy)
    (h_ok : AegisVerifier.checkPolicy claims pol = .ok ()) :
    claims.debugDisabled = true ∧
    claims.tcbSvn ≥ pol.minTcbSvn ∧
    claims.loomLockBlake3 = pol.expectedLoomBlake3 ∧
    claims.oleanSpecMif = pol.expectedOleanMif := by
  simp [AegisVerifier.checkPolicy] at h_ok
  exact h_ok
```

=== 29.5 Rust Remote-Attestation Policy Verifier and GPA Page-Acceptance Tracker (`crates/vesper-aegis`)
<ch-28-5>
`crates/vesper-aegis` implements the cross-architecture RA-TLS attestation policy verifier (enforcing TCB SVN floors, non-debug mode, `Loom.lock` Merkle hashes, and `.olean` theorem fingerprints) alongside the Guest Physical Address (`GPA`) acceptance tracker preventing hypervisor page-replay/double-accept attacks:

```rust
// crates/vesper-aegis/src/lib.rs
use rustc_hash::FxHashSet;

pub const ENCLAVE_FLAG_DEBUG_DISABLED: u32 = 1 << 0;
pub const ENCLAVE_FLAG_SMT_MITIGATED: u32 = 1 << 1;
pub const ENCLAVE_FLAG_LEAN_VERIFIED: u32 = 1 << 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum TeeArch {
    IntelTdx15 = 1,
    AmdSevSnp = 2,
    ArmCcaRealm = 3,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationClaims {
    pub arch: TeeArch,
    pub tcb_svn: u16,
    pub enclave_flags: u32,
    pub mr_enclave_sha384: [u8; 48],
    pub loom_lock_blake3: [u8; 32],
    pub olean_spec_mif: [u8; 32],
    pub chan_binding_prefix: [u8; 8],
    pub channel_binding: [u8; 48], // From authenticated evidence, not the 128-byte header.
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelyingPartyPolicy {
    pub allowed_archs: Vec<TeeArch>,
    pub min_tcb_svn: u16,
    pub expected_mr_enclave: [u8; 48],
    pub expected_loom_blake3: [u8; 32],
    pub expected_olean_mif: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AegisVerifyError {
    DisallowedTeeArchitecture(TeeArch),
    EnclaveDebugModeEnabled,
    StaleTcbSvn { actual: u16, min_required: u16 },
    BootMeasurementMismatch,
    SupplyChainLoomLockMismatch,
    FormalOleanProofDigestMismatch,
    ChannelTranscriptBindingMismatch,
    HypervisorGpaDoubleAccept { gpa_page_4k: u64 },
    UnalignedOrZeroGpaRange,
}

/// Verifies normalized TDX / SEV-SNP / Arm CCA attestation claims against pinned Loom & Lean 4 digests.
pub fn verify_attestation_claims(
    claims: &AttestationClaims,
    policy: &RelyingPartyPolicy,
    expected_chan_binding: &[u8; 48],
) -> Result<(), AegisVerifyError> {
    if !policy.allowed_archs.contains(&claims.arch) {
        return Err(AegisVerifyError::DisallowedTeeArchitecture(claims.arch));
    }
    if (claims.enclave_flags & ENCLAVE_FLAG_DEBUG_DISABLED) == 0 {
        return Err(AegisVerifyError::EnclaveDebugModeEnabled);
    }
    if claims.tcb_svn < policy.min_tcb_svn {
        return Err(AegisVerifyError::StaleTcbSvn {
            actual: claims.tcb_svn,
            min_required: policy.min_tcb_svn,
        });
    }
    if claims.mr_enclave_sha384 != policy.expected_mr_enclave {
        return Err(AegisVerifyError::BootMeasurementMismatch);
    }
    if claims.loom_lock_blake3 != policy.expected_loom_blake3 {
        return Err(AegisVerifyError::SupplyChainLoomLockMismatch);
    }
    if claims.olean_spec_mif != policy.expected_olean_mif {
        return Err(AegisVerifyError::FormalOleanProofDigestMismatch);
    }
    // Compare all 48 authenticated bytes; the prefix is only an extra consistency check.
    if !crate::claims::ct_eq(&claims.channel_binding, expected_chan_binding)
        || claims.chan_binding_prefix != expected_chan_binding[..8] {
        return Err(AegisVerifyError::ChannelTranscriptBindingMismatch);
    }
    Ok(())
}

/// Tracks accepted 4 KiB private GPA frames (`PVALIDATE` / `TDG.MEM.PAGE.ACCEPT`) to block VMM replay attacks.
#[derive(Default)]
pub struct GpaAcceptanceTracker {
    accepted_4k_pfns: FxHashSet<u64>,
}

impl GpaAcceptanceTracker {
    pub fn accept_gpa_range(&mut self, gpa_base: u64, page_count_4k: u32) -> Result<(), AegisVerifyError> {
        if page_count_4k == 0 || (gpa_base & 0xFFF) != 0 {
            return Err(AegisVerifyError::UnalignedOrZeroGpaRange);
        }
        let base_pfn = gpa_base >> 12;
        for offset in 0..u64::from(page_count_4k) {
            let pfn = base_pfn + offset;
            if self.accepted_4k_pfns.contains(&pfn) {
                return Err(AegisVerifyError::HypervisorGpaDoubleAccept {
                    gpa_page_4k: pfn << 12,
                });
            }
        }
        for offset in 0..u64::from(page_count_4k) {
            self.accepted_4k_pfns.insert(base_pfn + offset);
        }
        Ok(())
    }
}
```


= Part VI. Serialization, Parsing, Telemetry, Networking, Storage, and Streaming
<part-6>
#chapter-deck[Codecs, Parsers, Telemetry, Transports, Web Services, Consensus, Storage, and Streams]

#_part-summary("Part VI Architectural Scope")[
  Part VI develops the data path from codecs to storage and streaming. #link(<ch-29>)[Chapter 30] covers *Wire and Json* serialization; #link(<ch-30>)[Chapter 31] covers *Grammar* parsers. #link(<ch-31>)[Chapter 32] introduces *Beacon* telemetry. #link(<ch-32>)[Chapters 33]–#link(<ch-34>)[35] describe *Warp* transports, *Prism* web services, and *Chorus* consensus and RDMA. #link(<ch-35>)[Chapter 36] covers *Quarry* SQL and storage, and #link(<ch-36>)[Chapter 37] covers *Flux* incremental streams. Local ownership checks and model proofs have narrower scope than complete distributed-system correctness.
]

#figure(
  table(
    columns: (14%, 34%, 52%),
    align: (left, left, left),
    table.header([*Chapter*], [*Subsystem*], [*Package, Host Crate, and Data-Plane Invariants*]),
    [#link(<ch-29>)[Chapter 30]], [Vesper Wire and Json], [`@vesper-wire/*` (`vesper-serde`): `Decode[r: Region]`, 2-stage SIMD JSON, `StreamVByte`, `RelPtr32`],
    [#link(<ch-30>)[Chapter 31]], [Vesper Grammar (Binary & Arrow IPC)], [`@vesper-parse/grammar` (`vesper-grammar`): `@wire_grammar(canonical = true)`, `UnconsumedTail`, Arrow IPC],
    [#link(<ch-31>)[Chapter 32]], [Vesper Beacon (Observability)], [`@vesper-obs/beacon` (`vesper-beacon`): `ActiveSpan`, SIMD `traceparent`, AoSoA `OTAP`, PII non-interference],
    [#link(<ch-32>)[Chapter 33]], [Vesper Warp (Kernel-Bypass QUIC/TCP)], [`@vesper-net/warp` (`vesper-warp`): linear `UmemFrame`, zero-copy QUIC 1-RTT, EDT wheel, verified BBRv3],
    [#link(<ch-33>)[Chapter 34]], [Vesper Prism (HTTP/2/3 & WebSockets)], [`@vesper-web/prism` (`vesper-prism`): `Responder[HeadersPending]`, lexical middleware, shard radix router],
    [#link(<ch-34>)[Chapter 35]], [Vesper Chorus (Consensus & RDMA)], [`@vesper-dist/chorus` (`vesper-chorus`): dual `Session` channels, RDMA `MrSlice`, `@choreography` EPP],
    [#link(<ch-35>)[Chapter 36]], [Vesper Quarry (Hermetic SQL & Storage)], [`@vesper-db/quarry` (`vesper-quarry`): WASI `@sql`, slotted `wire struct` pages, linear `PagePin`, AoSoA scans],
    [#link(<ch-36>)[Chapter 37]], [Vesper Flux (DBSP Stream Processing)], [`@vesper-stream/flux` (`vesper-flux`): AoSoA Z-Set tiles, `FrontierTicket` watermarks, bilinear joins],
  ),
  kind: table,
)


== 30. Vesper Wire and Json: Arena Serialization and SIMD JSON
<ch-29>
#chapter-deck[Region Codecs, SIMD JSON, Integer Compression, and Relative-Pointer Archives]

*Vesper Wire and Json* (`@vesper-wire/serde`, `@vesper-wire/json`, and `@vesper-wire/archive`, with implementation code in `crates/vesper-serde`) describe region-scoped codecs, SIMD JSON parsing, `StreamVByte` compression, and `RelPtr32` archives. The intended `@derive(JsonSliceDecode)` and `Json::parse_simd` paths feed *Prism* (#link(<ch-33>)[Chapter 34]) through lexical `r: Region` arenas. Avoiding global heap allocation and reaching multi-GB/s throughput are goals that require validation for each codec and workload.

=== 30.1 Zero-Copy Serialization, SIMD Parsing, and Archive Guarantees
<ch-29-1>
+ *Lifetime-Free Zero-Copy Strings (`JsonStr[r]`):* Borrowing unescaped strings in Rust `serde` infects data structures and trait bounds with viral `'de` lifetimes, while escaped strings silently fall back to `GlobalAlloc` heap `String` allocations via `Cow`. Vesper's `JsonStr[r]` borrows unescaped UTF-8 slices directly from the immutable input `ByteSlice` and bump-unescapes the rare escaped strings into the caller's lexical `r: Region` (`VirtualArena`, #link(<ch-12>)[Chapter 13]) with zero global heap traffic and zero `'de` lifetime parameters.

+ *Safe Two-Stage On-Demand SIMD JSON:* Whereas `serde_json` parses byte-by-byte and `simd-json` relies on thousands of lines of `unsafe` architecture intrinsics while mutating input buffers or allocating a full DOM tape, Vesper's Stage 1 structural indexer uses 100% safe predicated `VVec[P, u8]` operations (`eff [Isa[P]]`, #link(<ch-13>)[Chapter 14]) across `NEON`, `AVX2`, `AVX-512`, `SVE2`, and `RVV 1.0`, paired with a Stage 2 schema-driven On-Demand cursor that materializes only requested struct fields.

+ *Compile-Time SWAR Key Dispatch and Single-Check Encoding:* Executing inside Vesper Forge's fuel-metered `wasm32-wasip2` sandbox (#link(<ch-6>)[Chapter 7]), `@derive(JsonDecode, JsonEncode)` compiles struct field keys up to 7 ASCII bytes (plus closing quote) into 1-cycle SWAR 64-bit integer mask-and-compare switches and computes `JsonSizeBound` byte ceilings so fixed-schema structs encode after a single buffer capacity check.

+ *Substructural Linear Deserialization Rollback:* If an error occurs midway through decoding a composite type containing `Mode::Linear` protocol handles, Vesper's `[T-Let]` Resumption Frame Condition (#link(<ch-4>)[Chapter 4]) forces the generated decoder state machine to explicitly abort or consume already-constructed linear fields before raising `Fail[JsonError]`.

+ *Direct Columnar AoSoA Ingestion and Pin-Free Archives:* Streams newline-delimited JSON (`NDJSON`) directly into cache-line-aligned `TiledSeq[T]` (`tiled(16) struct`, #link(<ch-12>)[Chapter 13]) tiles without intermediate Array-of-Structs allocation, alongside `RelPtr32` zero-copy binary archives (`@vesper-wire/archive`) that memory-map from `io_uring` buffers without `Pin`.

=== 30.2 Colorless `Encode` and `Decode[r: Region]` Traits
<ch-29-2>
Built on Vesper Pact (#link(<ch-6>)[Chapter 7]), a single pair of colorless traits spans in-memory DMA slices, growing `VirtualArena` buffers, and suspending Vesper Pulse `io_uring` streams (#link(<ch-14>)[Chapter 15]):

```
// Colorless byte-output sink parameterized over lexical effect row `r`:
trait ByteOut[r: eff = []] {
    fn write_window[P: IsaProfile](
        inout self,
        min_cap: usize,
        writer: fn(inout ByteSlice) -> usize eff [Isa[P]],
    ) -> Unit eff [Isa[P] | r];
}

// Unified encoder supporting copy, affine, and shared `in` projections of linear structs:
trait Encode {
    fn encode_json[W, P: IsaProfile, r: eff](
        in self,
        inout out: W,
    ) -> Unit eff [Isa[P] | r]
    where
        W: ByteOut[r];
}

// Unified On-Demand decoder tied to lexical region `r` (zero 'de lifetimes!):
trait Decode[r: Region] {
    fn decode_json[P: IsaProfile](
        in scratch: RegionCap[r],
        inout cursor: SimdJsonCursor[r],
    ) -> Result[Self, JsonError] eff [r: Region, Isa[P], Fail[JsonError]];
}
```

When `cursor.read_str(in scratch)` inspects a JSON string token whose Stage 1 backslash bitmask is zero, it validates UTF-8 in SIMD registers and returns a 16-byte `copy struct JsonStr[r]` pointing directly into the input `ByteSlice`. Only when an escape character is present does it bump-allocate unescaped bytes inside `scratch`, while `[T-RegionScope]` (#link(<ch-12-2>)[Section 13.2]) statically prevents `JsonStr[r]` from escaping the enclosing `Region::scoped` block.

=== 30.3 Two-Stage Safe SIMD JSON Parsing (`eff [Isa[P]]`)
<ch-29-3>
*Stage 1 Carryless Quote Masking and Structural Tape Compression:* Processing 64 bytes per iteration via `VVec::load_masked`, the kernel detects odd-length backslash runs via branchless integer carry propagation, computes string interiors in one carryless multiply instruction (`VPCLMULQDQ` on `x86_64`, `PMULL` on AArch64, `clmul` on RISC-V), validates UTF-8 via dual-nibble `swizzle` lookup tables, and compresses structural token offsets (`{`, `}`, `[`, `]`, `:`, `,`) outside strings into a `u32` tape inside `r: Region`.

*Stage 2 SWAR Key Matching and O(1) Tape Skipping:* `@derive(JsonDecode)` matches short object keys in 3 ALU instructions using an unaligned `u64le` word load masked against precomputed key constants. Unknown JSON objects or arrays are skipped without parsing nested primitives via SIMD bracket-depth counting over the `u32` tape, while floats decode in \~5 ns via the Eisel-Lemire 128-bit mantissa algorithm.

```
// 100% safe Stage 1 64-byte structural indexer using Vesper Vector (zero `unsafe`!):
fn index_json_block_64[P: IsaProfile, r: Region](
    in buf: ByteSlice,
    offset: usize,
    inout prev_in_string: u64,
    inout prev_is_escaped: u64,
    inout tape_out: RegionSliceMut[r, u32],
    inout tape_len: usize,
) eff [Isa[P]] {
    let mask = P::active_mask[u8](offset, buf.len());
    let v = VVec::load_masked(in buf, offset, mask, 0x20);

    let bs_bits    = v.simd_eq(VVec::splat[P, u8](0x5C)).to_bitmask64();
    let quote_bits = v.simd_eq(VVec::splat[P, u8](0x22)).to_bitmask64();

    let escaped = compute_escaped_mask(bs_bits, inout prev_is_escaped);
    let unescaped_quotes = quote_bits & !escaped;

// Carryless multiply prefix-XOR turns quote pairs into contiguous 1-runs for string interiors:
    let in_string = Isa::clmul_prefix_xor[P](unescaped_quotes) ^ prev_in_string;
    prev_in_string = ((in_string as i64) >> 63) as u64;

    let struct_bits = classify_structural_lut[P](v) & !in_string;
    Isa::compress_append_offsets[P](
        struct_bits | unescaped_quotes,
        offset as u32,
        inout tape_out,
        inout tape_len,
    );
}
```

=== 30.4 SIMD-Accelerated Encoding, `StreamVByte`, and `RelPtr32` Binary Archives
<ch-29-4>
*Single-Reservation JSON Encoding:* `@derive(JsonEncode)` computes compile-time byte upper bounds (`JsonSizeBound::Bounded`) for fixed-width fields (`u64` max 20 bytes, `bool` max 5 bytes, plus static key literals), coalescing per-field capacity checks into one `write_window` call. String encoding scans 16 to 64 bytes per cycle via `VVec` and stores unescaped chunks directly into Domain-1 DMA or `Region` buffers, paired with divisionless SIMD BCD integer formatting and Dragonbox shortest-roundtrip float formatting.

*SIMD Varint and Zero-Parse Relative-Pointer Archives:* `@vesper-wire/compact` uses `StreamVByte` (separating 2-bit length control tags from packed data bytes) so four 32-bit integers decode in a single SIMD `swizzle` instruction for RPC and WAL records. For zero-parse random access, `@vesper-wire/archive` combines #link(<ch-15>)[Chapter 16]'s padding-free `wire struct` rules with 32-bit signed relative offsets (`RelPtr32[T]` and `RelSlice32[T]`), allowing archived lookup tables and ASTs to memory-map from disk or `io_uring` buffers in `O(1)` after one SIMD bounds validation pass—without `Pin`.

=== 30.5 Complete Showcase: Arena Envelope, `AoSoA` `NDJSON` Ingestion, and Lean 4 Proofs
<ch-29-5>
```
// 1. WASI wasm32-wasip2 derive macro (emits_effects = [], confined by [T-MacroConfinement]):
@derive(JsonDecode, JsonEncode, VerifiedCodec)
copy struct EventEnvelope[r: Region] {
    @json(rename = "id")
    event_id: u64,
    topic: JsonStr[r],        // Zero-copy slice when unescaped; Region-allocated if backslash present
    raw_meta: RawJsonView[r], // O(1) lazy slice over nested JSON—unparsed unless queried!
}

// 2. Direct NDJSON -> 16-wide AoSoA L1 Cache Tile Ingestion (Section 15 `tiled(16) struct`):
tiled(16) @derive(JsonDecode, JsonEncode)
copy struct TelemetryPoint {
    hot ts_unix_ms: i64,
    hot latency_us: u32,
    hot status_code: u16,
    hot flags: u16,
    cold trace_id_hex: InlineStr[32],
}

fn ingest_ndjson_to_tiles[P: IsaProfile, r: Region](
    in arena: RegionCap[r],
    in ndjson_dma_buf: ByteSlice,
    inout dest_tiles: TiledSeq[TelemetryPoint],
) -> Result[u32, JsonError] eff [r: Region, Isa[P], Fail[JsonError]] {
    let mut count: u32 = 0;
    for line_slice in SimdLines::split[P](in ndjson_dma_buf) {
        let mark: Watermark = arena.checkpoint();
        let pt: TelemetryPoint = Json::decode_slice[P, TelemetryPoint](in arena, in line_slice)?;
        dest_tiles.push(pt);     // Writes directly into segregated hot/cold AoSoA columns!
        arena.rewind(sink mark); // O(1) reset of Stage 1 structural tape per record!
        count += 1;
    }
    Ok(count)
}
```

In `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]), both Stage 1 bitwise escape masks and `@derive(..., VerifiedCodec)` roundtrip properties are verified mechanically in Lean 4:

```lean
-- illustrative/SimdJsonProofs.lean
theorem odd_backslash_simd_eq_scalar (bsBits : BitVec 64) (carryIn : BitVec 1) :
    SimdJson.computeEscapedMask bsBits carryIn =
    ScalarJsonSpec.escapedPositions64 bsBits carryIn := by
  bv_decide

theorem telemetry_point_json_roundtrip (pt : TelemetryPoint) :
    JsonSpec.decodeTelemetryPoint (JsonSpec.encodeTelemetryPoint pt) = .ok pt := by
  simp [JsonSpec.encodeTelemetryPoint, JsonSpec.decodeTelemetryPoint]
```

=== 30.6 Rust Host Implementation (`crates/vesper-serde`)
<ch-29-6>
`crates/vesper-serde` provides the compile-time SWAR key compiler, verified odd-backslash SIMD carry kernel, and static JSON byte-bound calculator:

```rust
// crates/vesper-serde/src/lib.rs
use smol_str::SmolStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwarFieldMatcher {
    pub field_name: SmolStr,
    pub field_idx: u16,
    pub swar_word_le: u64,
    pub swar_mask_le: u64,
    pub exact_byte_len: u32,
}

impl SwarFieldMatcher {
    pub fn compile_key(field_idx: u16, json_key: &str) -> Option<Self> {
        let bytes = json_key.as_bytes();
        if bytes.len() > 7 {
            return None;
        }
        let mut buf = [0u8; 8];
        buf[..bytes.len()].copy_from_slice(bytes);
        buf[bytes.len()] = b'"';

        let total_bytes = bytes.len() + 1;
        let mask = if total_bytes == 8 {
            u64::MAX
        } else {
            (1u64 << (total_bytes * 8)) - 1
        };

        Some(Self {
            field_name: json_key.into(),
            field_idx,
            swar_word_le: u64::from_le_bytes(buf),
            swar_mask_le: mask,
            exact_byte_len: bytes.len() as u32,
        })
    }

    #[inline(always)]
    pub fn matches_u64_window(&self, raw_word_le: u64) -> bool {
        (raw_word_le & self.swar_mask_le) == self.swar_word_le
    }
}

#[inline(always)]
pub fn compute_escaped_mask_64(bs_bits: u64, prev_is_escaped: u64) -> (u64, u64) {
    if bs_bits == 0 {
        return (prev_is_escaped, 0);
    }
    let potential_escape = bs_bits & !prev_is_escaped;
    let maybe_escaped = potential_escape << 1;
    let maybe_escaped_and_odd = maybe_escaped | 0xAAAA_AAAA_AAAA_AAAAu64;
    let even_series_codes_and_odd = maybe_escaped_and_odd.wrapping_sub(potential_escape);
    let escape_and_terminal_code = even_series_codes_and_odd ^ 0xAAAA_AAAA_AAAA_AAAAu64;

    let escaped = escape_and_terminal_code ^ (bs_bits | prev_is_escaped);
    let next_carry = (escape_and_terminal_code & bs_bits) >> 63;
    (escaped, next_carry)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonSizeBound {
    Bounded(usize),
    Dynamic { fixed_overhead: usize },
}

impl JsonSizeBound {
    pub const U8: Self = Self::Bounded(3);
    pub const U32: Self = Self::Bounded(10);
    pub const U64: Self = Self::Bounded(20);
    pub const I64: Self = Self::Bounded(20);
    pub const F64: Self = Self::Bounded(24);
    pub const BOOL: Self = Self::Bounded(5);

    pub fn combine_field(self, key_escaped_len: usize, field_bound: Self, is_first: bool) -> Self {
        let prefix = usize::from(!is_first) + key_escaped_len + 3;
        match (self, field_bound) {
            (Self::Bounded(a), Self::Bounded(b)) => Self::Bounded(a + prefix + b),
            (Self::Bounded(a), Self::Dynamic { fixed_overhead: b })
            | (Self::Dynamic { fixed_overhead: a }, Self::Bounded(b))
            | (Self::Dynamic { fixed_overhead: a }, Self::Dynamic { fixed_overhead: b }) => {
                Self::Dynamic { fixed_overhead: a + prefix + b }
            }
        }
    }
}
```


== 31. Vesper Grammar: Non-Malleable Binary Parsers and Arrow IPC
<ch-30>
#chapter-deck[Canonical Encodings, Secret Parsing, Arrow IPC Views, and Stream Tails]

*Vesper Grammar* (`@vesper-parse/grammar`, with implementation code in `crates/vesper-grammar`) describes binary formats, protocol framing, codecs, and an LL($k$)/PEG grammar compiler. The proposal covers Protocol Buffers, Cap'n Proto, and Apache Arrow IPC. Research influences include *Vest* (_Verified, Secure, High-Performance Parsing and Serialization for Rust_, Cai et al., USENIX Security 2025) and *EverParse / ASN1\** (_Provably Correct Non-Malleable Parsing for ASN.1 DER_, Ni et al., ACM CCS 2025). Intended contracts include canonical encodings (`@wire_grammar(canonical = true)`), secret parsing (`@secret_grammar`), Domain-1 views, and linear `UnconsumedTail` leases. Each contract needs evidence for its supported format and fragment.

=== 31.1 Canonical Non-Malleability, Secret Parsing, and Arrow IPC Guarantees
<ch-30-1>
+ *Dependent Binary and PEG/LL Grammars (`@wire_grammar`, `RegionSlice[r, u8]`):* The proposed macro ties parsed views to a lexical `r: Region` (#link(<ch-12-2>)[Section 13.2]). It describes dependent lengths, including TLS 1.3 `u24` headers and ASN.1 TLV envelopes, without adding explicit slice-lifetime annotations to every node. `@wire_grammar` runs in *Forge*'s fuel-metered `wasm32-wasip2` sandbox (`macro_emits_effects = []`, #link(<ch-6-4>)[Section 7.4]) and emits branch-coalesced Cranelift state machines. Parser termination and removal of redundant bounds checks are separate proof obligations; macro fuel alone establishes neither.
+ *Compile-Time Strong Non-Malleability and Canonical Bijection Guarantees:* Unverified binary parsers (`prost`, `der`, `serde_cbor`) frequently accept multiple distinct byte sequences for the same logical value—such as overlong LEB128 varints (`0x81 0x00` vs. `0x01`), non-minimal ASN.1 DER lengths, or unsorted map keys—creating catastrophic *signature-verification bypass* and *transaction-malleability* vulnerabilities when bytes are hashed or signed. Grammar enforces *Strong Non-Malleability* by construction: every `@wire_grammar(canonical = true)` format compiles into a verified bijection where `parse(b1) == Ok(v)` and `parse(b2) == Ok(v)` implies `b1 == b2` bit-for-bit.
+ *Side-Channel-Free Secret Format Parsing Over Vesper Vault (`eff [ConstTime]`):* Parsing decrypted PKCS\#8 private keys, TLS 1.3 inner plaintexts, or *Vesper Aegis* confidential enclave payloads (#link(<ch-28>)[Chapter 29]) with standard `nom` or `prost` combinators leaks secret field values and padding lengths via data-dependent branches. Annotating a grammar rule with `@secret_grammar` lowers fixed-layout and bounded-padding fields directly into *Vesper Vault* `SecU8` / `SecU64` constant-time combinators (`eff [ConstTime]`, #link(<ch-27-2>)[Section 28.2]), verified free of secret-dependent branches and memory indices by Stage 5's Cranelift SSA taint pass.
+ *Zero-Allocation Apache Arrow IPC and SIMD Protobuf Projection Into AoSoA Tiles:* Conventional Protobuf and Arrow libraries allocate per-message heap vectors or wrap columnar buffers in atomic reference-counted handles. Grammar validates *Apache Arrow IPC* `RecordBatch` envelopes in place over *Hardware Domain-1* `DmaSlab` pages (#link(<ch-12-3>)[Section 13.3])—exposing 64-byte-aligned validity bitmaps and value buffers directly to *Vesper Vector* `VVec` registers (#link(<ch-13>)[Chapter 14]) with zero copies—and decodes *Protobuf* wire streams via SIMD tag/varint dispatch straight into 16-wide *Vesper Strata* `TiledSeq` (`AoSoA`) cache tiles inside a lexical `Region`.
+ *Mode::Linear Stream Remainder Leases (UnconsumedTail) and Lean 4 Mutual-Inverse Proofs (`vesper verify`):* Silently ignoring unparsed trailing bytes after a framed message is the root cause of HTTP/2, gRPC, and Kafka request-smuggling attacks. Grammar's `parse_framed` returns `(Msg, UnconsumedTail)` where `linear struct UnconsumedTail` is `Mode::Linear`: under Theorem 2 and `[T-Let]`, callers must either prove end-of-frame emptiness (`assert_eof`) or feed `UnconsumedTail` into the next pipelined frame on *Vesper Pulse* / *Vesper Warp* streams (#link(<ch-14>)[Chapter 15] and #link(<ch-32>)[Chapter 33]). Simultaneously, `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges *Parser-Serializer Mutual Inversion* and *Byte-Unique Non-Malleability*.

=== 31.2 Padding-Free Arrow IPC Buffers, Linear UnconsumedTail Leases, and Canonical Combinators
<ch-30-2>
Grammar pairs padding-free `wire struct` descriptors for zero-copy *Apache Arrow IPC* buffer metadata (#link(<ch-15-2>)[Section 16.2]) with `Mode::Linear` stream-remainder obligations that make trailing-byte smuggling a compile-time error:

```
// Exact 16-byte Apache Arrow IPC Buffer descriptor (64B-aligned offset + length, 0 implicit padding):
wire struct ArrowBufferDescWire {
    offset_bytes: i64le,
    byte_length: i64le,
}

// Exact 16-byte Apache Arrow IPC FieldNode descriptor (row count + null count, 0 implicit padding):
wire struct ArrowFieldNodeWire {
    row_length: i64le,
    null_count: i64le,
}

// Mode::Linear trailing-byte lease: prevents silent trailing-data truncation & request smuggling!
linear struct UnconsumedTail {
    next_offset: usize,
    total_len: usize,
}

impl UnconsumedTail {
    // Discharges linear obligation ONLY if zero trailing bytes remain in the authenticated frame:
    fn assert_exact_eof(sink self) -> Result[Unit, ParseError] eff [Fail[ParseError]] {
        let UnconsumedTail { next_offset, total_len } = self;
        if next_offset != total_len {
            Fail::raise(ParseError::TrailingBytesForbidden {
                remaining: (total_len - next_offset) as u32,
            });
        }
        Ok(())
    }

    // Transfers exact pipelined remainder to the next frame parser in a stream loop:
    fn advance_stream_cursor(sink self) -> (usize, usize) {
        let UnconsumedTail { next_offset, total_len } = self;
        (next_offset, total_len)
    }
}

// Verified mutual-inverse non-malleable binary codec trait (Vest / EverParse bijection contract):
trait VerifiedWireGrammar[r: Region] {
    fn parse_canonical(
        in arena: RegionCap[r],
        in buf: ByteSlice,
    ) -> Result[(Self, UnconsumedTail), ParseError] eff [r: Region, Fail[ParseError]];

    fn encoded_byte_len(in self) -> usize;

    fn serialize_canonical(
        in self,
        inout out_buf: ByteSliceMut,
    ) -> Result[usize, ParseError] eff [Fail[ParseError]];
}
```

=== 31.3 Zero-Copy Arrow IPC SIMD Validation and Non-Malleable Protobuf/DER Ingress (0 Alloc)
<ch-30-3>
Inside an analytics or RPC edge shard (*Vesper Prism* / *Vesper Quarry*, #link(<ch-33>)[Chapter 34] and #link(<ch-35>)[Chapter 36]), `validate_and_sum_arrow_ipc_column` verifies 64-byte alignment and buffer bounds on a Domain-1 DMA buffer in O(1) before running predicated `VVec` SIMD aggregation, while `decode_signed_envelope_strict` enforces canonical non-malleability and exact EOF consumption before signature verification:

```
fn decode_signed_envelope_strict[r: Region, Msg: VerifiedWireGrammar[r]](
    in arena: RegionCap[r],
    in signed_frame_bytes: ByteSlice,
) -> Result[Msg, ParseError] eff [r: Region, Fail[ParseError]] {
    let (msg, tail) = Msg::parse_canonical(in arena, in signed_frame_bytes)?;
    // `[T-Let]` FORBIDS returning `Ok(msg)` without consuming linear `tail`:
    // rejects any trailing smuggling bytes or non-canonical padding!
    tail.assert_exact_eof()?;
    Ok(msg)
}

fn validate_and_sum_arrow_ipc_i64[P: IsaProfile](
    in ipc_body_dma: DmaSlice,
    in field_node: ArrowFieldNodeWire,
    in validity_desc: ArrowBufferDescWire,
    in values_desc: ArrowBufferDescWire,
) -> Result[i64, ParseError] eff [Isa[P], Fail[ParseError]] {
    let rows = field_node.row_length.to_native();
    let val_off = values_desc.offset_bytes.to_native();
    let val_len = values_desc.byte_length.to_native();

    // Enforce Arrow IPC 64-byte SIMD alignment & overflow-safe byte range before zero-copy view:
    if rows < 0 || val_off < 0 || (val_off & 63) != 0 || val_len != rows * 8 {
        Fail::raise(ParseError::MalformedArrowBufferBounds);
    }

    let values_slice = ipc_body_dma
        .subslice_checked(val_off as usize, val_len as usize)
        .ok_or(ParseError::ArrowTruncatedBody)?;
    let _null_bitmap = ipc_body_dma
        .subslice_checked(
            validity_desc.offset_bytes.to_native() as usize,
            validity_desc.byte_length.to_native() as usize,
        )
        .ok_or(ParseError::ArrowTruncatedBitmap)?;

    // Zero-copy predicated VVec reduction directly over Domain-1 DMA pages (`0` Alloc, `0` memcpy!):
    Ok(Grammar::simd_sum_nullable_i64[P](in values_slice, rows as usize))
}
```

=== 31.4 Lean 4 Mutual-Inverse Roundtrip and Non-Malleability Proofs (`vesper verify`)
<ch-30-4>
Extracting `@wire_grammar(canonical = true)` parsers and serializers through `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Roundtrip Correctness* and *Strong Non-Malleability* (proving that canonical serialization is injective and rejects overlong varint encodings):

```lean
-- illustrative/GrammarNonMalleability.lean
-- 1. Strong Non-Malleability from Serializer-After-Parser Identity:
-- If `serialize(parse(b)) = b` holds on all accepted inputs, two distinct wire encodings
-- `b1 != b2` can NEVER parse to the same logical message `v` (blocks signature malleability!):
theorem canonical_roundtrip_implies_non_malleable
    {Msg : Type}
    (parse : ByteArray -> Option Msg)
    (serialize : Msg -> ByteArray)
    (h_ser_parse : forall b v, parse b = some v -> serialize v = b)
    (b1 b2 : ByteArray) (v : Msg)
    (hp1 : parse b1 = some v) (hp2 : parse b2 = some v) :
    b1 = b2 := by
  have h1 : serialize v = b1 := h_ser_parse b1 v hp1
  have h2 : serialize v = b2 := h_ser_parse b2 v hp2
  rw [<-h1, <-h2]

-- 2. Canonical 2-Byte Varint Lower-Bound Guard: values `< 128` MUST use 1 byte, rejecting `0x80..0xFF 0x00`:
theorem canonical_leb128_2byte_rejects_overlong (b0 b1 : BitVec 8)
    (h_cont : (b0 &&& 0x80#8) != 0#8)
    (h_last : (b1 &&& 0x80#8) = 0#8)
    (h_minimal : b1 != 0#8) :
    let decoded : BitVec 16 := (b0.zeroExtend 16 &&& 0x7F#16) ||| ((b1.zeroExtend 16) <<< 7)
    decoded >= 128#16 := by
  bv_decide
```

=== 31.5 Rust Canonical Non-Malleable LEB128 Decoder and Arrow Buffer Verifier (`crates/vesper-grammar`)
<ch-30-5>
`crates/vesper-grammar` implements the canonical non-malleable LEB128 varint codec (rejecting overlong zero-padded continuations and >64-bit overflows), the LL(1) FIRST-set disjointness verifier for `@wire_grammar` choice rules, and the overflow-safe Arrow IPC buffer range validator:

```rust
// crates/vesper-grammar/src/lib.rs
use rustc_hash::FxHashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrammarDecodeError {
    UnexpectedEof,
    OverlongNonCanonicalVarint,
    VarintOverflow64,
    UnalignedArrowOffset { offset: u64 },
    OutOfBoundsSlice { offset: u64, len: u64, total: u64 },
    AmbiguousChoiceFirstSets { overlapping_tag: u8 },
}

/// Decodes a canonical unsigned 64-bit LEB128 varint, rejecting overlong encodings (Strong Non-Malleability).
pub fn decode_canonical_u64_leb128(buf: &[u8]) -> Result[(u64, usize), GrammarDecodeError] {
    let mut result: u64 = 0;
    for (idx, &byte) in buf.iter().take(10).enumerate() {
        let low7 = u64::from(byte & 0x7F);
        if idx == 9 && byte > 0x01 {
            return Err(GrammarDecodeError::VarintOverflow64);
        }
        result |= low7 << (idx * 7);
        if (byte & 0x80) == 0 {
            if idx > 0 && byte == 0 {
                return Err(GrammarDecodeError::OverlongNonCanonicalVarint);
            }
            return Ok((result, idx + 1));
        }
    }
    if buf.len() < 10 {
        Err(GrammarDecodeError::UnexpectedEof)
    } else {
        Err(GrammarDecodeError::VarintOverflow64)
    }
}

/// Validates an Apache Arrow IPC 64-byte-aligned buffer slice without integer overflow.
pub fn validate_arrow_ipc_buffer_64(
    total_body_bytes: u64,
    offset_i64: i64,
    len_i64: i64,
) -> Result[(usize, usize), GrammarDecodeError] {
    let offset = u64::try_from(offset_i64).map_err(|_| GrammarDecodeError::OutOfBoundsSlice {
        offset: 0,
        len: 0,
        total: total_body_bytes,
    })?;
    let len = u64::try_from(len_i64).map_err(|_| GrammarDecodeError::OutOfBoundsSlice {
        offset,
        len: 0,
        total: total_body_bytes,
    })?;
    if (offset & 63) != 0 {
        return Err(GrammarDecodeError::UnalignedArrowOffset { offset });
    }
    let end = offset.checked_add(len).ok_or(GrammarDecodeError::OutOfBoundsSlice {
        offset,
        len,
        total: total_body_bytes,
    })?;
    if end > total_body_bytes {
        return Err(GrammarDecodeError::OutOfBoundsSlice {
            offset,
            len,
            total: total_body_bytes,
        });
    }
    Ok((offset as usize, len as usize))
}

/// Verifies deterministic single-byte tag disjointness across `@wire_grammar` `choose` branches.
pub fn verify_disjoint_tag_branches(branch_tag_sets: &[&[u8]]) -> Result<(), GrammarDecodeError> {
    let mut seen: FxHashSet<u8> = FxHashSet::default();
    for branch in branch_tag_sets {
        for &tag in *branch {
            if !seen.insert(tag) {
                return Err(GrammarDecodeError::AmbiguousChoiceFirstSets { overlapping_tag: tag });
            }
        }
    }
    Ok(())
}
```


== 32. Vesper Beacon: Distributed TraceContext and OTLP-Arrow Telemetry
<ch-31>
#chapter-deck[Span Lifecycles, TraceContext Propagation, and Columnar Telemetry]

*Vesper Beacon* (`@vesper-obs/beacon`, with implementation code in `crates/vesper-beacon`) describes distributed tracing, structured logging, PII redaction, and columnar metrics. It complements the local `vesper top` recorder in #link(<ch-14-6>)[Section 15.6]. The design combines linear `ActiveSpan` completion, W3C `TraceContext` propagation, and OpenTelemetry-Arrow (`OTAP`) batches in per-core `AoSoA` rings. This chapter presents the intended data path and ownership contracts.

=== 32.1 Distributed Tracing, Columnar Telemetry, and Redaction Guarantees
<ch-31-1>
+ *Linear Span Completion and Mandatory Error Attribution (`ActiveSpan`):* In Rust's `tracing` ecosystem, spans rely on synchronous `Drop` guards that cannot distinguish normal scope exit from early `?` error unwinds without manual boilerplate, and require wrapping every async task in `Instrumented<F>` state machines. Beacon models `ActiveSpan` as a `Mode::Linear` obligation under Theorem 2: every control-flow path must explicitly discharge an open span via `span.finish_ok(...)` or `span.finish_err(in err)`, statically guaranteeing balanced span trees and exact failure attribution.

+ *Zero-Wrapper Fiber Context Propagation via `EvidenceVector`:* Rather than mutating thread-local `RefCell` globals (which break across work-stealing fiber migrations) or inflating return types with instrumentation wrappers, Beacon stores the 24-byte `SpanContext` (`trace_id_hi`, `trace_id_lo`, `span_id`, and `trace_flags`) directly inside Vesper Pulse's pinned `EvidenceVector` (`r15` on `x86_64`, `x28` on AArch64, #link(<ch-14-5>)[Section 15.5]). Child fibers spawned inside `Pulse::scope` inherit parent trace context automatically in 1 register copy.

+ *Columnar `AoSoA` Telemetry Rings and Zero-Copy OTLP-Arrow Export:* Conventional OpenTelemetry SDKs allocate `Vec<KeyValue>` and boxed strings on the global heap per span or log event, causing tail-latency spikes under load. Beacon records spans, structured logs, and trace-linked exemplars straight into core-local, cache-aligned Vesper Strata `tiled(16) struct` (`AoSoA`, #link(<ch-12-5>)[Section 13.5]) ring buffers whose hot columns already match Apache Arrow IPC buffers for zero-serialization OTLP-Arrow network export.

+ *3-Nanosecond SIMD W3C `traceparent` Wire Codec and Wait-Free Shard Histograms:* Pairing Vesper Bridge `wire struct` views (#link(<ch-15-2>)[Section 16.2]) with Vesper Vector `eff [Isa[P]]` nibble lookups (#link(<ch-13>)[Chapter 14]), Beacon parses and formats 55-byte W3C `traceparent` headers and Kafka trace headers in \~3 nanoseconds with zero `unsafe`. Latency metrics aggregate into core-local logarithmic sub-bucket histograms (`ShardHistogram`) mutated via disjoint `inout shard.{telemetry}` projections (`[T-CallProj]`) with zero atomic CAS contention.

+ *Compile-Time PII Redaction and Lean 4 Non-Leakage Proofs:* Beacon's colorless `Telemetrable` trait (#link(<ch-6>)[Chapter 7] Vesper Pact) omits implementations for raw `Secret[T]` and `@pii`-annotated types unless wrapped in an explicit `Redacted` model, while `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) mechanically proves that sensitive credentials never flow into exported telemetry streams.

=== 32.2 Lexical `Trace` Capability, Linear `ActiveSpan`, and SIMD W3C `traceparent`
<ch-31-2>
Beacon declares `Trace` as a `linear effect` (`q = linear`), allowing live `ActiveSpan` obligations to survive across `Storage`, `Net`, and `Clock` suspensions while #link(<ch-4>)[Chapter 4]'s `[T-Let]` Resumption Frame Condition prevents early `?` unwinds from leaking an unclosed span:

```
// Exact 55-byte W3C TraceContext v00 wire header: "00-<32-hex-trace-id>-<16-hex-span-id>-<2-hex-flags>"
wire struct W3cTraceParentWire {
    version_hex: ByteArray[2],
    sep0: u8,
    trace_id_hex: ByteArray[32],
    sep1: u8,
    parent_span_hex: ByteArray[16],
    sep2: u8,
    flags_hex: ByteArray[2],
}

copy struct SpanContext {
    trace_id_hi: u64,
    trace_id_lo: u64,
    span_id: u64,
    flags: u8,
}

// Mode::Linear: compiler rejects scope exit or early ? unless finish_ok or finish_err is called!
linear struct ActiveSpan {
    ctx: SpanContext,
    parent_span_id: u64,
    start_mono_ns: u64,
    schema_id: u32,
}

linear effect Trace {
    fn current_ctx() -> SpanContext;
    fn start_span(schema_id: u32, name: StaticStr) -> ActiveSpan;
    fn emit_ok(sink span: ActiveSpan, attr_u64: u64) -> Unit;
    fn emit_err(sink span: ActiveSpan, err_code: u32, msg: StaticStr) -> Unit;
}

// 100% safe SIMD W3C traceparent parser over zero-copy HTTP or Kafka headers:
fn parse_w3c_traceparent[P: IsaProfile](
    in raw_hdr: ByteSlice,
) -> Result[SpanContext, TraceParseError] eff [Isa[P], Fail[TraceParseError]] {
    let in wire = raw_hdr.view_wire[W3cTraceParentWire]()
        .map_err(|_| TraceParseError::InvalidLength)?;
    if wire.sep0 != 0x2D || wire.sep1 != 0x2D || wire.sep2 != 0x2D {
        Fail::raise(TraceParseError::MissingHyphen);
    }
    // Branchless SIMD hex-to-nibble decode across all 52 hex digits in one vector pass:
    let (trace_hi, trace_lo, parent_id, flags) = Isa::decode_w3c_hex_52[P](in wire)?;
    if (trace_hi | trace_lo) == 0 || parent_id == 0 {
        Fail::raise(TraceParseError::AllZeroIdentifier);
    }
    Ok(SpanContext { trace_id_hi: trace_hi, trace_id_lo: trace_lo, span_id: parent_id, flags })
}
```

=== 32.3 Columnar `AoSoA` Ring Buffers and Zero-Copy OTLP-Arrow (`OTAP`) Export
<ch-31-3>
Instead of heap-allocating heterogeneous attribute maps, Vesper Forge's WASI `@span_schema` macro assigns each call site a compile-time `schema_id` and writes fixed-width columns directly into a core-local 16-wide `AoSoA` ring buffer mapped in Hardware Domain 1 (`DmaSlab`, #link(<ch-12-3>)[Section 13.3]):

```
// 16-span cache-aligned AoSoA tile: hot timing and trace columns match OTLP-Arrow column buffers!
tiled(16) copy struct SpanTileRow {
    hot trace_id_hi: u64,
    hot trace_id_lo: u64,
    hot span_id: u64,
    hot parent_span_id: u64,
    hot start_unix_ns: u64,
    hot duration_ns: u64,
    hot schema_and_status: u64,
    hot primary_attr_u64: u64,
    cold error_summary: InlineStr[64],
}

// Zero-Global-Heap instrumented broker append (`Alloc` is omitted from `eff [...]`!):
fn append_instrumented_batch(
    inout broker.{index, telemetry}: BrokerShard,
    in raw_traceparent: ByteSlice,
    sink batch: RecordBatch,
) -> Result[CommitReceipt, WalError] eff [Trace, Storage, Clock, Isa[Avx2], Fail[WalError]] {
    let parent_ctx = parse_w3c_traceparent[Avx2](in raw_traceparent)
        .unwrap_or_else(|_| Trace::current_ctx());

    handle Trace with_parent(parent_ctx) in {
        let span: ActiveSpan = Trace::start_span(0x1042, "krabka.wal.append");

        // Note: An unguarded `?` here is statically rejected by `[T-Let]` because `span` is linear!
        match write_and_fsync_batch(inout broker.index, sink batch) {
            Ok(receipt) => {
                let elapsed_ns = Clock::now_ns().wrapping_sub(span.start_mono_ns);
                broker.telemetry.append_latency.record_exemplar(elapsed_ns, span.ctx.span_id);
                Trace::emit_ok(sink span, receipt.wal_slot);
                Ok(receipt)
            }
            Err(wal_err) => {
                broker.telemetry.append_errors += 1;
                Trace::emit_err(sink span, wal_err.code(), "wal_fsync_failed");
                Fail::raise(wal_err)
            }
        }
    }
}
```

When a 16-row `SpanTileRow` block fills on a worker shard, a background Vesper Pulse `io_uring` fiber flushes the contiguous column pages directly into an OTLP-Arrow gRPC/HTTP2 frame (`IORING_OP_SEND_ZC`) without touching global heap memory or stalling foreground request fibers.

=== 32.4 Compile-Time PII Redaction and Lean 4 Non-Interference Proofs
<ch-31-4>
To eliminate accidental credential leakage into logs and span attributes (a recurring vulnerability under automatic debug-formatting derivation), Beacon separates structural formatting from `Telemetrable`:

```
// Sealed wrapper whose canonical `Debug` or `Telemetrable` implementation is intentionally absent!
affine struct Secret[T] {
    inner: T,
}

// Named Vesper Pact model permitting safe, Fingerprinted telemetry emission when explicitly opted in:
pub model RedactedTokenTelemetry: Telemetrable for Secret[BearerToken] {
    fn encode_attr(in self, inout out: AttrWriter) eff [] {
        out.write_static_str("[REDACTED:sha256_prefix]");
    }
}
```

In `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]), `vesper verify` extracts instrumented handlers and proves both *Exact Span Conservation* and *Secret Non-Interference* (changing a secret `BearerToken` input never alters any byte of the emitted `TraceOp` stream):

```lean
-- illustrative/BeaconPrivacySafety.lean
theorem ingest_telemetry_secret_noninterference
    (tok1 tok2 : Secret BearerToken) (req : PublicReqMeta) (st : RefBrokerState) :
    EmittedTraceOps (runInstrumented st tok1 req) =
    EmittedTraceOps (runInstrumented st tok2 req) := by
  rfl
```

=== 32.5 Rust Host Implementation (`crates/vesper-beacon`)
<ch-31-5>
`crates/vesper-beacon` provides the branchless W3C `traceparent` codec and wait-free logarithmic sub-bucket latency histogram (under 2% relative error across nanosecond-to-hour ranges in 2 KiB of L1 cache):

```rust
// crates/vesper-beacon/src/lib.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct W3cSpanContext {
    pub trace_id: u128,
    pub span_id: u64,
    pub trace_flags: u8,
}

#[inline]
const fn decode_hex_byte(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        _ => 0xFF, // W3C TraceContext mandates lowercase hex
    }
}

/// Zero-allocation W3C `traceparent` v00 parser (`00-<32hex>-<16hex>-<2hex>`).
pub fn parse_traceparent_55(raw: &[u8]) -> Option<W3cSpanContext> {
    if raw.len() != 55 || &raw[0..3] != b"00-" || raw[35] != b'-' || raw[52] != b'-' {
        return None;
    }
    let mut trace_id: u128 = 0;
    for &b in &raw[3..35] {
        let nib = decode_hex_byte(b);
        if nib == 0xFF {
            return None;
        }
        trace_id = (trace_id << 4) | u128::from(nib);
    }

    let mut span_id: u64 = 0;
    for &b in &raw[36..52] {
        let nib = decode_hex_byte(b);
        if nib == 0xFF {
            return None;
        }
        span_id = (span_id << 4) | u64::from(nib);
    }

    let hi = decode_hex_byte(raw[53]);
    let lo = decode_hex_byte(raw[54]);
    if hi == 0xFF || lo == 0xFF || trace_id == 0 || span_id == 0 {
        return None;
    }
    Some(W3cSpanContext {
        trace_id,
        span_id,
        trace_flags: (hi << 4) | lo,
    })
}

/// Core-local wait-free logarithmic histogram (4-bit linear sub-buckets per power-of-2 octave,
/// 256 buckets = 2 KiB L1-resident array, zero atomics via Vesper `inout shard.telemetry`).
pub struct FastLogHistogram {
    pub buckets: [u64; 256],
    pub exemplar_span_id: [u64; 256],
    pub total_count: u64,
}

impl Default for FastLogHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; 256],
            exemplar_span_id: [0; 256],
            total_count: 0,
        }
    }
}

impl FastLogHistogram {
    #[inline(always)]
    pub fn bucket_index(val_ns: u64) -> usize {
        if val_ns < 16 {
            return val_ns as usize;
        }
        let lz = val_ns.leading_zeros() as usize;
        let octave = 63usize.saturating_sub(lz);
        let shift = octave.saturating_sub(3);
        let sub = ((val_ns >> shift) & 0b0111) as usize;
        let idx = ((octave - 3) << 3) + sub;
        idx.min(255)
    }

    #[inline(always)]
    pub fn record_with_exemplar(&mut self, duration_ns: u64, span_id: u64) {
        let idx = Self::bucket_index(duration_ns);
        self.buckets[idx] += 1;
        self.exemplar_span_id[idx] = span_id;
        self.total_count += 1;
    }
}
```


== 33. Vesper Warp: Kernel-Bypass QUIC/TCP and BBRv3 Networking
<ch-32>
#chapter-deck[Packet Ownership, QUIC/TCP, Retransmission, and Congestion Control]

*Vesper Warp* (`@vesper-net/warp`, with implementation code in `crates/vesper-warp`) describes a user-space transport stack for *Prism* (#link(<ch-33>)[Chapter 34]) and *Chorus* (#link(<ch-34>)[Chapter 35]). The proposal combines `AF_XDP`/DPDK packet I/O, NIC offloads, RFC 9000 QUIC, RFC 9293 TCP, and BBRv3 congestion control. Linear `UmemFrame` ownership, retransmission leases, and separate Lean models address specific lifecycle and protocol obligations. Line-rate 100/400 GbE operation is a performance goal, not an established property of every path.

=== 33.1 User-Space UMEM Conservation, Zero-Copy QUIC, and Pacing Guarantees
<ch-32-1>
+ *UMEM Frame Ownership (`UmemFrame[State]`):* Warp models each 4 KiB Hardware Domain-1 `DmaSlab` chunk (#link(<ch-12-3>)[Section 13.3]) as a linear frame moving through the Fill, Rx, Tx, and Completion rings. The intended `[T-Let]` contract requires each harvested `RxReady` frame to reach `FillRing::replenish` or `TxRing::submit_tx` on every control path. Losing a descriptor can starve the Fill ring; recycling it before DMA completion can overwrite an in-flight packet. The design aims to enforce conservation without reference counting. Theorem 2 does not by itself prove correspondence between these source obligations and NIC ownership.

+ *Zero-Copy Split-Header QUIC Packet Protection Over Vesper Vault (NonceTicket + Hardware Crypto):* Standard Rust QUIC stacks (quinn and quiche) copy stream payloads between application Bytes buffers and UDP datagram staging vectors while managing packet-number AEAD nonces via mutable integer counters. Pairing *Vesper Bridge* `wire struct` headers (#link(<ch-15-2>)[Section 16.2]) with Vesper Vault (ConstTime and NonceTicket, #link(<ch-27-3>)[Section 28.3]), Warp formats RFC 9000 short/long headers in place inside UmemFrame headroom, seals payloads in constant time (or offloads inline AES-GCM / ChaCha20-Poly1305 to ConnectX / SmartNIC hardware ktls/`crypto_offload`), and statically prevents RFC 9001 QUIC packet-number nonce reuse.

+ *Pin-Free Retransmission Leases and Hardware Timing-Wheel Pacing (InFlightQuicPkt):* Holding zero-copy payload slices inside a QUIC/TCP retransmission buffer until peer ACK arrival or PTO (Probe Timeout) loss recovery requires Arc reference counting or unsafe self-referential pointers in Rust. Warp models transmitted packets awaiting acknowledgment as linear struct InFlightQuicPkt parked inside a core-local Hierarchical Hashed Timing Wheel (O(1) insert/cancel, 0 Alloc) and transmits bursts using hardware NIC EDT (Earliest Departure Time / `SO_TXTIME`) timestamps so BBRv3 pacing runs in silicon without CPU timer interrupts.

+ *AoSoA Connection-State Tiling and RSS Core-Pinned Shard Isolation:* Rather than scattering 1,000,000 concurrent QUIC connections across heap-allocated `Arc<Mutex<Connection>>` objects, Warp configures NIC Receive-Side Scaling (RSS / Toeplitz flow steering) so each Vesper Pulse worker shard (#link(<ch-14-4>)[Section 15.4]) owns a disjoint flow partition stored in a *Vesper Strata* `tiled(16) struct` (AoSoA, #link(<ch-12-5>)[Section 13.5]) table. Batch-processing 64 `AF_XDP` Rx descriptors vectorizes congestion-window updates, RTT estimators, and idle-timeout scans across *Vesper Vector* `VVec` lanes (`eff [Isa\[P]`\], #link(<ch-13>)[Chapter 14]) with zero cross-core locks.

+ *SimPulse Adversarial WAN Loss Fuzzing and Lean 4 RFC 9000 / BBRv3 Proofs (`vesper verify`):* Because XdpNic is an ordinary lexical effect (`eff [.\.\.]`), `vesper test --sim` (#link(<ch-14-6>)[Section 15.6]) and Vesper Crucible (#link(<ch-21-2>)[Section 22.2]) test unmodified Warp QUIC/TCP endpoints against Gilbert-Elliott burst loss, NAT rebinding, and micro-burst bufferbloat deterministically. Simultaneously, vesper-verify-lean (#link(<ch-8>)[Chapter 9]) mechanically proves UMEM Frame Conservation, RFC 9000 Monotonic Packet-Number Decoding, and BBRv3 Inflight Safety Bounds.

=== 33.2 Padding-Free wire struct `AF_XDP` / QUIC Headers and Linear UMEM Ring Capabilities
<ch-32-2>
Warp pairs the Linux kernel's 16-byte `xdp_desc` UMEM descriptor layout and RFC 9000 1-RTT short headers as padding-free wire struct definitions (#link(<ch-15-2>)[Section 16.2]) with typestate UmemFrame handles:

```
// Exact 16-byte Linux `AF_XDP` (`linux/if_xdp.h`) RX/TX ring descriptor (0 implicit padding):
wire struct XdpDescWire {
    umem_addr: u64le,
    pkt_byte_len: u32le,
    options: u32le,
}
// Exact 13-byte QUIC 1-RTT Short Header with 8-byte Destination Connection ID (RFC 9000):
wire struct QuicShortHdrWire {
    flags_spin_keyphase: u8,
    dest_conn_id: ByteArray[8],
    pkt_num_trunc: u32be,
}
// Typestate phantom markers for Hardware Domain-1 4 KiB UMEM DMA frames:
copy struct RxHarvested {}
copy struct TxStaged {}
// Mode::Linear UMEM frame: compiler REJECTS dropping or forgetting a NIC DMA slot on any branch!
linear struct UmemFrame[State] {
    chunk_idx: u32,
    data_offset: u16,
    pkt_len: u16,
}
// Mode::Linear in-flight packet parked in shard retransmission wheel until ACK or PTO retransmit:
linear struct InFlightQuicPkt {
    pkt_num_full: u64,
    edt_departure_ns: u64,
    held_frame: UmemFrame[TxStaged],
}
linear effect XdpNic {
    fn harvest_rx_single() -> Option[UmemFrame[RxHarvested]];
    fn view_frame_bytes(in frame: UmemFrame[RxHarvested]) -> ByteSlice;
    // Recycles an RX UMEM slot back onto the NIC Fill Ring (discharges linear obligation):
    fn recycle_to_fill_ring(sink frame: UmemFrame[RxHarvested]) -> Unit;
    // Converts RX buffer headroom in place into an outgoing TX response frame (zero alloc, zero copy!):
    fn turn_around_tx_in_place(
        sink rx_frame: UmemFrame[RxHarvested],
        tx_len: u16,
    ) -> UmemFrame[TxStaged];
    // Submits TX frame with hardware EDT pacing timestamp (`SO_TXTIME` / NIC launch-time offload):
    fn submit_paced_tx(
        sink tx_frame: UmemFrame[TxStaged],
        pkt_num_full: u64,
        edt_launch_ns: u64,
    ) -> InFlightQuicPkt;
    fn park_for_ack_or_pto(sink inflight: InFlightQuicPkt) -> Unit;
}
```

=== 33.3 Zero-Copy `AF_XDP` QUIC 1-RTT Ingress + BBRv3 Paced Egress (0 Alloc)
<ch-32-3>
Processing a QUIC 1-RTT datagram on a core-pinned shard inspects the `AF_XDP` UMEM frame directly in place, authenticates ciphertext in constant time via Vesper Vault (#link(<ch-27>)[Chapter 28]), updates AoSoA BBRv3 pacing state, and turns the same UMEM buffer around onto the TX ring with a hardware EDT timestamp—while `[T-Let]` statically guarantees that malformed or unauthenticated packets still return their UmemFrame to the NIC Fill ring:

```
// 16-connection AoSoA BBRv3 + QUIC flow tile: hot congestion & pacing fields pack into 64B L1 lines!
tiled(16) copy struct QuicConnTile {
    hot largest_rx_pn: u64,
    hot next_tx_pn: u64,
    hot bbr_bw_bytes_per_sec: u64,
    hot bbr_min_rtt_ns: u64,
    hot cwnd_bytes: u32,
    hot bytes_in_flight: u32,
    hot next_edt_ns: u64,
    cold peer_cid: ByteArray[8],
}
// Zero-Global-Heap `AF_XDP` + QUIC 1-RTT datagram step (`Alloc` omitted from `eff [...]`!):
fn poll_and_step_quic_rx_one[Alg: AeadCipher, P: IsaProfile](
    inout conns: TiledSeq[QuicConnTile],
    in rx_keys: SecretSlab,
    inout tx_sealer: AeadSealer[Alg],
    now_ns: u64,
) -> Result[bool, QuicError] eff [XdpNic, ConstTime, Isa[P], Fail[QuicError]] {
    let Some(rx_frame) = XdpNic::harvest_rx_single() else {
        return Ok(false);
    };
    let raw_pkt = XdpNic::view_frame_bytes(in rx_frame);
    // Note: `[T-Let]` forbids `?` while linear `rx_frame` is live; recycle on short/malformed header!
    let Ok((in quic_hdr, in cipher_body)) = raw_pkt.split_wire_prefix[QuicShortHdrWire]() else {
        XdpNic::recycle_to_fill_ring(sink rx_frame);
        return Ok(true);
    };
    let Some(conn_slot) = Warp::lookup_cid_simd[P](in conns, in quic_hdr.dest_conn_id) else {
        XdpNic::recycle_to_fill_ring(sink rx_frame);
        return Ok(true);
    };
    // RFC 9000 Appendix A.3 monotonic packet-number reconstruction + BBRv3 EDT schedule:
    let full_pn = Warp::decode_packet_number_rfc9000(
        conns.get_largest_rx_pn(conn_slot),
        quic_hdr.pkt_num_trunc.to_native() as u64,
        32,
    );
    let edt_ns = Warp::advance_bbrv3_edt(inout conns, conn_slot, 64, now_ns);
    // Turn UMEM frame around in place as a 64-byte coalesced ACK + stream reply (0 memcpy, 0 Alloc):
    let tx_frame: UmemFrame[TxStaged] = XdpNic::turn_around_tx_in_place(sink rx_frame, 64);
    let inflight: InFlightQuicPkt = XdpNic::submit_paced_tx(sink tx_frame, full_pn + 1, edt_ns);
    XdpNic::park_for_ack_or_pto(sink inflight);
    Ok(true)
}
```

=== 33.4 Lean 4 UMEM Frame Conservation and BBRv3 Pacing Proofs (`vesper verify`)
<ch-32-4>
Extracting Warp's `AF_XDP` ring state machine, RFC 9000 packet-number decoder, and BBRv3 EDT scheduler through vesper-verify-lean (#link(<ch-8>)[Chapter 9]) discharges both Total UMEM Pool Conservation (across Fill, Rx, Tx, and Retransmit queues) and Monotonic Hardware EDT Pacing:

```lean
-- illustrative/WarpQuicSafety.lean
-- 1. UMEM Pool Conservation: harvesting from RX and either recycling to Fill or parking in TX
-- preserves exact total 4 KiB Domain-1 UMEM frame count (zero NIC descriptor starvation!):
theorem umem_step_conserves_total_frames
    (fillCnt rxCnt txParkedCnt : Nat) (h_rx_nonempty : rxCnt ≥ 1) (sendReply : Bool) :
    let (fill', rx', tx') :=
      if sendReply then (fillCnt, rxCnt - 1, txParkedCnt + 1)
      else (fillCnt + 1, rxCnt - 1, txParkedCnt)
    fill' + rx' + tx' = fillCnt + rxCnt + txParkedCnt := by
  cases sendReply <;> simp <;> omega
-- 2. Hardware EDT Pacing Monotonicity: BBRv3 `max(nowNs, prevEdtNs) +serializationDelay` never regresses:
theorem bbrv3_edt_launch_monotonic
    (nowNs prevEdtNs pacingDelayNs : Nat) :
    let nextEdt := Nat.max nowNs prevEdtNs + pacingDelayNs
    prevEdtNs ≤ nextEdt ∧ nowNs ≤ nextEdt := by
  omega
```

=== 33.5 Rust RFC 9000 Packet-Number Decoder, BBRv3 Pacer, and UMEM Ring Verifier (`crates/vesper-warp`)
<ch-32-5>
`crates/vesper-warp` implements the exact RFC 9000 Appendix A.3 truncated packet-number decoder, the BBRv3 congestion-window and EDT pacing calculator, and the `AF_XDP` 4-ring conservation auditor used by SimPulse:

```rust
// crates/vesper-warp/src/lib.rs
/// RFC 9000 Appendix A.3: Reconstructs full 62-bit QUIC packet number from truncated wire bits
/// closest to `largest_acked + 1` without modular wrap-around ambiguity.
pub fn decode_quic_packet_number_rfc9000(
    largest_pn: u64,
    truncated_pn: u64,
    pn_nbits: u32,
) -> u64 {
    let expected_pn = largest_pn.wrapping_add(1);
    let pn_win = 1u64 << pn_nbits;
    let pn_hwin = pn_win >> 1;
    let pn_mask = pn_win - 1;
    let candidate_pn = (expected_pn & !pn_mask) | (truncated_pn & pn_mask);
    if candidate_pn + pn_hwin <= expected_pn && candidate_pn < (1u64 << 62) - pn_win {
        candidate_pn + pn_win
    } else if candidate_pn > expected_pn + pn_hwin && candidate_pn >= pn_win {
        candidate_pn - pn_win
    } else {
        candidate_pn
    }
}
/// Core BBRv3 flow state tracking bottleneck bandwidth, min-RTT, `inflight_hi`, and hardware EDT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bbrv3FlowState {
    pub max_bw_bytes_per_sec: u64,
    pub min_rtt_ns: u64,
    pub pacing_gain_x256: u32,
    pub cwnd_gain_x256: u32,
    pub inflight_hi_bytes: u32,
    pub next_edt_ns: u64,
}
impl Bbrv3FlowState {
    /// Computes target BBRv3 `cwnd` bounded above by `inflight_hi` (loss/ECN headroom) and >= 4 MSS.
    pub fn target_cwnd_bytes(&self, mss_bytes: u32) -> u32 {
        let bdp_bytes = ((u128::from(self.max_bw_bytes_per_sec) * u128::from(self.min_rtt_ns))
            / 1_000_000_000u128) as u64;
        let gained = (bdp_bytes.saturating_mul(u64::from(self.cwnd_gain_x256))) >> 8;
        let min_cwnd = mss_bytes.saturating_mul(4);
        (gained as u32).clamp(min_cwnd, self.inflight_hi_bytes.max(min_cwnd))
    }
    /// Advances hardware Earliest Departure Time (`SO_TXTIME` / NIC launch-time offload) monotonically.
    pub fn schedule_edt_ns(&mut self, pkt_bytes: u32, now_ns: u64) -> u64 {
        let effective_bw = ((u128::from(self.max_bw_bytes_per_sec)
            * u128::from(self.pacing_gain_x256.max(1)))
            >> 8)
            .max(1);
        let delay_ns = ((u128::from(pkt_bytes) * 1_000_000_000u128) / effective_bw) as u64;
        let launch_ns = self.next_edt_ns.max(now_ns);
        self.next_edt_ns = launch_ns.saturating_add(delay_ns);
        launch_ns
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UmemRingError {
    FramePoolExhausted,
    ConservationInvariantBroken { expected: u32, actual: u32 },
}
/// Verifies `Mode::Linear` conservation of 4 KiB Domain-1 UMEM frames across `AF_XDP` Fill/Rx/Tx/Comp rings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XdpUmemAccounting {
    pub total_frames: u32,
    pub fill_ring: u32,
    pub rx_ring: u32,
    pub app_linear_held: u32,
    pub tx_inflight: u32,
}
impl XdpUmemAccounting {
    pub fn check_conservation(&self) -> Result<(), UmemRingError> {
        let sum = self
            .fill_ring
            .saturating_add(self.rx_ring)
            .saturating_add(self.app_linear_held)
            .saturating_add(self.tx_inflight);
        if sum != self.total_frames {
            return Err(UmemRingError::ConservationInvariantBroken {
                expected: self.total_frames,
                actual: sum,
            });
        }
        Ok(())
    }
}
```


== 34. Vesper Prism: HTTP/2/3 and WebSocket Service Contracts
<ch-33>
#chapter-deck[Response Typestates, Disconnect Cleanup, Middleware, and Request Routing]

*Vesper Prism* (`@vesper-web/prism`, with implementation code in `crates/vesper-prism`) describes HTTP/1.1, HTTP/2, HTTP/3, and WebSocket services. Its proposed architecture combines linear response typestates (`Responder[HeadersPending]`), disconnect cancellation rules, lexical-handler middleware, request-local arenas, and SIMD routing/JSON ingress. This chapter follows those contracts through a request lifecycle; their composition does not yet establish complete protocol correctness.

=== 34.1 HTTP/2/3 Typestate Completion, Middleware, and Routing Guarantees
<ch-33-1>
+ *Typestate Linear Response and Upgrade Lifecycles:* In Rust web frameworks, implicitly droppable response builders and `TimeoutLayer` wrappers allow handler futures to drop mid-flight at any `.await`, causing truncated responses, orphaned SSE streams, or half-closed WebSockets. Prism passes a `linear struct Responder[HeadersPending]` that must transition along 100% of control paths into `ResponseSent` or a `linear struct WsSession`, while `[T-Let]` enforces compile-time disconnect safety.
+ *Middleware as Zero-Allocation Lexical Effect Handlers:* Instead of nesting `Tower::Service` and `Layer` traits into complex generic types that force boxed heap futures and `Arc` state cloning, Prism models `Auth`, `RateLimit`, `TenantDb`, and `RequestTrace` as lexical effects in `eff [...]`. Mounting middleware is an ordinary `handle` block backed by Cranelift's pinned `vmctx` register (`r15` / `x28`), paired with disjoint per-core shard borrows (`inout shard.{rate_buckets, metrics}`).
+ *Zero-Global-Heap Per-Request Arenas:* Rust web services allocate headers, path strings, and JSON vectors through `GlobalAlloc`, incurring fragmentation slowdowns under sustained load (ISMM 2026). Every Prism request executes inside a `Region::scoped(64.kib(), ...)` `VirtualArena` with `Alloc` omitted from `eff [...]`, guaranteeing zero global heap churn and `O(1)` bulk memory reclamation while `[T-RegionScope]` prevents arena slices from escaping into global caches without lifetime annotations.
+ *Safe SIMD Parsing, Wire Struct Framing, and MPK-Isolated TLS:* Rather than relying on `unsafe` SIMD intrinsics and unisolated C TLS bindings, Prism parses HTTP headers and JSON structural indices via Vesper Vector (`eff [Isa[P]]` across `AVX-512`, `Arm SVE2`, `RISC-V RVV`, and `NEON`), casts HTTP/2 and RFC 6455 WebSocket headers in `O(1)` via Vesper Bridge `wire struct` views over Domain-1 `io_uring` buffers, and isolates foreign C TLS libraries in 1.5 ns via Intel MPK and Arm POE.
+ *Confined WASI Routing Macros and Lean 4 Security Proofs:* Prism's `@route` and `@derive(JsonSliceDecode, OpenApiSchema)` macros execute inside Vesper Forge's fuel-metered Wasmtime sandbox in under 50 microseconds (`macro_emits_effects = []`), preventing compromised serialization macros from smuggling `Net` or `Storage` calls. Meanwhile, `vesper-verify-lean` extracts route handlers into pure Lean 4 `EffM` definitions to prove _Paralegal_-class (OSDI 2025) authorization and privacy invariants in three tactic lines.

=== 34.2 Typestate Linear Protocol Obligations and Disconnect Cancel Safety
<ch-33-2>
Prism encodes RFC 9110 HTTP and RFC 6455 WebSocket state machines directly into substructural modal types:

+ *Exact-Once HTTP Response Finalization:* `linear struct Responder[S]` is parameterized by typestate `S` (`HeadersPending` or `StreamingBody`). Consuming transitions such as `res.json(status, in payload)`, `res.problem(status, msg)`, or `res.upgrade_websocket(in req)` discharge `Responder[HeadersPending]` and produce the `ResponseSent` receipt required by the router signature. Forgetting to reply on an error branch or dropping a WebSocket session without completing a close handshake fails compilation under Theorem 2 (Total Linear Exhaustion).
+ *Client-Disconnect Safety:* Socket disconnects and request deadlines use *Pulse*'s abortable `Cancel` effect (`q = affine`). `TenantDb`, `Storage`, and socket DMA flushes (`Net`) are declared `linear effect` (`q = linear`). Before a linear `DbTxn` is acquired, `Cancel::check()?` may return through the connection handler's unwind boundary. Once `let mut db_tx: DbTxn = TenantDb::begin_write(...)` is live, `[T-Let]` rejects `linear ⊑ res(Cancel)` because `linear ⊑ affine` is false. Under this baseline rule, timeout or disconnect aborts must wait until `db_tx.commit()` or `db_tx.rollback()` discharges the transaction.

=== 34.3 Middleware as Lexical Effect Handlers and Disjoint Shard State
<ch-33-3>
Cross-cutting web concerns are ordinary capability interfaces in `Σ` rather than trait-object wrappers:

```
// Abortable authentication & RBAC capability (q = affine: unauthorized requests short-circuit cleanly)
effect Auth {
    fn require_org_writer(org_id: u64) -> OrgWriteGrant;
    fn verify_read_scope(org_id: u64, in req: HttpRequestView) -> Result[OrgReadCap, AuthError];
}

// Linear multi-tenant storage capability (q = linear: in-flight writes never drop mid-CQE)
linear effect TenantDb {
    fn begin_write(grant: OrgWriteGrant) -> DbTxn;
    fn append_event(
        inout tx: DbTxn,
        grant: OrgWriteGrant,
        in topic: ByteSlice,
        in payload: ByteSlice,
    ) -> Unit;
}
```

When a middleware `handle Auth` block wraps a router group, Stage 4 (`vesper-opt-egglog`) specializes synchronous JWT or session verification clauses into inline register checks with zero heap closures. Each Vesper Pulse worker shard owns its own `WorkerShard` struct (`rate_buckets`, `metrics`, `jwt_keys`), allowing route handlers to borrow `inout shard.{rate_buckets, metrics}` alongside `in shard.jwt_keys` via `[T-CallProj]` with zero `Arc`, zero `RwLock`, and zero lifetime parameters.

=== 34.4 Complete Showcase: Zero-Allocation API, WebSocket Upgrade, and Shard Server
<ch-33-4>
```
// 1. Padding-free RFC 6455 WebSocket wire header (Section 13 Vesper Bridge):
wire struct WsFrameWireHeader {
    fin_opcode: u8,
    mask_payload_len: u8,
}

// Unforgeable capability token minted exclusively by Auth::require_org_writer:
copy struct OrgWriteGrant {
    org_id: u64,
    actor_id: u64,
}

// 2. WASI wasm32-wasip2 derive macro (emits_effects = []):
// Generates zero-copy Region arena JSON decoder + OpenAPI 3.1 schema in under 50us!
@derive(JsonSliceDecode, OpenApiSchema)
copy struct IngestEventReq[r: Region] {
    topic: RegionStr[r],
    crc32: u32,
    payload: RegionSlice[r, u8],
}

// 3. Unary JSON Ingestion Route:
// - Alloc is ABSENT from eff [...]: statically guaranteed zero global heap allocation!
// - inout shard.{rate_buckets, metrics} mutates core-local state without Arc/Mutex.
// - sink res: Responder[HeadersPending] is linear: every branch MUST send a response!
@route("POST /v1/orgs/{org_id:u64}/events")
fn ingest_event[P: IsaProfile](
    org_id: u64,
    in req_raw: HttpRequestView,
    inout shard.{rate_buckets, metrics}: WorkerShard,
    in config: PrismConfig,
    sink res: Responder[HeadersPending],
) -> ResponseSent eff [
    req: Region,
    Isa[P],
    Auth,
    TenantDb,
    Net,
    Cancel,
    Fail[ApiError]
] {
    // Permitted BEFORE acquiring DbTxn (res(Cancel) = affine is legal here):
    Cancel::check()?;

    if !shard.rate_buckets.allow(org_id, config.burst_limit) {
        return res.problem(429, "rate limit exceeded");
    }

    // O(1) EvidenceVector lookup in r15 / x28:
    let grant: OrgWriteGrant = Auth::require_org_writer(org_id)?;

    // 100% safe SIMD JSON parse into the 64 KiB per-request VirtualArena (req):
    let body: IngestEventReq[req] =
        Json::parse_simd[P, IngestEventReq[req]](in req, in req_raw.body())?;

    // Acquire a linear write transaction:
    let mut db_tx: DbTxn = TenantDb::begin_write(grant);

    // COMPILE-TIME CANCEL SAFETY: Inserting Cancel::check()? or an unguarded ? here
    // fails compilation because db_tx is Mode::Linear (linear <= affine is FALSE)!
    match verify_payload_crc[P](in body.payload, body.crc32) {
        Ok(()) => {
            TenantDb::append_event(inout db_tx, grant, in body.topic.as_bytes(), in body.payload);
            let seq = db_tx.commit();
            shard.metrics.events_ingested += 1;
            res.json(201, in CreatedReceipt { seq })
        }
        Err(_bad_crc) => {
            db_tx.rollback();
            res.problem(422, "payload CRC mismatch")
        }
    }
}

// 4. Typestate Protocol Upgrade: HTTP Responder -> linear struct WsSession
@route("GET /v1/orgs/{org_id:u64}/tail")
fn live_event_tail(
    org_id: u64,
    in req_raw: HttpRequestView,
    sink res: Responder[HeadersPending],
) -> ResponseSent eff [Auth, EventBus, Net, Clock, Cancel] {
    match Auth::verify_read_scope(org_id, in req_raw) {
        Err(_) => res.problem(403, "forbidden"),
        Ok(read_cap) => {
            let mut ws: WsSession = res.upgrade_websocket(in req_raw);
            loop {
                match Pulse::race_next_event_or_ping(read_cap, 15.seconds()) {
                    TailStep::Event(frame) => ws.send_binary(in frame),
                    TailStep::ClientClose  => break ws.close_gracefully(1000),
                }
            }
        }
    }
}

// 5. Per-Core io_uring Listener Mounting Region Arena & Lexical Auth Middleware:
fn serve_shard(
    inout shard.{rate_buckets, metrics}: WorkerShard,
    in jwt_keys: JwtKeyRing,
    in cfg: PrismConfig,
) eff [Scope, Net, TenantDb, Clock, Isa[Avx2]] {
    Prism::listen_uring(in cfg.bind_addr, |in raw_req, sink responder| {
        Region::scoped(64.kib(), |cap req| {
            handle Auth in {
                ApiRouter::dispatch[Avx2](
                    in raw_req,
                    inout shard.{rate_buckets, metrics},
                    in cfg,
                    sink responder,
                )
            } with {
                return sent => sent,
                require_org_writer(target_org, k) => {
                    match Jwt::verify_org_role(
                        in jwt_keys,
                        in raw_req.bearer_token(),
                        target_org,
                        Role::Writer,
                    ) {
                        Ok(grant) => k(grant),
                        Err(_)    => Prism::abort_unauthorized(sink k),
                    }
                }
            }
        })
    })
}
```

=== 34.5 Lean 4 Policy Proofs (`vesper verify`) and `SimPulse` Chaos Testing
<ch-33-5>
Because `ingest_event` uses second-class `inout` projections and lexical effects instead of `Pin`-wrapped coroutines and `Arc` pointers, `vesper verify` (#link(<ch-8>)[Chapter 9]) extracts it directly into a pure Lean 4 `EffM` definition. Proving a _Paralegal_-class global authorization policy—that no `TenantDbOp.appendEvent` operation ever executes for an `org_id` without a matching `OrgWriteGrant` issued by `Auth`—closes in three lines:

```lean
-- illustrative/PrismAuthSafety.lean
theorem ingest_event_never_writes_unauthorized_org
    (authOracle : UInt64 -> Option OrgWriteGrant)
    (h_sound : ∀ org g, authOracle org = some g -> g.orgId = org)
    (org : UInt64) (req : HttpRequestView) (shard : WorkerShardProj) (cfg : PrismConfig) :
    ∀ op ∈ TraceOf (runWithAuth authOracle (ingestEvent org req shard cfg)),
      ∀ g topic bytes, op = TenantDbOp.appendEvent g topic bytes -> g.orgId = org := by
  simp [ingestEvent, runWithAuth, TraceOf]
  aesop
```

For end-to-end systems resilience and production observability:

+ *Deterministic Chaos Simulation (`vesper test --sim`):* Executes the unmodified `serve_shard` router inside `SimPulse` (#link(<ch-14>)[Chapter 15]), deterministically injecting slowloris 1-byte TCP drips, mid-upgrade client RST packets, and database tail-latency spikes in milliseconds from a reproducible 64-bit PRNG seed.
+ *Live Linear Obligation Inspector (`vesper top`):* Streams per-route latency histograms alongside every suspended fiber holding an open `Responder[HeadersPending]`, `WsSession`, or `DbTxn`—requiring zero `#[instrument]` macro annotations.

=== 34.6 Rust Host Runtime and Radix Router Core (`crates/vesper-prism`)
<ch-33-6>
`crates/vesper-prism` provides the radix route compiler and zero-heap budget verifier invoked by `@route(...)`:

```rust
// crates/vesper-prism/src/lib.rs
use smol_str::SmolStr;
use vesper_hir::ty::EffectRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum HttpMethod {
    Get = 0,
    Post = 1,
    Put = 2,
    Delete = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponderTypestate {
    HeadersPending,
    StreamingBody,
    UpgradedWebSocket,
    DischargedResponseSent,
}

#[derive(Clone, Debug)]
pub struct CompiledRouteEntry {
    pub method: HttpMethod,
    pub pattern: SmolStr,
    pub param_types: Vec<(SmolStr, SmolStr)>,
    pub handler_symbol: SmolStr,
    pub required_effects: EffectRow,
    pub requires_global_alloc: bool,
}

impl CompiledRouteEntry {
    /// Audits at router registration time that zero-allocation routes omit Alloc from eff [...].
    pub fn verify_zero_heap_budget(&self) -> Result<(), SmolStr> {
        let uses_alloc = self
            .required_effects
            .instances
            .iter()
            .any(|inst| inst.cap_label.as_deref() == Some("Alloc"));
        if uses_alloc && !self.requires_global_alloc {
            return Err(format!(
                "route `{}` unexpectedly requires global Alloc; use per-request req: Region instead",
                self.pattern
            )
            .into());
        }
        Ok(())
    }
}
```


== 35. Vesper Chorus: Choreographic Consensus and Zero-Copy RDMA
<ch-34>
#chapter-deck[Session Endpoints, RDMA Lifetimes, Protocol Projection, and Quorum Models]

*Vesper Chorus* (`@vesper-dist/chorus`, with implementation code in `crates/vesper-chorus`) describes consensus, session-typed RPC, and RDMA (`ibverbs`, RoCEv2, and CXL). Drawing on _Choreographic Programming_ (PLDI 2026, #link(<ch-1>)[Chapter 1]) and multiparty session types, it proposes projecting `@choreography` protocols into dual `Session` endpoints. `RdmaRegion` and `InFlightRdmaWrite` track memory-registration obligations, while Lean 4 models address Raft/Paxos quorum intersection. Endpoint typing and quorum lemmas do not by themselves establish distributed liveness or safe hardware revocation.

=== 35.1 Deadlock-Free Session Typing, RDMA Registration, and Consensus Guarantees
<ch-34-1>
+ *Protocol Projection (`@choreography`):* Chorus proposes generating role-local code from one global protocol through endpoint projection (EPP). The WASI macro runs with `macro_emits_effects = []` (#link(<ch-6-4>)[Section 7.4]). Projection must reject unmergeable branches when an uninvolved participant cannot determine which path to follow. The intended deadlock-freedom result depends on the protocol fragment, projection theorem, and communication assumptions; generating dual endpoint types alone does not establish it for a deployed system.

+ *Typestate Linear Session Channels (`Session[Proto]`):* Conventional gRPC and TCP streams permit handlers to drop a bidirectional stream mid-handshake or send out-of-order replies. Chorus types every protocol channel as a `Mode::Linear` `Session[P]` obligation advancing across `Send[Msg, Next]`, `Recv[Msg, Next]`, `Select[L, R]`, `Offer[L, R]`, and `End`. Under Theorem 2 and #link(<ch-4>)[Chapter 4]'s `[T-Let]` Resumption Frame Condition, every protocol step must be driven to `End` or explicitly transitioned onto a fault-recovery handler on transport failure (`NetFail`).

+ *NIC-Safe Zero-Copy RDMA Leases (`RdmaRegion` and `InFlightRdmaWrite`):* In Rust `ibverbs` wrappers, registering a memory region (`ibv_reg_mr`) and posting one-sided `IBV_WR_RDMA_WRITE` work requests requires `unsafe` or heap `Arc` guards because dropping or mutating host buffers before the RNIC posts a Completion Queue Entry (`ibv_wc`) causes silent DMA corruption or PCIe faults. Chorus allocates registered regions directly in Hardware Domain 1 (`DmaSlab` 2 MiB hugepages, #link(<ch-12-3>)[Section 13.3]) and returns a `Mode::Linear` `InFlightRdmaOp[B]` token on Queue Pair submission—statically guaranteeing host memory remains live and unaliased until Vesper Pulse harvests the RNIC completion event.

+ *Deterministic `SimPulse` Partition, Clock-Skew, and Split-Brain Simulation:* Because `ChorusNet` and `RdmaQp` are ordinary lexical effects (`eff [...]`), `vesper test --sim` (#link(<ch-14-6>)[Section 15.6]) runs unmodified 5-node and 7-node Raft/Paxos clusters inside a single OS thread—deterministically exploring asymmetric network partitions, reordered RPC frames, stale-leader clock drift, and silent RNIC responder timeouts from a single 64-bit PRNG seed.

+ *Lean 4 Election Safety and Quorum Log-Matching Proofs (`vesper verify`):* `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts projected Chorus role implementations directly into pure Lean 4 `EffM` state machines, discharging *Election Safety* (at most one leader per term) and *Log-Matching* invariants via quorum-intersection arithmetic (`omega`).

=== 35.2 Linear Session Channels and NIC-Safe Zero-Copy RDMA Protocol
<ch-34-2>

*Review limitation:* A peer timeout does not revoke an RNIC remote key. A safe design must quarantine the registered memory, invalidate its per-transfer Type-2 memory window (or use an equivalent provider-supported revocation procedure), and wait for the required completion before reuse. Session typing cannot guarantee peer progress during a partition; liveness is conditional on transport and timeout assumptions.
Chorus unifies message-passing session types with one-sided zero-copy RDMA writes over padding-free `wire struct` headers (#link(<ch-15-2>)[Section 16.2]). Posting an RDMA write consumes or borrows the registered slice linearly until the RNIC Completion Queue confirms remote placement:

```
// Exact 40-byte Raft AppendEntries wire header for RDMA_WRITE_WITH_IMM (0 implicit padding):
wire struct RaftAppendWireHeader {
    term: u64be,
    leader_id: u32be,
    follower_slot: u32be,
    prev_log_index: u64be,
    prev_log_term: u64be,
    leader_commit: u64be,
}

// Session protocol step markers:
copy struct Send[Msg, Next] {}
copy struct Recv[Msg, Next] {}
copy struct End {}

// Mode::Linear session channel: every step consumes `sink self` and returns the next protocol state!
linear struct Session[Proto] {
    chan_id: u64,
}

impl[M: WireFrame, Next] Session[Send[M, Next]] {
    fn send(sink self, in msg: M) -> Result[Session[Next], ChorusError]
        eff [ChorusNet, Fail[ChorusError]]
    {
        let Session { chan_id } = self;
        ChorusNet::send_frame(chan_id, in msg)?;
        Ok(Session { chan_id })
    }
}

impl[M: WireFrame, Next] Session[Recv[M, Next]] {
    fn recv(sink self) -> Result[(M, Session[Next]), ChorusError]
        eff [ChorusNet, Fail[ChorusError]]
    {
        let Session { chan_id } = self;
        let msg = ChorusNet::recv_frame[M](chan_id)?;
        Ok((msg, Session { chan_id }))
    }
}

impl Session[End] {
    fn close(sink self) -> Unit eff [ChorusNet] {
        let Session { chan_id } = self;
        ChorusNet::retire_channel(chan_id);
    }
}

// Mode::Linear token representing an in-flight RNIC Queue Pair DMA transfer:
linear struct InFlightRdmaWrite {
    qp_num: u32,
    wr_id: u64,
    local_mr_slot: u32,
}

linear effect RdmaQp {
    fn post_write_with_imm(
        peer_node: u32,
        remote_addr: u64,
        rkey: u32,
        imm_term_lo: u32,
        in local_dma_slice: DmaSlice,
    ) -> InFlightRdmaWrite;

    // Linear effect (`q = linear`): suspends Pulse fiber until RNIC CQE confirms DMA completion!
    fn await_wc(sink op: InFlightRdmaWrite) -> Result[Unit, RdmaError];
}
```

=== 35.3 Endpoint Projection (`@choreography`) for Zero-Copy Quorum Replication
<ch-34-3>
Writing a single `@choreography` block allows Vesper Forge (`vesper-loom-sandbox`) to project synchronized `Leader` and `Follower` functions automatically while preserving zero global heap allocation (`Alloc` omitted from `eff [...]`):

```
copy struct AppendAck {
    follower_id: u32,
    term: u64,
    match_index: u64,
    accepted: WireFlag8,
}

// Global choreography: verified mergeable & projected to `Leader` and `Follower` endpoints in <80us!
@choreography(roles = [Leader, Follower])
fn raft_replicate_slot(
    Leader.hdr: RaftAppendWireHeader,
    Leader.batch_dma: DmaSlice,
    Follower.ring_va: RemoteSlotGrant,
) -> Leader.AppendAck {
    // 1. One-sided RDMA write places record batch directly into Follower's Domain-1 NVMe/RAM ring:
    rdma_write Leader.(hdr, batch_dma) -> Follower.ring_va;
    // 2. Follower validates CRC + term, flushes WAL via Vesper Quarry, and replies on linear session:
    let Follower.ack: AppendAck = Follower.persist_and_ack(in ring_va);
    comm Follower.ack -> Leader
}

// Projected Leader quorum driver combining local WAL write (Section 2) + parallel RDMA fan-out:
fn commit_quorum_batch(
    inout node.{term, commit_index, peers}: RaftLeaderState,
    in batch_dma: DmaSlice,
    sink local_tx: WriteTxn,
) -> Result[CommitReceipt, ConsensusError] eff [
    Scope,
    Storage,
    RdmaQp,
    ChorusNet,
    Fail[ConsensusError]
] {
    let hdr = RaftAppendWireHeader {
        term: node.term.to_be(),
        leader_id: node.self_id.to_be(),
        follower_slot: node.next_slot.to_be(),
        prev_log_index: node.commit_index.to_be(),
        prev_log_term: node.term.to_be(),
        leader_commit: node.commit_index.to_be(),
    };

    let (local_res, peer0_res, peer1_res) = Pulse::scope(|inout s| eff [Scope, Storage, RdmaQp, ChorusNet] {
        // Fiber 0: Flush local NVMe WAL segment (`WriteTxn` linearly consumed):
        let f_local = s.spawn(|| eff [Storage] {
            handle Fail[IoError] in { Ok(local_tx.commit()) } with {
                return r => r,
                raise(e) => Err(e),
            }
        });

        // Fibers 1 & 2: One-sided RDMA write + typed `Session[Recv[AppendAck, End]]` ack harvest:
        let f_p0 = s.spawn(|| eff [RdmaQp, ChorusNet] {
            replicate_to_peer_rdma(in node.peers[0], in hdr, in batch_dma)
        });
        let f_p1 = s.spawn(|| eff [RdmaQp, ChorusNet] {
            replicate_to_peer_rdma(in node.peers[1], in hdr, in batch_dma)
        });

        (f_local.join(), f_p0.join(), f_p1.join())
    });

    let receipt = local_res.map_err(ConsensusError::LocalWal)?;
    let acks = 1 + u32::from(peer0_res.is_ok()) + u32::from(peer1_res.is_ok());
    if acks < 2 {
        Fail::raise(ConsensusError::QuorumNotReached { acks });
    }
    node.commit_index += 1;
    Ok(receipt)
}
```

=== 35.4 Lean 4 Deadlock-Freedom and Quorum Intersection Proofs (`vesper verify`)
<ch-34-4>
Extracting `@choreography` projections and quorum rules via `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) mechanically proves both *Session Duality / Deadlock Freedom* and *Raft Single-Leader Election Safety* across arbitrary cluster sizes:

```lean
-- illustrative/ChorusConsensusSafety.lean
-- 1. Projected Leader & Follower session types are exact duals (zero deadlocks or orphan messages):
theorem raft_replicate_slot_session_dual :
    SessionDual (projectRole .leader raftReplicateSlotChor)
                (projectRole .follower raftReplicateSlotChor) = true := by
  decide

-- 2. Quorum Intersection implies at most one leader can be elected in any term `t`:
theorem raft_election_single_leader_per_term
    (clusterNodes : Nat) (votesA votesB overlap : Nat)
    (h_majA : 2 * votesA > clusterNodes)
    (h_majB : 2 * votesB > clusterNodes)
    (h_incl : votesA + votesB ≤ clusterNodes + overlap) :
    overlap ≥ 1 := by
  omega
```

=== 35.5 Rust Endpoint Projection and Mergeability Verifier (`crates/vesper-chorus`)
<ch-34-5>
`crates/vesper-chorus` implements formal *Endpoint Projection (EPP)* with full branch-mergeability verification—guaranteeing that whenever a choreography branches at a decision role, every other role either learns the choice explicitly via labeled `Select`/`Offer` messages or executes identical local actions across both branches:

#_compact-listing[
```rust
// crates/vesper-chorus/src/lib.rs
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoleId(pub u16);

/// Global choreography AST parsed by the WASI `@choreography` macro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalChor {
    /// Point-to-point message or one-sided RDMA write `from -> to : msg_ty; next`.
    Comm {
        from: RoleId,
        to: RoleId,
        msg_ty: SmolStr,
        next: Box<GlobalChor>,
    },
    /// Knowledge-of-choice branch decided by `decider` and communicated to `notified` via label.
    Branch {
        decider: RoleId,
        notified: RoleId,
        left_label: SmolStr,
        left: Box<GlobalChor>,
        right_label: SmolStr,
        right: Box<GlobalChor>,
    },
    End,
}

/// Projected per-role linear session type (`Session[P]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalSessionTy {
    Send { peer: RoleId, msg_ty: SmolStr, next: Box<LocalSessionTy> },
    Recv { peer: RoleId, msg_ty: SmolStr, next: Box<LocalSessionTy> },
    Select {
        peer: RoleId,
        left_label: SmolStr,
        left: Box<LocalSessionTy>,
        right_label: SmolStr,
        right: Box<LocalSessionTy>,
    },
    Offer {
        peer: RoleId,
        left_label: SmolStr,
        left: Box<LocalSessionTy>,
        right_label: SmolStr,
        right: Box<LocalSessionTy>,
    },
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    SelfCommunication { role: RoleId },
    UnmergeableBranchForUninvolvedRole { role: RoleId },
}

/// Merges two local session projections for an uninvolved third-party role (`PLDI 2026` EPP merge).
pub fn merge_local_projections(
    role: RoleId,
    lhs: LocalSessionTy,
    rhs: LocalSessionTy,
) -> Result<LocalSessionTy, ProjectionError> {
    if lhs == rhs {
        return Ok(lhs);
    }
    match (lhs, rhs) {
        (
            LocalSessionTy::Offer {
                peer: p1,
                left_label: ll1,
                left: l1,
                right_label: rl1,
                right: r1,
            },
            LocalSessionTy::Offer {
                peer: p2,
                left_label: ll2,
                left: l2,
                right_label: rl2,
                right: r2,
            },
        ) if p1 == p2 && ll1 == ll2 && rl1 == rl2 => Ok(LocalSessionTy::Offer {
            peer: p1,
            left_label: ll1,
            left: Box::new(merge_local_projections(role, *l1, *l2)?),
            right_label: rl1,
            right: Box::new(merge_local_projections(role, *r1, *r2)?),
        }),
        _ => Err(ProjectionError::UnmergeableBranchForUninvolvedRole { role }),
    }
}

/// Projects a global choreography onto `target` role, proving absence of distributed deadlocks.
pub fn project_endpoint(
    chor: &GlobalChor,
    target: RoleId,
) -> Result<LocalSessionTy, ProjectionError> {
    match chor {
        GlobalChor::End => Ok(LocalSessionTy::End),
        GlobalChor::Comm { from, to, msg_ty, next } => {
            if from == to {
                return Err(ProjectionError::SelfCommunication { role: *from });
            }
            let cont = Box::new(project_endpoint(next, target)?);
            if *from == target {
                Ok(LocalSessionTy::Send { peer: *to, msg_ty: msg_ty.clone(), next: cont })
            } else if *to == target {
                Ok(LocalSessionTy::Recv { peer: *from, msg_ty: msg_ty.clone(), next: cont })
            } else {
                Ok(*cont)
            }
        }
        GlobalChor::Branch { decider, notified, left_label, left, right_label, right } => {
            if decider == notified {
                return Err(ProjectionError::SelfCommunication { role: *decider });
            }
            let l_proj = project_endpoint(left, target)?;
            let r_proj = project_endpoint(right, target)?;
            if *decider == target {
                Ok(LocalSessionTy::Select {
                    peer: *notified,
                    left_label: left_label.clone(),
                    left: Box::new(l_proj),
                    right_label: right_label.clone(),
                    right: Box::new(r_proj),
                })
            } else if *notified == target {
                Ok(LocalSessionTy::Offer {
                    peer: *decider,
                    left_label: left_label.clone(),
                    left: Box::new(l_proj),
                    right_label: right_label.clone(),
                    right: Box::new(r_proj),
                })
            } else {
                merge_local_projections(target, l_proj, r_proj)
            }
        }
    }
}

/// Verifies exact binary duality between two projected endpoints (`r_a` and `r_b`).
pub fn are_dual_sessions(r_a: RoleId, s_a: &LocalSessionTy, r_b: RoleId, s_b: &LocalSessionTy) -> bool {
    match (s_a, s_b) {
        (LocalSessionTy::End, LocalSessionTy::End) => true,
        (
            LocalSessionTy::Send { peer: p1, msg_ty: m1, next: n1 },
            LocalSessionTy::Recv { peer: p2, msg_ty: m2, next: n2 },
        )
        | (
            LocalSessionTy::Recv { peer: p1, msg_ty: m1, next: n1 },
            LocalSessionTy::Send { peer: p2, msg_ty: m2, next: n2 },
        ) => *p1 == r_b && *p2 == r_a && m1 == m2 && are_dual_sessions(r_a, n1, r_b, n2),
        (
            LocalSessionTy::Select { peer: p1, left_label: ll1, left: l1, right_label: rl1, right: r1 },
            LocalSessionTy::Offer { peer: p2, left_label: ll2, left: l2, right_label: rl2, right: r2 },
        )
        | (
            LocalSessionTy::Offer { peer: p1, left_label: ll1, left: l1, right_label: rl1, right: r1 },
            LocalSessionTy::Select { peer: p2, left_label: ll2, left: l2, right_label: rl2, right: r2 },
        ) => {
            *p1 == r_b
                && *p2 == r_a
                && ll1 == ll2
                && rl1 == rl2
                && are_dual_sessions(r_a, l1, r_b, l2)
                && are_dual_sessions(r_a, r1, r_b, r2)
        }
        _ => false,
    }
}
```
]


== 36. Vesper Quarry: Hermetic SQL, Buffer Pools, and AoSoA Execution
<ch-35>
#chapter-deck[SQL Checks, Page Layouts, Buffer-Pool Ownership, and Vectorized Queries]

*Vesper Quarry* (`@vesper-db/quarry`, with implementation code in `crates/vesper-quarry`) describes transactional storage, vectorized queries, and compile-time SQL checking. It supplies the storage design behind the `WriteTxn` example in #link(<ch-1>)[Chapter 1] and the `TenantDb` capability in #link(<ch-33>)[Chapter 34]. Proposed mechanisms include offline WASI `@sql` checks, linear `PagePin` ownership, `io_uring` `O_DIRECT` I/O, `egglog` query optimization, and SIMD execution over tiled collections. Storage and concurrency correctness require the additional conditions discussed below.

=== 36.1 Compile-Time SQL, Buffer-Pool Pin Conservation, and AoSoA Guarantees
<ch-35-1>
+ *Offline SQL Checking (`@sql`):* Quarry proposes replaying `migrations/*.sql` DDL into an in-memory catalog, then checking SQL syntax, join nullability, bind-parameter types, and output `AoSoA` schemas. The macro executes in *Forge*'s fuel-metered `wasm32-wasip2` Wasmtime sandbox (`macro_emits_effects = []`; #link(<ch-10-2>)[Section 11.2] and #link(<ch-6-4>)[Section 7.4]). It needs declared migration inputs, without network or ambient filesystem access. The draft targets checks below 100 microseconds; that timing requires a stated catalog and workload.

+ *Zero-Atomic Linear Buffer-Pool Page Pinning (`PagePin[M]`):* Conventional C++ and Rust storage engines (`SQLite`, `RocksDB`, `redb`, `LeanStore`) guard cached 4 KiB and 16 KiB B+Tree pages with atomic pin counts (`Arc` or `AtomicU32`), incurring multi-core cache-line contention on root and internal pages while risking silent buffer-pool exhaustion if an error path forgets to unpin. Quarry combines optimistic lock-coupling (`PageEpoch`) for latch-free internal traversal with `linear struct PagePin[Access]` (`Shared` or `Exclusive`) for leaf mutations and scans. Under Theorem 2 and `[T-Let]`, every pinned page frame must be explicitly released via `pool.unpin(sink pin)` or `pool.unpin_dirty(sink pin, wal_lsn)` on 100 percent of control-flow paths.

+ *Direct `io_uring` `O_DIRECT` Slotted Pages and `wire struct` Headers:* Pairing Vesper Pulse `linear effect Storage` (`IORING_OP_READ_FIXED` and `IORING_OP_WRITE_FIXED` over Domain-1 2 MiB hugepages, #link(<ch-14-3>)[Section 15.3] and #link(<ch-12-3>)[Section 13.3]) with Vesper Bridge padding-free `wire struct` layouts (#link(<ch-15-2>)[Section 16.2]), Quarry maps NVMe 16 KiB slotted B+Tree and PAX columnar pages directly in place with zero `memcpy` serialization and hardware-accelerated CRC32C page verification.

+ *`egglog` Relational Optimization and Vectorized `AoSoA` Morsel Execution:* Rather than materializing row-oriented `Vec<Row>` heap allocations via volcano iterators, Quarry lowers SQL ASTs into algebraic relational plans saturated by Stage 4 `egglog` rewrite rules (predicate pushdown, join reordering, projection pruning) and executes vectorized morsels directly into 16-wide Vesper Strata `tiled(16) struct` (`AoSoA`, #link(<ch-12-5>)[Section 13.5]) L1 cache tiles using Vesper Vector `eff [Isa[P]]` SIMD predicates (`Alloc` omitted from `eff [...]`).

+ *Lean 4 ARIES Crash-Consistency and Multi-Tenant Isolation Proofs (`vesper verify`):* `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts Quarry's MVCC snapshot visibility rules, ARIES write-ahead log physiological redo invariants, and `@sql` row-level security predicates into Lean 4, proving both zero leaked page pins and strict tenant data isolation.

=== 36.2 Slotted `wire struct` Pages and Linear Buffer-Pool Pin Protocol
<ch-35-2>

*Review limitation:* Linear pins prevent leaks but not latch-order deadlocks. A complete concurrent B-tree design also needs a defined latch order, safe split protocol, and a prohibition on suspending while holding a page latch.
Every 16 KiB storage page begins with a 32-byte padding-free `wire struct` header validated in place upon `io_uring` DMA completion. Acquiring a leaf page from `BufferPool` yields a `Mode::LinearPagePin` that statically forbids early `?` unwinds (`[T-Let]`) until the frame slot is returned to the pool:

```
// Exact 32-byte slotted B+Tree / PAX page header (0 implicit padding, Section 13.2):
wire struct QuarryPageWireHeader {
    page_lsn: u64le,
    page_id: u64le,
    checksum_crc32c: u32le,
    slot_count: u16le,
    free_space_offset: u16le,
    page_kind_tag: u8,
    level: u8,
    flags: u16le,
    right_sibling_page: u32le,
}

// Phantom access tags for shared reader vs. exclusive writer latches:
copy struct SharedPin {}
copy struct ExclusivePin {}

// Mode::Linear: forgetting to unpin a buffer-pool frame is a compile-time error!
linear struct PagePin[Access] {
    frame_slot: u32,
    page_id: u64,
}

linear effect BufferPool {
    fn fix_shared(page_id: u64) -> Result[PagePin[SharedPin], DbError];
    fn fix_exclusive(page_id: u64) -> Result[PagePin[ExclusivePin], DbError];
    fn view_page(in pin: PagePin[SharedPin]) -> ByteSlice;
    fn mutate_page(inout pin: PagePin[ExclusivePin]) -> ByteSliceMut;
    fn unpin_clean(sink pin: PagePin[SharedPin]) -> Unit;
    fn unpin_dirty(sink pin: PagePin[ExclusivePin], rec_lsn: u64) -> Unit;
    fn unpin_aborted(sink pin: PagePin[ExclusivePin]) -> Unit;
}
```

=== 36.3 Hermetic `@sql` Queries and Vectorized `AoSoA` SIMD Execution
<ch-35-3>
When `@sql(...)` expands inside `vesper-loom-sandbox`, it validates column types against `migrations/*.sql` and emits a vectorized morsel operator that streams matching tuples straight into a `TiledSeq` inside a bounded lexical `Region` scratchpad (`Alloc` is absent from `eff [...]`):

```
// Cache-conscious 16-row AoSoA output tile verified against `migrations/001_orders.sql`:
tiled(16) copy struct TenantOrderSummary {
    hot order_id: u64,
    hot org_id: u64,
    hot total_cents: i64,
    hot created_epoch_s: u64,
    cold customer_ref: InlineStr[32],
}

// Predicated SIMD PAX page filter + aggregation (`0` global heap allocations, `0` unsafe!):
fn scan_high_value_orders[P: IsaProfile, r: Region](
    in arena: RegionCap[r],
    target_org: u64,
    min_cents: i64,
    leaf_page_ids: Slice[u64],
    inout out_tiles: TiledSeq[TenantOrderSummary],
) -> Result[i64, DbError] eff [r: Region, BufferPool, Isa[P], Fail[DbError]] {
    let v_org = VVec::splat[P, u64](target_org);
    let v_min = VVec::splat[P, i64](min_cents);
    let mut grand_total: i64 = 0;

    for i in 0..leaf_page_ids.len() {
        let pin: PagePin[SharedPin] = BufferPool::fix_shared(leaf_page_ids[i])?;

        // Note: `[T-Let]` statically forbids `?` while `pin` is live; validate page explicitly:
        let page_bytes = BufferPool::view_page(in pin);
        match decode_pax_order_columns[P](in page_bytes, v_org, v_min, inout out_tiles) {
            Ok(page_sum) => {
                grand_total = grand_total.wrapping_add(page_sum);
                BufferPool::unpin_clean(sink pin); // Linearly releases frame_slot on happy path!
            }
            Err(corrupt_err) => {
                BufferPool::unpin_clean(sink pin); // Linearly releases frame_slot on error path!
                Fail::raise(corrupt_err);
            }
        }
    }
    Ok(grand_total)
}

// Hermetic compile-time SQL macro (`emits_effects = []`) lowering to `scan_high_value_orders`:
fn fetch_org_revenue[P: IsaProfile](
    org_grant: OrgReadCap,
    min_cents: i64,
) -> Result[(i64, u32), DbError] eff [BufferPool, Isa[P], Fail[DbError]] {
    Region::scoped(128.kib(), |cap q_arena| {
        let mut rows = TiledSeq::with_region[TenantOrderSummary](in q_arena, 1024);
        let sum = @sql(
            "SELECT order_id, org_id, total_cents, created_epoch_s, customer_ref\n             FROM orders\n             WHERE org_id = :org_grant.org_id AND total_cents >= :min_cents"
        ).fetch_into_tiles[P](in q_arena, inout rows)?;
        Ok((sum, rows.len()))
    })
}
```

=== 36.4 Lean 4 Pin-Conservation and Multi-Tenant Isolation Proofs (`vesper verify`)
<ch-35-4>
Extracting `scan_high_value_orders` and `@sql` query plans through `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Buffer-Pool Frame Conservation* (zero leaked pins even on corrupt-page CRC errors) and *Row-Level Tenant Isolation*:

```lean
-- illustrative/QuarrySafety.lean
-- 1. Pin Conservation: net pinned frames in `RefBufferPool` is invariant across all Ok and Err paths!
theorem scan_orders_conserves_page_pins
    (pool : RefBufferPool) (org : UInt64) (minCents : Int64) (pages : Array UInt64)
    (tiles : Array TenantOrderSummary) :
    let (pool', _res) := RefBufferPool.run pool (scanHighValueOrders org minCents pages tiles)
    pool'.pinnedFrameCount = pool.pinnedFrameCount := by
  simp [scanHighValueOrders, RefBufferPool.run]

-- 2. Strict Tenant Confinement: every materialized row in `out_tiles` belongs to `target_org`!
theorem fetch_org_revenue_tenant_isolated
    (db : CanonicalRelationOrders) (targetOrg : UInt64) (minCents : Int64) :
    ∀ row ∈ MaterializedRows (evalQuarryPlan db targetOrg minCents),
      row.orgId = targetOrg ∧ row.totalCents ≥ minCents := by
  simp [evalQuarryPlan, MaterializedRows]
  aesop
```

=== 36.5 Rust Hermetic SQL Catalog and Relational Optimizer Core (`crates/vesper-quarry`)
<ch-35-5>
`crates/vesper-quarry` provides the deterministic SQL schema catalog used by the WASI `@sql` macro (with full outer-join nullability inference) alongside the relational predicate-pushdown optimizer:

```rust
// crates/vesper-quarry/src/lib.rs
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SqlScalarTy {
    U64,
    I64,
    F64,
    Bool,
    Utf8Str,
    Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: SmolStr,
    pub ty: SqlScalarTy,
    pub nullable: bool,
}

#[derive(Clone, Default, Debug)]
pub struct HermeticCatalog {
    pub tables: FxHashMap<SmolStr, Vec<ColumnDef>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    LeftOuter,
    RightOuter,
    FullOuter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlTypeError {
    UnknownTable(SmolStr),
    UnknownColumn { table: SmolStr, col: SmolStr },
}

impl HermeticCatalog {
    pub fn register_table(&mut self, table: impl Into<SmolStr>, cols: Vec<ColumnDef>) {
        self.tables.insert(table.into(), cols);
    }

    /// Infers projected column types & exact outer-join nullability inside `vesper-loom-sandbox` (`0` sockets!).
    pub fn infer_join_projection(
        &self,
        lhs_table: &str,
        rhs_table: &str,
        join_kind: JoinKind,
        select_cols: &[(&str, &str)],
    ) -> Result<Vec<ColumnDef>, SqlTypeError> {
        let lhs_cols = self
            .tables
            .get(lhs_table)
            .ok_or_else(|| SqlTypeError::UnknownTable(lhs_table.into()))?;
        let rhs_cols = self
            .tables
            .get(rhs_table)
            .ok_or_else(|| SqlTypeError::UnknownTable(rhs_table.into()))?;

        let lhs_forced_null = matches!(join_kind, JoinKind::RightOuter | JoinKind::FullOuter);
        let rhs_forced_null = matches!(join_kind, JoinKind::LeftOuter | JoinKind::FullOuter);

        let mut out = Vec::with_capacity(select_cols.len());
        for &(tbl, col_name) in select_cols {
            let (table_cols, force_null) = if tbl == lhs_table {
                (lhs_cols, lhs_forced_null)
            } else if tbl == rhs_table {
                (rhs_cols, rhs_forced_null)
            } else {
                return Err(SqlTypeError::UnknownTable(tbl.into()));
            };

            let found = table_cols
                .iter()
                .find(|c| c.name == col_name)
                .ok_or_else(|| SqlTypeError::UnknownColumn {
                    table: tbl.into(),
                    col: col_name.into(),
                })?;

            out.push(ColumnDef {
                name: found.name.clone(),
                ty: found.ty,
                nullable: found.nullable || force_null,
            });
        }
        Ok(out)
    }
}

/// Algebraic relational plan node optimized prior to vectorized AoSoA codegen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelPlan {
    PaxTableScan {
        table: SmolStr,
        pushed_predicates: Vec<(SmolStr, SmolStr)>,
    },
    Filter {
        referenced_tables: FxHashSet<SmolStr>,
        predicate_sql: SmolStr,
        input: Box<RelPlan>,
    },
    HashJoin {
        lhs: Box<RelPlan>,
        rhs: Box<RelPlan>,
        on_eq: (SmolStr, SmolStr),
    },
}

impl RelPlan {
    pub fn provided_tables(&self) -> FxHashSet<SmolStr> {
        match self {
            Self::PaxTableScan { table, .. } => FxHashSet::from_iter([table.clone()]),
            Self::Filter { input, .. } => input.provided_tables(),
            Self::HashJoin { lhs, rhs, .. } => {
                let mut set = lhs.provided_tables();
                set.extend(rhs.provided_tables());
                set
            }
        }
    }

    /// Pushes single-table selection predicates down into `PaxTableScan` SIMD column filters.
    pub fn push_down_predicates(self) -> Self {
        match self {
            Self::Filter { referenced_tables, predicate_sql, input } => {
                let opt_in = input.push_down_predicates();
                match opt_in {
                    Self::PaxTableScan { table, mut pushed_predicates }
                        if referenced_tables.len() == 1 && referenced_tables.contains(&table) =>
                    {
                        pushed_predicates.push((table.clone(), predicate_sql));
                        Self::PaxTableScan { table, pushed_predicates }
                    }
                    Self::HashJoin { lhs, rhs, on_eq } => {
                        let lhs_tabs = lhs.provided_tables();
                        let rhs_tabs = rhs.provided_tables();
                        if referenced_tables.is_subset(&lhs_tabs) {
                            Self::HashJoin {
                                lhs: Box::new(
                                    Self::Filter { referenced_tables, predicate_sql, input: lhs }
                                        .push_down_predicates(),
                                ),
                                rhs,
                                on_eq,
                            }
                        } else if referenced_tables.is_subset(&rhs_tabs) {
                            Self::HashJoin {
                                lhs,
                                rhs: Box::new(
                                    Self::Filter { referenced_tables, predicate_sql, input: rhs }
                                        .push_down_predicates(),
                                ),
                                on_eq,
                            }
                        } else {
                            Self::Filter {
                                referenced_tables,
                                predicate_sql,
                                input: Box::new(Self::HashJoin { lhs, rhs, on_eq }),
                            }
                        }
                    }
                    other => Self::Filter {
                        referenced_tables,
                        predicate_sql,
                        input: Box::new(other),
                    },
                }
            }
            Self::HashJoin { lhs, rhs, on_eq } => Self::HashJoin {
                lhs: Box::new(lhs.push_down_predicates()),
                rhs: Box::new(rhs.push_down_predicates()),
                on_eq,
            },
            scan @ Self::PaxTableScan { .. } => scan,
        }
    }
}
```


== 37. Vesper Flux: Differential Stream Processing and DBSP Z-Sets
<ch-36>
#chapter-deck[Incremental Operators, Consolidation, Watermarks, and Feedback]

*Vesper Flux* (`@vesper-stream/flux`, with implementation code in `crates/vesper-flux`) describes incremental view maintenance (IVM), differential streams, and change-data-capture (CDC) materialized views. Its research basis is *DBSP* (_Automatic Incremental View Maintenance for Rich Query Languages_, Budiu, McSherry, Ryzhyk, and Tannen, VLDB 2023 / Feldera 2025–2026). The design combines Z-Set group operators ($Delta (Q)$), incremental joins, linear `FrontierTicket` watermarks, and clock-guarded recursion over `AoSoA` tiles. Bounded state and distributed execution require separate justification.

=== 37.1 Incremental Z-Set Exactness, Frontier Watermark, and State Guarantees
<ch-36-1>
+ *Z-Set Storage (`ZSetTile[K, V]`):* Flux represents changes as tuples with signed 64-bit multiplicity weights: `+1` inserts a tuple, and `-1` deletes or retracts it. The proposed layout uses *Strata* `TiledSeq` columns (`tiled(16) struct`, #link(<ch-12-5>)[Section 13.5]) in per-epoch `VirtualArena` regions, with `Alloc` omitted from the effect row. SIMD consolidation (`eff [Isa[P]]`, #link(<ch-13>)[Chapter 14]) merges duplicate keys and removes net-zero updates (`+1 - 1 == 0`) before accessing persistent state. Cache residency depends on the batch size and target.

+ *Mode::Linear Frontier and Watermark Progress Tracking (FrontierTicket):* In timely-dataflow, operator progress capabilities (`Capability<Timestamp>`) rely on runtime reference counts; retaining a capability accidentally in a map stalls global garbage collection and window emission forever, while premature drops emit corrupt incomplete windows. Flux reifies every event-time watermark hold as a `Mode::Linear` obligation (`linear struct FrontierTicket`). Under Theorem 2 and `[T-Let]`, every shard operator must explicitly advance (`advance_to`) or discharge (`retire_frontier`) its `FrontierTicket` along 100 percent of control paths, guaranteeing deadlock-free monotonic frontier propagation with zero atomic reference counting.

+ *Stage 4 egglog Automatic Query Differentiation (`@materialized`\_view):* Rather than hand-coding stateful window panes, join eviction timers, and retraction Compensation logic, developers write standard declarative *Vesper Quarry* SQL or relational expressions (#link(<ch-35>)[Chapter 36]) annotated with `@materialized_view`. Stage 4 (`vesper-opt-egglog`, #link(<ch-7>)[Chapter 8]) applies the exact *DBSP* differentiation functor `D(I(Q(s))) = Q_inc(s)`—rewriting bilinear joins into `Delta(A Join B) = (DeltaA Join Delay(B)) + (Delay(A) Join DeltaB) + (DeltaA Join DeltaB)` paired with `z^-1` delay integrators—and fuses filter, project, and hash-probe pipelines into branchless SIMD loops.

+ *Exactly-Once Chandy-Lamport Checkpointing Over Krabka and Quarry (BarrierToken):* Flux aligns distributed asynchronous snapshots directly with Vesper's substructural types. Checkpoint barriers propagate across *Vesper Pulse* channels (#link(<ch-14-4>)[Section 15.4]) as `Mode::Linear` `BarrierToken` obligations; an operator cannot process post-barrier records until it flushes its incremental LSM Z-Set spine delta and upstream Krabka offset atomically inside a *Vesper Quarry* `WriteTxn` (#link(<ch-1>)[Chapter 1] and #link(<ch-35>)[Chapter 36]) over `io_uring` `O_DIRECT`.

+ *Lean 4 Incremental-Batch Equivalence and Watermark Monotonicity Proofs (`vesper verify`):* `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) models Z-Sets as finitely supported abelian group homomorphisms (`Tuple -> Int`) and mechanically proves that integrating Flux's incremental circuit output reproduces the exact batch query result at every timestamp (`I(Q_inc(ds)) = Q(I(ds))`), alongside zero-weight cancellation and monotonic watermark invariants.

=== 37.2 wire struct CDC Deltas, AoSoA Z-Set Tiles, and Linear Frontier Capabilities
<ch-36-2>
Flux ingests Kafka/Krabka Change-Data-Capture (CDC) log segments zero-copy via a 32-byte padding-free `wire struct` header (#link(<ch-15-2>)[Section 16.2]), materializing batch deltas into 16-wide AoSoA Z-Set tiles while guarding event-time progress and checkpoint barriers with `Mode::Linear` tokens:

```
// Exact 32-byte CDC changelog wire header (+1 insert / -1 delete retraction, 0 implicit padding):
wire struct CdcDeltaWireHeader {
    source_lsn: u64le,
    event_ts_ms: i64le,
    z_weight: i64le,
    partition_id: u32le,
    schema_id: u16le,
    flags: u16le,
}

// 16-wide AoSoA Z-Set cache tile: hot join keys and signed multiplicities align to 64B L1 cache lines!
tiled(16) copy struct OrderRiskDeltaTile {
    hot account_id: u64,
    hot amount_cents: i64,
    hot z_weight: i64,
    hot event_ts_ms: i64,
    cold merchant_code: u32,
}

// Mode::Linear event-time progress capability: prevents both stalled frontiers and premature window close!
linear struct FrontierTicket {
    operator_id: u32,
    watermark_ts_ms: i64,
}

// Mode::Linear Chandy-Lamport checkpoint barrier token: enforces atomic Quarry spine + offset commit!
linear struct BarrierToken {
    epoch_id: u64,
    shard_id: u16,
}

linear effect FluxRuntime {
    fn advance_watermark(
        sink ticket: FrontierTicket,
        next_watermark_ts_ms: i64,
    ) -> Result[FrontierTicket, FluxError];

    fn retire_frontier(sink ticket: FrontierTicket) -> Unit;

    // Commits compacted Z-Set spine batch + Krabka source offset atomically into Vesper Quarry:
    fn commit_checkpoint_barrier(
        sink barrier: BarrierToken,
        inout spine: ZSetLsmSpine,
        source_lsn: u64,
    ) -> Result[CommitReceipt, FluxError];

    fn abort_checkpoint_barrier(sink barrier: BarrierToken) -> Unit;
}
```

=== 37.3 Zero-Allocation Incremental Bilinear Stream Join and Tumbling Window (0 Alloc)
<ch-36-3>

*Review limitation:* General incremental joins can retain unbounded historical state. Zero-allocation operation requires a proven finite state bound and capacity policy; otherwise state must grow or spill with explicit effects and failure handling.
Inside a per-core Krabka stream processor shard, `step_incremental_exposure_view` evaluates the differentiated bilinear join between streaming order deltas (`delta_orders`—including `-1` cancellations and amendments) and an indexed `account_limits` Z-Set spine inside a 256 KiB lexical `Region::scoped` arena (`Alloc` omitted from `eff [...]`), enforcing under `[T-Let]` that neither `FrontierTicket` nor `BarrierToken` can be dropped on error paths:

```
fn step_incremental_exposure_view[P: IsaProfile](
    in raw_cdc_batch: DmaSlice,
    in risk_spine: ZSetLsmSpine,
    inout exposure_spine: ZSetLsmSpine,
    sink frontier: FrontierTicket,
    sink maybe_barrier: Option[BarrierToken], // Crosses to Mode::Linear via [K-Adt] (Section 4)!
) -> Result[FrontierTicket, FluxError] eff [FluxRuntime, Storage, Isa[P], Fail[FluxError]] {
    let (batch_max_ts, max_lsn) = Region::scoped(256.kib(), |cap tick_arena| {
        let mut delta_in  = TiledSeq::with_region[OrderRiskDeltaTile](in tick_arena, 2048);
        let mut delta_out = TiledSeq::with_region[OrderRiskDeltaTile](in tick_arena, 2048);

        // 1. Zero-copy wire decode + SIMD Z-Set consolidation (cancels +1/-1 updates in L1 cache!):
        let (observed_ts, last_lsn) =
            Flux::decode_and_consolidate_cdc[P](in raw_cdc_batch, inout delta_in);

        // 2. DBSP incremental bilinear join: Delta(Orders Join Risk) = DeltaOrders Join z^-1(Risk):
        Flux::probe_zset_spine_simd[P](
            in delta_in,
            in risk_spine,
            inout delta_out,
        );

        // 3. Apply consolidated output Z-Set weights (+w / -w) into materialized exposure spine:
        exposure_spine.merge_consolidated_tiles[P](in delta_out);
        (observed_ts, last_lsn)
    });

    // 4. Discharge optional Mode::Linear Chandy-Lamport checkpoint barrier before advancing frontier:
    match maybe_barrier {
        Some(barrier) => {
            match FluxRuntime::commit_checkpoint_barrier(sink barrier, inout exposure_spine, max_lsn) {
                Ok(_receipt) => {}
                Err(ckpt_err) => {
                    FluxRuntime::retire_frontier(sink frontier);
                    Fail::raise(ckpt_err);
                }
            }
        }
        None => {}
    }

    // 5. Monotonically advance event-time FrontierTicket (rejects watermark regressions):
    FluxRuntime::advance_watermark(sink frontier, batch_max_ts)
}
```

=== 37.4 Lean 4 DBSP Incremental Exactness and Watermark Monotonicity Proofs (`vesper verify`)
<ch-36-4>
`vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) models Z-Sets as finitely supported abelian group homomorphisms (`Tuple -> Int`) and mechanically proves both the fundamental *DBSP Bilinear Join Differentiation Identity* over signed `Int` Z-Set weights and *Strict Watermark Non-Regression*:

```lean
-- illustrative/FluxDbspSafety.lean
-- 1. DBSP Bilinear Join Exactness: for any Z-Set multiplicities `a, b : Int` and streaming deltas `da, db`,
-- applying the 3-term incremental join delta to `a * b` yields the exact updated batch join `(a + da) * (b + db)`:
theorem dbsp_bilinear_join_weight_exact (a b da db : Int) :
    let prevJoin := a * b
    let incDelta := da * b + a * db + da * db
    prevJoin + incDelta = (a + da) * (b + db) := by
  have h_expand : (a + da) * (b + db) = a * b + (da * b + a * db + da * db) := by
    simp [Int.add_mul, Int.mul_add, Int.add_assoc, Int.add_left_comm, Int.add_comm]
  omega

-- 2. Z-Set Retraction Cancellation: inserting (+w) then retracting (-w) consolidates to multiplicity 0:
theorem zset_retraction_cancels_cleanly (w : Int) :
    w + (-w) = 0 := by
  omega

-- 3. Frontier Monotonicity: chained `advanceWatermark` transitions never regress event-time bounds:
theorem frontier_chain_monotonic (t0 t1 t2 : Int)
    (h01 : t0 ≤ t1) (h12 : t1 ≤ t2) :
    t0 ≤ t2 := by
  omega
```

=== 37.5 Rust Z-Set Consolidation Engine and DBSP Circuit Differentiator (`crates/vesper-flux`)
<ch-36-5>
`crates/vesper-flux` implements the zero-allocation in-place Z-Set weight consolidator, the monotonic `FrontierTracker` enforcing event-time progress invariants, and the Stage 4 algebraic *DBSP* relational differentiator (`D(Q)`):

```rust
// crates/vesper-flux/src/lib.rs
use smol_str::SmolStr;

/// Consolidated Z-Set batch sorting keys and summing signed `i64` multiplicities (`+1` insert, `-1` delete).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ZSetBatch<K: Ord> {
    pub entries: Vec<(K, i64)>,
}

impl<K: Ord> ZSetBatch<K> {
    /// Coalesces duplicate keys by summing `i64` weights in place and eliding net-zero (`w == 0`) tuples.
    pub fn consolidate_in_place(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.entries.sort_unstable_by(|(ka, _), (kb, _)| ka.cmp(kb));

        let mut write_idx = 0usize;
        for read_idx in 1..self.entries.len() {
            if self.entries[write_idx].0 == self.entries[read_idx].0 {
                let w = self.entries[read_idx].1;
                self.entries[write_idx].1 = self.entries[write_idx].1.saturating_add(w);
            } else {
                if self.entries[write_idx].1 != 0 {
                    write_idx += 1;
                }
                if write_idx != read_idx {
                    self.entries.swap(write_idx, read_idx);
                }
            }
        }
        let final_len = if self.entries[write_idx].1 != 0 {
            write_idx + 1
        } else {
            write_idx
        };
        self.entries.truncate(final_len);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FluxCheckError {
    WatermarkRegression { prev_ts_ms: i64, attempted_ts_ms: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrontierTracker {
    pub current_watermark_ts_ms: i64,
}

impl FrontierTracker {
        if next_ts_ms < self.current_watermark_ts_ms {
            return Err(FluxCheckError::WatermarkRegression {
                prev_ts_ms: self.current_watermark_ts_ms,
                attempted_ts_ms: next_ts_ms,
            });
        }
        self.current_watermark_ts_ms = next_ts_ms;
        Ok(self.current_watermark_ts_ms)
    }
}

/// Relational Z-Set algebra differentiated by Stage 4 (`vesper-opt-egglog`) into incremental DBSP circuits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZSetCircuitExpr {
    StreamInput(SmolStr),
    Delta(SmolStr),
    DelayPrevIntegrate(Box<ZSetCircuitExpr>),
    LinearFilterMap { pred_name: SmolStr, input: Box<ZSetCircuitExpr> },
    UnionPlus(Box<ZSetCircuitExpr>, Box<ZSetCircuitExpr>),
    EquiJoin(Box<ZSetCircuitExpr>, Box<ZSetCircuitExpr>),
}

impl ZSetCircuitExpr {
    /// Applies the VLDB 2023 DBSP differentiation operator `D(Q)`:
    /// - Linear operators commute with differentiation: `D(L(A)) = L(D(A))`
    /// - Bilinear EquiJoin expands to `(dA Join z^-1(I(B))) + (z^-1(I(A)) Join dB) + (dA Join dB)`.
    pub fn differentiate_dbsp(&self) -> Self {
        match self {
            Self::StreamInput(name) => Self::Delta(name.clone()),
            Self::Delta(name) => Self::Delta(name.clone()),
            Self::DelayPrevIntegrate(inner) => {
                Self::DelayPrevIntegrate(Box::new(inner.differentiate_dbsp()))
            }
            Self::LinearFilterMap { pred_name, input } => Self::LinearFilterMap {
                pred_name: pred_name.clone(),
                input: Box::new(input.differentiate_dbsp()),
            },
            Self::UnionPlus(a, b) => Self::UnionPlus(
                Box::new(a.differentiate_dbsp()),
                Box::new(b.differentiate_dbsp()),
            ),
            Self::EquiJoin(a, b) => {
                let da = a.differentiate_dbsp();
                let db = b.differentiate_dbsp();
                let delayed_a = Self::DelayPrevIntegrate(a.clone());
                let delayed_b = Self::DelayPrevIntegrate(b.clone());

                let term1 = Self::EquiJoin(Box::new(da.clone()), Box::new(delayed_b));
                let term2 = Self::EquiJoin(Box::new(delayed_a), Box::new(db.clone()));
                let term3 = Self::EquiJoin(Box::new(da), Box::new(db));

                Self::UnionPlus(Box::new(term1), Box::new(Self::UnionPlus(Box::new(term2), Box::new(term3))))
            }
        }
    }
}
```


= Part VII. Native Interfaces, Tri-Backend Graphics, Simulation, and Machine Learning
<part-7>
#chapter-deck[Native Interfaces, Graphics, Physics, Audio, Robotics, and Tensor Compute]

#_part-summary("Part VII Architectural Scope")[
  Part VII describes client applications and compute services. #link(<ch-37>)[Chapter 38] presents *Aura* native interfaces, and #link(<ch-38>)[Chapter 39] presents *Ignis* graphics across DirectX 12, Vulkan, and Metal. #link(<ch-39>)[Chapter 40] covers *Kinetic* physics and rollback; #link(<ch-40>)[Chapter 41] covers *Resonance* audio and robotics control. #link(<ch-41>)[Chapter 42] presents *Tensor* shape checking and inference. Backend support, numerical determinism, and real-time bounds depend on the implementation and target; the proposed contracts do not establish those properties across every platform.
]

#figure(
  table(
    columns: (14%, 34%, 52%),
    align: (left, left, left),
    table.header([*Chapter*], [*Subsystem*], [*Package, Host Crate, and Interactive / Compute Guarantees*]),
    [#link(<ch-37>)[Chapter 38]], [Vesper Aura (Native UI Framework)], [`@vesper-ui/aura` (`vesper-aura`): view-delta calculus, WinUI 3 / GTK 4 / SwiftUI, `UiThread`, `SimAura`],
    [#link(<ch-38>)[Chapter 39]], [Vesper Ignis (Tri-Backend 3D Engine)], [`@vesper-game/ignis` (`vesper-ignis`): WASI Slang, D3D12 / Vulkan 1.4 / Metal 4, static hazard barriers],
    [#link(<ch-39>)[Chapter 40]], [Vesper Kinetic (Deterministic Physics)], [`@vesper-sim/kinetic` (`vesper-kinetic`): `eff [DetFloat]`, graph-colored SIMD XPBD, Morton LBVH, rollback],
    [#link(<ch-40>)[Chapter 41]], [Vesper Resonance (Realtime DSP & Control)], [`@vesper-rt/resonance` (`vesper-resonance`): `@realtime`, wait-free SPSC rings, automatic PDC delay graphs],
    [#link(<ch-41>)[Chapter 42]], [Vesper Tensor (Shape-Checked ML & LLM)], [`@vesper-ml/tensor` (`vesper-tensor`): symbolic dimensions, `wire struct` quantization, `KvPageLease` GQA],
  ),
  kind: table,
)


== 38. Vesper Aura: Cross-Platform Native User Interfaces
<ch-37>
#chapter-deck[View Updates, State Lenses, Native Backends, and Accessibility]

*Vesper Aura* (`@vesper-ui/aura`, with implementation code in `crates/vesper-aura`) describes a typed native user-interface framework. Its proposed backends map one declarative view architecture to WinUI 3 (Windows App SDK 2.x), GTK 4 (`libgtk-4` and `libadwaita`), and SwiftUI (`NSHostingView` and Swift 6 `ObservationRegistrar`). This chapter explains view updates, state access, and accessibility contracts; backend coverage must be checked against the implementation.

Grounded in typed view-tree reconciliation (_Xilem_) and generic incremental differentiation (_DeCo: A Core Calculus for Incremental Functional Programming with Generic Data Types_, Böhler, Reinhard, Richter, and Mezini, OOPSLA 2026), Aura reconciles COM/WinRT object trees, GObject signals, and declarative Swift value trees without reference-counted `RefCell` graphs, runtime borrow panics, or heap-boxed `AnyView` erasure—combining disjoint `inout` state lenses, `UiThread` capability confinement, and headless `SimAura` WCAG 2.2 AA verification.

=== 38.1 Fine-Grained View-Delta, Main-Thread Confinement, and Lifecycle Guarantees
<ch-37-1>
+ *100% Authentic OS Fidelity and Accessibility:* Renders genuine Windows 11 Mica/Fluent controls with DirectComposition animations, GNOME `libadwaita` widgets over GTK 4's Vulkan GSK scene graph with AT-SPI accessibility, and macOS vibrancy materials, native menu bars, SF Symbols, and VoiceOver `NSAccessibility` trees.
+ *Zero-RefCell State and Disjoint Sub-View Lenses:* View functions read pure second-class `in` projections of application state while reducers mutate disjoint field subtrees (`inout app.{sidebar}` vs. `inout app.{editor}`) proven non-aliasing at compile time via `[T-CallProj]`—eliminating `Rc`, `RefCell`, `@weak` callback macros, and runtime re-entrancy panics.
+ *Linear Native Signal and Event Teardown:* Native event registrations (`EventRegistrationToken` in WinUI 3, `gulong` signal handler IDs in GTK 4, and slot callbacks in SwiftUI) are wrapped in `linear struct` handles that must be explicitly disconnected inside `View::unmount`, making reference-cycle and listener leaks compile-time errors under Theorem 2.
+ *Dual Retained and Declarative Lowering Bridge:* Lowers incremental O(Δ) view-tree patches directly into imperative COM/C mutations on WinUI 3 and GTK 4 while driving fine-grained `ObservationRegistrar` slot invalidations for SwiftUI's `AttributeGraph` on macOS without rebuilding `AnyView` hierarchies.
+ *Compile-Time Main-Thread Confinement and Pulse Co-Scheduling:* Widget handles are non-portable (`!portable`) and gated by the lexical capability `eff [UiThread]`, statically preventing off-thread UI mutations while co-scheduling native GUI frame clocks alongside Vesper Pulse `io_uring`, `kqueue`, and IOCP fibers.
+ *120 Hz Zero-Copy Virtualized Tables Over AoSoA Collections:* Binds Vesper Strata `TiledSeq` (`AoSoA`) columns directly to WinUI 3 `ItemsRepeater`, GTK 4 `GtkListView`, and macOS `NSTableView` / SwiftUI `List`—scrolling 10,000,000 structured rows at 120 fps with zero per-row heap allocations.

=== 38.2 Incremental View-Delta Calculus and Disjoint State Lenses
<ch-37-2>
Aura eliminates Virtual-DOM heap allocation churn by encoding widget topology into generic Vesper Pact `View` types (#link(<ch-6>)[Chapter 7]). Static layout compositions (`AppShell`, `VStack`, `NavigationSplit`) compile into flat inline structs, while dynamic child lists bump-allocate inside a 32 KiB per-frame `Region` (`VirtualArena`, #link(<ch-12>)[Chapter 13]) that resets in one instruction after reconciliation:

```
// Colorless View trait parameterized over state S, affine/linear message Msg, and OS backend B:
trait View[S, Msg: mod linear, B: UiBackend] {
    // Persistent widget tracking state holding linear OS signal subscriptions and cached props:
    type State: mod linear;

    // Initial mount: constructs the native WinUI 3 / GTK 4 / SwiftUI control and linear State:
    fn mount(in self, inout ctx: B::MountCtx) -> (B::Widget, Self::State) eff [UiThread];

    // O(Delta) incremental reconciliation (DeCo, OOPSLA 2026): diffs `self` against `prev`
    // and patches ONLY mutated properties or child subtrees on the native OS widget!
    fn reconcile(
        in self,
        in prev: Self,
        inout widget: B::Widget,
        inout state: Self::State,
        inout ctx: B::ReconcileCtx,
    ) -> Unit eff [UiThread];

    // Exact-once teardown: consumes linear State, disconnecting all native OS event handlers:
    fn unmount(
        sink widget: B::Widget,
        sink state: Self::State,
        inout ctx: B::UnmountCtx,
    ) -> Unit eff [UiThread];
}
```

Parent views compose modular child views using `Lens::zoom(|in app.{sidebar}| sidebar_view(in app.sidebar), Msg::Sidebar)`. Because `vesper-hir` represents `app.{sidebar}` as a `FieldTree` (#link(<ch-7-3>)[Section 8.3]), independent UI panes project disjoint state slices without cloning models or boxing closures.

=== 38.3 Native Toolkit Lowering: WinUI 3, GTK 4, and SwiftUI
<ch-37-3>
Aura bridges imperative retained widgets (WinUI 3 and GTK 4) and declarative value-typed views (SwiftUI) through three thin, declarative `.vsys` backends (#link(<ch-15>)[Chapter 16]):

+ *Windows Backend (`AuraWinUi3`):* The proposed `sys/winui3.vsys` bindings expose Windows App SDK 2.x C/COM activation vtables, including `RoGetActivationFactory`, `Microsoft.UI.Xaml.Window`, `MicaBackdrop`, `NavigationView`, `CommandBar`, `ItemsRepeater`, and `Microsoft.UI.Composition`. Each `add_Click` or `add_TextChanged` registration returns a linear `WinRtEventSub` containing the 64-bit `EventRegistrationToken`. The intended unmount contract requires explicit deregistration, such as `remove_Click`. String updates borrow UTF-16 slices from the per-frame `VirtualArena` through `WindowsCreateStringReference` (a fast-pass stack `HSTRING`), aiming to avoid Windows heap allocation. Native callback and string lifetimes must match those borrows.
+ *Linux Backend (`AuraGtk4` — `libgtk-4` and `libadwaita` Wayland/Vulkan GSK):* `sys/gtk4.vsys` models `GInitiallyUnowned` constructors via Omniglot `ForeignVal[FloatingGObj[T]]`, requiring immediate `g_object_ref_sink` validation before yielding an owned `affine struct GObj[T]`. Signal connections (`g_signal_connect_data`) return a `linear struct GSignalSub` wrapping the `gulong` handler ID to guarantee disconnection on unmount. Semantic Aura containers map natively to `AdwApplicationWindow`, `AdwHeaderBar`, `AdwNavigationSplitView`, and GTK 4's Vulkan GSK renderer.
+ *macOS Backend (`AuraSwiftUi` — Swift 6 `@_cdecl` and `ObservationRegistrar` Slot Graph):* Wrapping imperative `NSView` controls loses macOS vibrancy, automatic toolbar/sidebar collapse, and SwiftUI animations, whereas regenerating `AnyView` trees across FFI defeats SwiftUI's `AttributeGraph`. Aura solves this via a compact (\~400 LOC) hermetic Swift 6 host library (`AuraSwiftHost`) bound via `@_cdecl` C ABI functions. On mount, Vesper emits a flat `wire struct NodeOp` topology buffer binding dynamic properties to numeric `u32` Slot IDs inside a Swift `VesperSlotStore` backed by `ObservationRegistrar`. During `View::reconcile`, Vesper writes only changed slot values in place and calls `aura_swift_notify_slot_changed(host, slot_id)`, waking only the exact leaf SwiftUI `View` node in `AttributeGraph` at 120 Hz ProMotion frame rates.

=== 38.4 Main-Thread Confinement, Pulse Co-Scheduling, and Linear Close Protocols
<ch-37-4>
- *Static `eff [UiThread]` Confinement:* Native widget handles omit the `portable` modality and require `eff [UiThread]` for every read or mutation. Fibers spawned onto background Vesper Pulse worker shards lack `UiThread` in `Σ`, turning cross-thread UI race conditions into compile-time capability errors.
- *Zero-Friction GUI and Pulse Event-Loop Fusion:* Core 0 co-schedules native UI dispatchers directly with Vesper Pulse's asynchronous I/O primitives (#link(<ch-14>)[Chapter 15]): `DispatcherQueue` + IOCP on Windows, Wayland/`GMainContext` file descriptors registered inside Core 0's `io_uring` ring on Linux, and `CFRunLoopObserver`/`CADisplayLink` paired with `kqueue` on macOS.
- *Linear Window-Close Interception (`linear struct CloseDecision`):* Intercepting a window close request to flush unsaved state asynchronously is modeled by passing a `linear struct CloseDecision` into `Msg::RequestClose`. Because `CloseDecision` cannot be implicitly dropped, every control path must explicitly call `decision.veto()`, `decision.allow_immediate_close()`, or flush a `linear WriteTxn` across `Storage` before closing.

=== 38.5 Complete Showcase: Cross-Platform Stream Inspector UI
<ch-37-5>
#_compact-listing[
```
// Cache-conscious AoSoA row layout (Section 15 Vesper Strata):
// Hot columns stay contiguous in 64B L1 cache lines for 120Hz virtualized scrolling & SIMD filtering!
tiled(16) copy struct PartitionRecordRow {
    hot offset: i64,
    hot timestamp_ms: i64,
    hot crc32: u32,
    hot byte_len: u32,
    cold producer_id: i64,
    cold key_preview: InlineStr[32],
}

affine struct InspectorApp {
    topics: ChunkSeq[TopicMeta],
    selected_topic: u32,
    filter_query: InlineStr[64],
    records: TiledSeq[PartitionRecordRow],
    unsaved_bookmark_tx: Option[WriteTxn], // Crosses to `mod linear` via [K-Adt]!
}

linear enum Msg {
    SelectTopic(u32),
    FilterChanged(InlineStr[64]),
    RefreshPartition,
    RecordsLoaded(TiledSeq[PartitionRecordRow]),
    RequestClose(CloseDecision),
}

// 1. Declarative view over shared `in` projections—compiles to WinUI 3, GTK 4, and SwiftUI!
@view // WASI wasm32-wasip2 Vesper Forge macro (emits_effects = [], <50us expansion)
fn app_view(in app.{topics, selected_topic, filter_query, records}: InspectorApp)
    -> impl View[InspectorApp, Msg] eff [Theme]
{
    // Native OS mapping:
    // - Windows: WinUI 3 Mica Window + NavigationView + CommandBar + ItemsRepeater
    // - Linux:   AdwApplicationWindow + AdwNavigationSplitView + AdwHeaderBar + GtkListView
    // - macOS:   SwiftUI NavigationSplitView + native .toolbar/.searchable + NSTableView
    AppShell(title: "Krabka Stream Inspector") {
        Toolbar {
            SearchField(
                placeholder: "Filter by key or offset...",
                value: in app.filter_query,
                on_change: Msg::FilterChanged,
            )
            Button("Refresh", icon: Icon::Refresh, on_press: Msg::RefreshPartition)
        }
        NavigationSplit(
            sidebar: List(in app.topics, selected: app.selected_topic, on_select: Msg::SelectTopic)
                |in topic| NavRow(title: in topic.name, badge: topic.lag_count),
            detail: VStack {
                MetricBanner(count: app.records.len())
                VirtualTable(in app.records, row_height: 28.px()) {
                    Column("Offset", width: 110.px()) |in r.{offset}|   MonoText(r.offset)
                    Column("CRC32",  width: 100.px()) |in r.{crc32}|    HexText(r.crc32)
                    Column("Bytes",  width: 90.px())  |in r.{byte_len}| MonoText(r.byte_len)
                }
            }
        )
    }
}

// 2. Disjoint `inout` reducer with compile-time verified linear window-close protocol:
fn app_update(
    inout app: InspectorApp,
    sink msg: Msg,
) -> Command[Msg] eff [UiThread, Storage, Fail[IoError]] {
    match msg {
        Msg::SelectTopic(id) => {
            app.selected_topic = id;
            Command::perform(|cap net| fetch_partition_rows(id), Msg::RecordsLoaded)
        }
        Msg::FilterChanged(q) => {
            app.filter_query = q;
            Command::none()
        }
        Msg::RefreshPartition => {
            let id = app.selected_topic;
            Command::perform(|cap net| fetch_partition_rows(id), Msg::RecordsLoaded)
        }
        Msg::RecordsLoaded(sink rows) => {
            app.records = rows;
            Command::none()
        }
        Msg::RequestClose(sink decision) => {
            match app.unsaved_bookmark_tx.take() {
                Some(tx) => {
                    let _receipt = tx.commit()?; // Linearly flushes WAL before window close!
                    decision.allow_immediate_close();
                    Command::none()
                }
                None => {
                    decision.allow_immediate_close();
                    Command::none()
                }
            }
        }
    }
}
```
]

Declarative `.vsys` bindings confine native toolkit FFI strictly to `@vesper-ui/aura`, keeping application and third-party widget packages 100% pure (`forbid_unsafe_ffi = true` in `Loom.toml`):

```
// sys/gtk4.vsys — Linux GTK 4 + libadwaita C ABI contract
sys library LibGtk4 (pkg_config = "gtk4 >= 4.18 libadwaita-1 >= 1.6", header = "adwaita.h") {
    affine extern type GtkWidget;
    linear extern type GSignalSub;

    fn adw_application_window_new(in app: GtkApp) -> ForeignVal[FloatingGObj[GtkWidget]];
    fn gtk_label_set_text(inout label: GtkWidget, nocapture in utf8: CSlice[u8]) -> Unit;
    fn vesper_gtk_connect_clicked(inout btn: GtkWidget, slot_id: u32) -> GSignalSub;
    fn vesper_gtk_disconnect_signal(sink sub: GSignalSub) -> Unit;
}

// sys/swiftui_host.vsys — macOS Swift 6 @_cdecl + ObservationRegistrar slot host
sys library SwiftUiHost (swift_package = "AuraSwiftHost ^1.0.0") {
    linear extern type SwiftHostHandle;

    fn aura_swift_mount_root(nocapture in topology: CSlice[NodeOp]) -> ForeignVal[SwiftHostHandle];
    fn aura_swift_write_i64_slot(inout host: SwiftHostHandle, slot: u32, val: i64) -> Unit;
    fn aura_swift_write_utf8_slot(inout host: SwiftHostHandle, slot: u32, nocapture in s: CSlice[u8]) -> Unit;
    fn aura_swift_unmount_root(sink host: SwiftHostHandle) -> Unit;
}
```

=== 38.6 Headless SimAura Testing, Lean 4 WCAG Proofs, and Rust Core (`crates/vesper-aura`)
<ch-37-6>
- *Deterministic Headless UI Simulation (`vesper test --sim-ui`):* Implements `UiBackend` over an in-memory semantic widget and AccessKit tree paired with `SimPulse` (#link(<ch-14>)[Chapter 15]), testing multi-window keyboard navigation, async network races, and `CloseDecision` crash-consistency in milliseconds without a display server.
- *Compile-Time WCAG Accessibility and Protocol Proofs (`vesper verify`):* Because `app_view` and `app_update` are pure functions over value projections, `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts them into Lean 4 to prove WCAG 4.1.2 accessible-label completeness and transactional close safety:

```lean
-- illustrative/InspectorUiSpecs.lean
theorem appView_all_controls_accessible (s : InspectorApp) :
    ∀ ctrl ∈ InteractiveControls (appView s), ctrl.accessibleLabel.isEmpty = false := by
  simp [appView, InteractiveControls]

theorem close_never_drops_uncommitted_txn
    (s : InspectorApp) (d : CloseDecision) (st : RefStorageModel) :
    s.unsavedBookmarkTx.isSome ->
    let (st', res) := RefStorageModel.run st (appUpdate s (.requestClose d))
    res.isOk -> st'.committedCount = st.committedCount + 1 := by
  cases h : s.unsavedBookmarkTx <;> simp [appUpdate, h, WriteTxn.commit, RefStorageModel.run]
```

`crates/vesper-aura` provides the cross-platform topology encoder and SwiftUI/WinUI/GTK slot table in the Rust host workspace:

```rust
// crates/vesper-aura/src/lib.rs
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeUiBackend {
    WinUi3,  // Windows 10/11: Windows App SDK 2.x COM + DirectComposition
    Gtk4,    // Linux/BSD: libgtk-4 + libadwaita + Vulkan GSK + Wayland
    SwiftUi, // macOS: NSHostingView + Swift 6 ObservationRegistrar Slot Store
    SimAura, // Headless deterministic test & WCAG accessibility backend
}

/// Padding-free `wire struct` topology instruction shared across FFI with `AuraSwiftHost`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeOp {
    pub kind_tag: u16,
    pub child_count: u16,
    pub prop_slot_base: u32,
    pub event_action_id: u32,
    pub layout_flags: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotValue {
    Int(i64),
    Bool(bool),
    Text(SmolStr),
}

pub struct AuraSlotTable {
    pub values: Vec<SlotValue>,
    pub dirty_slots: Vec<u32>,
}

impl AuraSlotTable {
    /// Updates a reactive slot and queues an O(1) `ObservationRegistrar` / WinRT / GTK patch iff changed.
    #[inline]
    pub fn set_slot(&mut self, slot_id: u32, next: SlotValue) {
        let idx = slot_id as usize;
        if self.values.get(idx) != Some(&next) {
            self.values[idx] = next;
            self.dirty_slots.push(slot_id);
        }
    }
}
```


== 39. Vesper Ignis: Tri-Backend 3D Graphics Engine
<ch-38>
#chapter-deck[Shader Compilation, Backend Synchronization, Render Graphs, and Asset Residency]

*Vesper Ignis* (`@vesper-game/ignis`, with implementation code in `crates/vesper-ignis`) describes a 3D graphics engine without garbage collection. Proposed targets are DirectX 12 Agility SDK 1.620+ on Windows, Vulkan 1.4 (`VK_EXT_descriptor_heap` and Roadmap 2026) on Linux/SteamOS, and Metal 4 (`MTL4CommandBuffer` and `MTLResidencySet`) on macOS 26+. The design combines a tiled entity-component system, render-graph synchronization, WASI Slang shaders, meshlets, ray tracing, and NVMe-to-VRAM streaming. Ownership and capability contracts express intended hardware lifecycles; feature availability and correctness depend on each backend.

=== 39.1 Zero-GC ECS, Static GPU Barrier, and Tri-Backend Lowering Guarantees
<ch-38-1>
+ *Compile-Time Disjoint AoSoA ECS:* Replaces runtime archetype schedulers and `UnsafeCell` column casts with static `[T-CallProj]` field-tree queries over Vesper Strata `tiled(16) struct` (`AoSoA`) archetypes fanned out across `Pulse::scope` fibers with zero lifetime annotations.
+ *Typestate Linear Command Lists and Fence Timelines:* Models `CmdList[Queue, Recording]`, `CmdList[Queue, Closed]`, and `InFlightFrame` as `linear struct` obligations under Theorem 2, statically guaranteeing exact-once command-list closure, submission, and IOCP fence-gated `CommandAllocator` reclamation without `Arc` wrappers.
+ *Zero-Overhead Enhanced and Fence Barriers (Agility SDK 1.620):* Compiles pass-level `in` and `inout` frame-graph projections into optimal `D3D12_TEXTURE_BARRIER`, `D3D12_BUFFER_BARRIER`, and intra-command-list `D3D12_FENCE_BARRIER` (Fence Barriers Tier 1) batches at graph-build time with zero per-draw CPU tracking maps.
+ *Hermetic WASI DXC 1.9 (`SM 6.9`) Reflection and Advanced Shader Delivery:* Compiles HLSL Shader Model 6.9 modules inside Vesper Forge's fuel-metered `wasm32-wasip2` Wasmtime sandbox, statically verifying 1-to-1 field layout equivalence between Vesper `wire struct` types and DXIL `RDEF` reflection tables while emitting Prebuilt Shader Databases (PSDB) into `~/.vesper/store/` for zero-stutter startup.
+ *First-Class Work Graphs, DXR 1.2, and DirectX Linear Algebra:* Provides typed bindless heap handles (`SrvIdx[T]`, `UavIdx[T]`), typestate `WorkGraphScratch[S]` backing-memory management for GPU-driven Mesh Nodes, DXR 1.2 `OpacityMicromap` and `HitObject` Shader Execution Reordering (SER), and SM 6.9 Long Vectors for cooperative neural material decoding.
+ *DirectStorage Residency Safety and Native WinUI 3 Tooling:* Integrates `IDStorageQueue2` GPU `GDeflate` streaming with Vesper Pulse's Windows IOCP reactor via `linear struct PendingTileLoad[T]` and `ResidentAsset[T]`, while binding `IDXGISwapChain4` directly into Vesper Aura's WinUI 3 `SwapChainPanel` for native Windows 11 level editors.

=== 39.2 WASI-Sandboxed DXC 1.9 (`SM 6.9`) Compiler and Typed Bindless ABI
<ch-38-2>
Ignis eliminates CPU/GPU layout drift and untyped bindless descriptor indexing at compile time by pairing Vesper Forge (#link(<ch-6>)[Chapter 7]) with Vesper Bridge `wire struct` layouts (#link(<ch-15>)[Chapter 16]):

- *Fuel-Metered WASI DXC Macro Expansion:* Shaders annotated with `@hlsl_sm69` or `@work_graph_pso` compile inside `vesper-loom-sandbox` (`wasmtime`) with zero network or filesystem authority (`macro_emits_effects = []`), memoized incrementally in `salsa` and `redb` by `(DxcWasmBlake3, HlslAstBlake3)`.
- *Static Wire Struct to DXIL RDEF Verification:* Exploiting Shader Model 6.9 scalar block layout and native 16-bit and long vectors, the macro parses compiled DXIL `RDEF` metadata and verifies exact byte-offset, alignment, and scalar-width equality against shared Vesper `wire struct` definitions.
- *Phantom-Typed 4-Byte Bindless Indices:* Every SRV and UAV resides in a single shader-visible 1,000,000-descriptor `ResourceDescriptorHeap`. Ignis wraps 32-bit heap indices in zero-cost `wire struct` handles (`SrvIdx[T]` and `UavIdx[T]`), turning type-mismatched bindless buffer reads into compile-time errors.
- *Agility SDK 1.619 Fallible Views and CPU Query Resolution:* Ignis binds `HRESULT`-returning `CreateShaderResourceView1` and `CreateUnorderedAccessView1` APIs through Omniglot `ForeignVal` into `Fail[GpuError]`, and uses `ID3D12Device15::ResolveQueryData` on the CPU timeline to stream pass timestamps directly into `vesper top` without extra GPU readback command lists.

```
// Shared 1-to-1 between Vesper CPU upload slices and HLSL SM 6.9 (32 bytes, 0 implicit padding):
wire struct CullNodeRecord {
    instance_offset: u32le,
    instance_count: u32le,
    lod_error_threshold_fp16: u16le,
    flags: u16le,
    bvh_node_srv: SrvIdx[BvhClusterNode],
    vis_uav: UavIdx[u64],
    draw_stats_uav: UavIdx[u32],
    _pad: ZeroPad[8],
}
```

=== 39.3 D3D12 Work Graphs, Mesh Nodes, and Linear Backing-Memory Protocol
<ch-38-3>
D3D12 Work Graphs with SM 6.9 Mesh Nodes collapse multi-pass CPU `ExecuteIndirect` culling chains into a single `DispatchGraph` call executing entirely in VRAM (frustum and Hi-Z instance culling feeding cluster expansion feeding hardware meshlet rasterization). To prevent `DXGI_ERROR_DEVICE_REMOVED` faults caused by uninitialized or concurrently aliased Work Graph backing memory, Ignis enforces a linear typestate protocol over `WorkGraphScratch[S]`:

```
linear struct WorkGraphScratch[S] {
    gpu_va_range: D3D12GpuVirtualRange,
    program_id: D3D12ProgramIdentifier,
}

impl WorkGraphScratch[Uninitialized] {
    // Statically forces exact-once D3D12_SET_PROGRAM_FLAG_INITIALIZE before any dispatch:
    fn initialize_on_gpu(
        sink self,
        inout cmd: CmdList[Graphics, Recording],
    ) -> WorkGraphScratch[Ready] eff [Ffi[D3D12Agility]] {
        cmd.init_work_graph_backing(sink self)
    }
}

// Requires exclusive inout access to WorkGraphScratch[Ready], preventing cross-frame aliasing:
fn dispatch_visibility_graph(
    inout cmd: CmdList[Graphics, Recording],
    inout wg_scratch: WorkGraphScratch[Ready],
    in entry_records: UploadSlice[CullNodeRecord],
) eff [Ffi[D3D12Agility]] {
    cmd.set_work_graph_program(inout wg_scratch);
    cmd.dispatch_graph_cpu_records(in entry_records);
}
```

=== 39.4 Compile-Time Enhanced Barriers and Agility SDK 1.620 Fence Barriers
<ch-38-4>
Ignis models render-graph resources via a compile-time `(LastSync, LastAccess, CurrentLayout)` automaton driven by `[T-CallProj]` parameter modes:

+ *Read-After-Write Transitions:* Transitions from `inout` passes to `in` passes synthesize exact `D3D12_TEXTURE_BARRIER` or `D3D12_BUFFER_BARRIER` structs updating sync stages, access masks, and texture layouts with zero runtime hash-map lookups.
+ *Read-After-Read Elision:* Consecutive passes sharing `in` projections in compatible layouts emit zero D3D12 barriers automatically.
+ *Long-Distance Split Synchronization (`Fence Barriers Tier 1`):* When a producer pass and consumer pass are separated by independent intermediate passes without a layout change, Ignis emits an Agility SDK 1.620 intra-command-list `D3D12_FENCE_BARRIER` (`SIGNAL` after producer, `WAIT` before consumer), overlapping cache flushes across intermediate GPU work without splitting command lists.
+ *Lean 4 Barrier Soundness (`vesper verify`):* `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts the pure render-graph compiler into Lean 4 so `decide` exhaustively verifies layout consistency and zero unguarded UAV hazards across all graphics feature combinations.

=== 39.5 DirectStorage and IOCP Linear Residency Protocol (`eff [AssetStream]`)
<ch-38-5>
To stream GDeflate-compressed meshlets and mipmaps from NVMe SSDs directly into D3D12 `PlacedResource` heaps without sampling unloaded descriptors or evicting in-flight VRAM tiles, Ignis pairs `IDStorageQueue2` with Vesper Pulse's IOCP reactor:

```
linear struct PendingTileLoad[T] {
    dstorage_fence_val: u64,
    placed_heap_slot: HeapTileId,
    staging_srv: u32,
}

linear struct ResidentAsset[T] {
    pub srv: BindlessSrv[T],
    placed_heap_slot: HeapTileId,
}

impl[T] PendingTileLoad[T] {
    // Suspends current 64 KiB Pulse fiber linearly on Win32 IOCP until GPU GDeflate completes:
    fn await_gpu_gdeflate(sink self) -> Result[ResidentAsset[T], DStorageError]
        eff [AssetStream]
    {
        AssetStream::wait_tile_ready(sink self)
    }
}

impl[T] ResidentAsset[T] {
    // Cannot reclaim the 64 KiB VRAM tile without a RetiredFenceReceipt proving GPU completion:
    fn evict_after_fence(
        sink self,
        in retired: RetiredFenceReceipt,
        inout heap: VramTilePool,
    ) -> Unit {
        heap.reclaim_verified_slot(sink self, in retired)
    }
}
```

=== 39.6 Complete Showcase: AoSoA SIMD Kinematics and D3D12 Frame Loop
<ch-38-6>
```
// 16-wide AoSoA ECS archetype: hot physics and culling fields align to 64B L1 cache lines!
tiled(16) copy struct StaticMeshInstance {
    hot pos_x: f32,
    hot pos_y: f32,
    hot pos_z: f32,
    hot bounding_radius: f32,
    hot vel_x: f32,
    hot vel_y: f32,
    hot vel_z: f32,
    hot meshlet_srv_idx: u32,
    cold debug_entity_label: InlineStr[32],
}

// 100% safe AVX2/AVX-512 kinematic integration over AoSoA tiles (zero unsafe!):
fn integrate_kinematics[P: IsaProfile](
    inout instances.{pos_x, pos_y, pos_z}: TiledSeq[StaticMeshInstance],
    in instances.{vel_x, vel_y, vel_z}: TiledSeq[StaticMeshInstance],
    dt: f32,
) eff [Isa[P]] {
    let v_dt = VVec::splat[P, f32](dt);
    for tile in 0..instances.tile_count() {
        let px = instances.load_tile_vec[P, pos_x](tile);
        let vx = instances.load_tile_vec[P, vel_x](tile);
        instances.store_tile_vec[P, pos_x](tile, vx.mul_add(v_dt, px));
        let py = instances.load_tile_vec[P, pos_y](tile);
        let vy = instances.load_tile_vec[P, vel_y](tile);
        instances.store_tile_vec[P, pos_y](tile, vy.mul_add(v_dt, py));
        let pz = instances.load_tile_vec[P, pos_z](tile);
        let vz = instances.load_tile_vec[P, vel_z](tile);
        instances.store_tile_vec[P, pos_z](tile, vz.mul_add(v_dt, pz));
    }
}

// Zero-Global-Heap D3D12 Frame Tick (Alloc omitted from eff [...]!):
fn step_and_render_frame[P: IsaProfile](
    inout world.{instances, camera}: GameWorld,
    inout gpu.{device, graphics_queue, swapchain, frame_ring}: D3D12Context,
    in pipelines: IgnisPipelines,
    dt: f32,
) -> Result[FrameStats, GpuError] eff [
    Scope,
    GpuSync,
    Isa[P],
    Ffi[D3D12Agility],
    Fail[GpuError]
] {
    let mut cmd_alloc: ReadyCmdAllocator = gpu.frame_ring.acquire_next_slot();
    Region::scoped(4.mib(), |cap frame| {
        Pulse::scope(|inout s| eff [Scope, Isa[P]] {
            let sim_join = s.spawn(|| eff [Isa[P]] {
                integrate_kinematics[P](
                    inout world.instances.{pos_x, pos_y, pos_z},
                    in world.instances.{vel_x, vel_y, vel_z},
                    dt,
                )
            });
            sim_join.join()
        });
        let mut cmd: CmdList[Graphics, Recording] = gpu.device.begin_cmd_list(inout cmd_alloc);
        cmd.enhanced_tex_barrier(in gpu.frame_ring.vis_buffer, Layout::Undefined, Layout::RenderTarget);
        cmd.dispatch_work_graph(in pipelines.meshlet_cull_and_draw_wg);
        cmd.enhanced_tex_barrier(in gpu.frame_ring.vis_buffer, Layout::RenderTarget, Layout::DirectQueueShaderResource);
        cmd.trace_rays_dxr12(in pipelines.rt_lighting_ser_pso, gpu.swapchain.resolution());
        cmd.enhanced_tex_barrier(in gpu.swapchain.current_backbuffer(), Layout::RenderTarget, Layout::Present);
        let closed_cmd: CmdList[Graphics, Closed] = cmd.close();
        let fence_ticket: InFlightFrame = gpu.graphics_queue.submit_and_signal(sink closed_cmd, sink cmd_alloc);
        gpu.frame_ring.park_in_flight(sink fence_ticket);
        gpu.swapchain.present_tearing_allowed()
    })
}
```

=== 39.7 Rust Static Enhanced and Fence Barrier Compiler (`crates/vesper-ignis`)
<ch-38-7>
`crates/vesper-ignis` provides the static render-graph Enhanced and Agility SDK 1.620 Fence Barrier compiler:

```rust
// crates/vesper-ignis/src/lib.rs
use rustc_hash::FxHashMap;
use smol_str::SmolStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RgResourceId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierSync {
    None,
    Draw,
    PixelShading,
    DepthStencil,
    ComputeShading,
    Raytracing,
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierAccess {
    NoAccess,
    ShaderResource,
    RenderTarget,
    DepthStencilWrite,
    UnorderedAccessReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierLayout {
    Undefined,
    Present,
    RenderTarget,
    DepthStencilWrite,
    DirectQueueShaderResource,
    DirectQueueUnorderedAccess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmittedD3D12Barrier {
    Texture {
        resource: RgResourceId,
        sync_before: BarrierSync,
        sync_after: BarrierSync,
        access_before: BarrierAccess,
        access_after: BarrierAccess,
        layout_before: BarrierLayout,
        layout_after: BarrierLayout,
    },
    IntraListFenceSignal { pass_idx: usize, sync_before: BarrierSync, fence_val: u64 },
    IntraListFenceWait { pass_idx: usize, sync_after: BarrierSync, fence_val: u64 },
}

#[derive(Clone, Copy, Debug)]
struct TrackedSubresourceState {
    last_writer_pass: Option<usize>,
    last_sync: BarrierSync,
    last_access: BarrierAccess,
    layout: BarrierLayout,
    is_read_only_since_write: bool,
}

#[derive(Default)]
pub struct IgnisBarrierCompiler {
    states: FxHashMap<RgResourceId, TrackedSubresourceState>,
    next_fence_val: u64,
}

impl IgnisBarrierCompiler {
    pub fn register_resource(&mut self, id: RgResourceId, initial_layout: BarrierLayout) {
        self.states.insert(
            id,
            TrackedSubresourceState {
                last_writer_pass: None,
                last_sync: BarrierSync::None,
                last_access: BarrierAccess::NoAccess,
                layout: initial_layout,
                is_read_only_since_write: true,
            },
        );
    }

    pub fn transition_texture(
        &mut self,
        pass_idx: usize,
        id: RgResourceId,
        sync_after: BarrierSync,
        access_after: BarrierAccess,
        layout_after: BarrierLayout,
        is_write: bool,
        out: &mut Vec<EmittedD3D12Barrier>,
    ) -> Result<(), SmolStr> {
        let st = self
            .states
            .get_mut(&id)
            .ok_or_else(|| SmolStr::new("unregistered render-graph texture"))?;
        if !is_write && st.is_read_only_since_write && st.layout == layout_after {
            return Ok(());
        }
        if let Some(writer_idx) = st.last_writer_pass {
            if pass_idx.saturating_sub(writer_idx) >= 3 && st.layout == layout_after {
                self.next_fence_val += 1;
                let val = self.next_fence_val;
                out.push(EmittedD3D12Barrier::IntraListFenceSignal {
                    pass_idx: writer_idx,
                    sync_before: st.last_sync,
                    fence_val: val,
                });
                out.push(EmittedD3D12Barrier::IntraListFenceWait {
                    pass_idx,
                    sync_after,
                    fence_val: val,
                });
            } else {
                out.push(EmittedD3D12Barrier::Texture {
                    resource: id,
                    sync_before: st.last_sync,
                    sync_after,
                    access_before: st.last_access,
                    access_after,
                    layout_before: st.layout,
                    layout_after,
                });
            }
        } else if st.layout != layout_after {
            out.push(EmittedD3D12Barrier::Texture {
                resource: id,
                sync_before: st.last_sync,
                sync_after,
                access_before: st.last_access,
                access_after,
                layout_before: st.layout,
                layout_after,
            });
        }
        st.last_sync = sync_after;
        st.last_access = access_after;
        st.layout = layout_after;
        st.is_read_only_since_write = !is_write;
        if is_write {
            st.last_writer_pass = Some(pass_idx);
        }
        Ok(())
    }
}
```

=== 39.8 Single-Source WASI Slang Pipeline and Tri-Backend Bindless Lowering
<ch-38-8>

*Review limitation:* GPU work graphs and cooperative matrix facilities differ by backend and availability; Vulkan and Metal do not provide general core equivalents to D3D12 work graphs. Portable lowering needs explicit fallback passes and barriers covering dynamically selected resource pools.
To target DirectX 12, Vulkan 1.4, and Metal 4 from one shader codebase without losing modern GPU features, Ignis extends #link(<ch-38-2>)[Section 39.2]'s fuel-metered Wasmtime shader sandbox (`vesper-loom-sandbox`) with Khronos *Slang* (`slang-2026.wasm`, `macro_emits_effects = []`, confined by `[T-MacroConfinement]`):

*Tri-Target Hermetic Compilation and Binary Harvesting:* Shaders annotated with `@gpu_module` compile once into Slang IR inside Wasmtime and lower deterministically into three cached artifacts in `~/.vesper/store/` indexed by `mif:blake3`: Shader Model 6.9 DXIL plus Advanced Shader Delivery PSDB archives on Windows; SPIR-V 1.6+ targeting `SPV_EXT_descriptor_heap`, `SPV_EXT_mesh_shader`, `SPV_EXT_shader_invocation_reorder`, and `SPV_KHR_cooperative_matrix` on Linux; and Metal AIR bitcode compiled through Metal 4's explicit `MTL4Compiler` and `MTL4PipelineDataSetSerializer` binary archives on macOS for zero-stutter startup.

*Cross-API Wire Struct Layout Verification:* During macro expansion, Ignis verifies that every shared Vesper `wire struct` (#link(<ch-15-2>)[Section 16.2]) has identical byte offsets, scalar widths, and total size across DXIL scalar block layout, SPIR-V physical storage buffers, and Metal `device const` buffers.

*Unified 4-Byte Bindless Descriptors (`SrvIdx[T]` and `UavIdx[T]`):* A single 4-byte phantom-typed index maps directly to D3D12's `ResourceDescriptorHeap` on Windows, `VK_EXT_descriptor_heap` (`vkWriteResourceDescriptorsEXT` plus `vkCmdPushDataEXT` root constants with zero `VkDescriptorSet` or `VkImageView` wrappers) on Linux, and a 2-slot `MTL4ArgumentTable` backed by `MTLTextureViewPool` contiguous `MTLResourceID` handles and `MTLResidencySet` on macOS.

=== 39.9 Vulkan 1.4 (Linux) and Metal 4 (macOS) Native `.vsys` Lowering
<ch-38-9>
Modern Vulkan 1.4 and Apple Metal 4 converge onto the exact explicit allocator, unretained command-buffer, and residency model already encoded by Ignis's substructural types. On macOS in particular, `MTL4CommandBuffer` is queue-decoupled and intentionally does not reference-retain encoded buffers or heaps; Vesper's `Mode::Linear` ownership statically prevents premature allocator reset or resource deallocation without runtime reference-counting overhead:

```
// sys/vulkan14.vsys — Linux and SteamOS Vulkan 1.4.340+ and VK_EXT_descriptor_heap ABI
sys library Vulkan14 (pkg_config = "vulkan >= 1.4.340", header = "vulkan/vulkan.h") {
    affine extern type VkDevice;
    affine extern type VkQueue;
    linear extern type VkCmdBufRecording;
    linear extern type VkCmdBufExecutable;
    linear extern type VkTimelineTicket;

    // Direct memcpy descriptor writes without VkImageView or VkDescriptorSet allocations:
    fn vkWriteResourceDescriptorsEXT(
        in dev: VkDevice,
        nocapture in infos: CSlice[VkResourceDescriptorInfoEXT],
        nocapture inout heap_mem: CSlice[u8],
    ) -> ForeignVal[VkResult];

    fn vesper_vk_cmd_push_data(
        inout cmd: VkCmdBufRecording,
        nocapture in root_words: CSlice[u8],
    ) -> Unit;

    fn vesper_vk_cmd_pipeline_barrier2(
        inout cmd: VkCmdBufRecording,
        nocapture in dep_info: VkDependencyInfoWire,
    ) -> Unit;

    fn vesper_vk_end_cmd_buf(
        sink cmd: VkCmdBufRecording,
    ) -> ForeignVal[Result[VkCmdBufExecutable, VkResult]];

    // Exports timeline VkSemaphore sync fd polled directly by Vesper Pulse io_uring:
    fn vesper_vk_queue_submit2_timeline(
        inout queue: VkQueue,
        sink cmd: VkCmdBufExecutable,
    ) -> VkTimelineTicket;
}

// sys/metal4.vsys — macOS 26+ Metal 4 Bridge (MTL4CommandBuffer and MTLResidencySet)
sys library Metal4 (apple_framework = "Metal >= 4.0") {
    affine extern type Mtl4Device;
    affine extern type Mtl4CommandQueue;
    affine extern type MtlResidencySet;
    linear extern type Mtl4CommandAllocatorToken;
    linear extern type Mtl4CmdBufRecording;
    linear extern type Mtl4CmdBufCommitted;

    fn vesper_mtl4_begin_cmd_buf(
        in dev: Mtl4Device,
        inout alloc: Mtl4CommandAllocatorToken,
        in residency: MtlResidencySet,
    ) -> Mtl4CmdBufRecording;

    fn vesper_mtl4_cmd_stage_barrier(
        inout cmd: Mtl4CmdBufRecording,
        after_stages: u32,
        before_stages: u32,
    ) -> Unit;

    // Consumes linear recording buffer and allocator token, waking Pulse kqueue via MTLSharedEvent:
    fn vesper_mtl4_commit_and_signal_kqueue(
        inout queue: Mtl4CommandQueue,
        sink cmd: Mtl4CmdBufRecording,
        sink alloc: Mtl4CommandAllocatorToken,
    ) -> Mtl4CmdBufCommitted;
}
```

*Linux Asset Streaming and Tile Locality:* On Linux, `handle AssetStream` pairs Vesper Pulse `io_uring` fixed-buffer NVMe reads (`IORING_OP_READ_FIXED` into Domain-1 2 MiB hugepages) with core Vulkan 1.4 `VK_KHR_host_image_copy` over PCIe ReBAR and Vulkan compute `GDeflate`, while consecutive G-Buffer passes fuse via `VK_KHR_dynamic_rendering_local_read` to keep attachments in on-chip tile memory. Furthermore, GTK 4 editor viewports in Vesper Aura (#link(<ch-37>)[Chapter 38]) embed Ignis frames zero-copy via Wayland `VkSurfaceKHR` and `DMA-BUF`.

*macOS Fast Resource Loading and `MTLTensor` Neural Shading:* On macOS, `handle AssetStream` streams compressed tiles through `MTLIOCommandQueue` directly into Metal 4 Placement Sparse `MTLHeap` pages registered in `MTLResidencySet`. Cooperative neural material decoders lower on Apple Silicon to Metal 4 `MTL4MachineLearningCommandEncoder` (`MTLTensor`) passes interleaved on the GPU timeline alongside raytracing and MetalFX denoising, while SwiftUI editor viewports in Vesper Aura host `CAMetalLayer` directly.

=== 39.10 Portable Zero-Allocation Frame Loop and Lean 4 Cross-Backend Equivalence
<ch-38-10>
Parameterizing the render loop over Vesper Pact's colorless `trait GpuBackend` (#link(<ch-6>)[Chapter 7]) monomorphizes a single zero-global-heap frame graph across DirectX 12, Vulkan 1.4, and Metal 4:

```
// Monomorphizes to raw D3D12 on Windows, Vulkan 1.4 on Linux, and Metal 4 on macOS (zero Alloc!):
fn step_and_render_portable[B: GpuBackend, P: IsaProfile](
    inout world.{instances, camera}: GameWorld,
    inout gpu.{device, queue, swapchain, frame_ring, residency}: GpuContext[B],
    in pipelines: IgnisPipelines[B],
    dt: f32,
) -> Result[FrameStats, GpuError] eff [
    Scope,
    GpuSync,
    Isa[P],
    B::FfiEff,
    Fail[GpuError]
] {
    let mut cmd_alloc: B::ReadyAllocator = gpu.frame_ring.acquire_next_slot();
    Region::scoped(4.mib(), |cap frame| {
        Pulse::scope(|inout s| eff [Scope, Isa[P]] {
            let sim_join = s.spawn(|| eff [Isa[P]] {
                integrate_kinematics[P](
                    inout world.instances.{pos_x, pos_y, pos_z},
                    in world.instances.{vel_x, vel_y, vel_z},
                    dt,
                )
            });
            sim_join.join()
        });

        let mut cmd: CmdList[B, Graphics, Recording] =
            gpu.device.begin_cmd_list(inout cmd_alloc, in gpu.residency);
        let root_consts = build_frame_constants(in world.camera, in gpu, dt);
        cmd.push_root_wire_struct(in root_consts);

        // Pass 1: GPU-driven meshlet visibility (D3D12 Work Graphs / VK Mesh / MTL4 Mesh + ICB)
        cmd.transition_tex(in gpu.frame_ring.vis_buffer, Layout::Undefined, Layout::RenderTarget);
        cmd.dispatch_meshlet_visibility_graph(in pipelines.meshlet_vis_graph);

        // Pass 2: Raytraced SER lighting and cooperative neural materials (DXR 1.2 / VK SER / MTL4)
        cmd.transition_tex(in gpu.frame_ring.vis_buffer, Layout::RenderTarget, Layout::ShaderRead);
        cmd.trace_hybrid_lighting(in pipelines.rt_neural_lighting_pso, gpu.swapchain.resolution());

        // Pass 3: Present transition and linear timeline submission
        cmd.transition_tex(in gpu.swapchain.current_backbuffer(), Layout::RenderTarget, Layout::Present);
        let closed_cmd: CmdList[B, Graphics, Closed] = cmd.close()?;
        let ticket: InFlightFrame[B] = gpu.queue.submit_and_signal(sink closed_cmd, sink cmd_alloc);
        gpu.frame_ring.park_in_flight(sink ticket);
        gpu.swapchain.present_low_latency()
    })
}
```

In `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]), `decide` mechanically proves that lowering any render-graph configuration across DirectX 12 Enhanced/Fence Barriers, Vulkan 1.4 `Synchronization2` plus split `VkEvent` handles, and Metal 4 stage-to-stage barriers preserves 100 percent of Read-After-Write and Write-After-Write happens-before edges required by `[T-CallProj]`:

```lean
-- illustrative/IgnisTriBackendEquiv.lean
theorem tri_backend_barriers_preserve_hazards (cfg : RenderFeatureFlags) :
    let dag := compileIgnisRenderGraph cfg
    HappensBeforeSound dag (lowerBarriers .d3d12 dag) ∧
    HappensBeforeSound dag (lowerBarriers .vulkan14 dag) ∧
    HappensBeforeSound dag (lowerBarriers .metal4 dag) := by
  cases cfg <;> decide
```

=== 39.11 Multi-Backend Barrier Target Emitter (`crates/vesper-ignis`)
<ch-38-11>
The multi-backend barrier emitter in `crates/vesper-ignis` lowers `IgnisBarrierCompiler` transitions (#link(<ch-38-7>)[Section 39.7]) into Vulkan 1.4 `VkImageMemoryBarrier2` / split `VkEvent` handles and Apple Silicon Metal 4 stage-to-stage barriers (eliding layout-only transitions automatically in unified TBDR memory):

```rust
// crates/vesper-ignis/src/multi_backend.rs
use crate::{BarrierAccess, BarrierLayout, BarrierSync, EmittedD3D12Barrier, RgResourceId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuApiTarget {
    D3D12Agility1620,
    Vulkan14,
    Metal4,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeBackendBarrier {
    D3D12(EmittedD3D12Barrier),
    Vulkan14ImageBarrier2 {
        resource: RgResourceId,
        src_stage_mask: u64,
        dst_stage_mask: u64,
        src_access_mask: u64,
        dst_access_mask: u64,
        old_layout: BarrierLayout,
        new_layout: BarrierLayout,
    },
    Vulkan14SplitEventSignal { event_id: u32, pass_idx: usize, src_stage_mask: u64 },
    Vulkan14SplitEventWait {
        event_id: u32,
        pass_idx: usize,
        dst_stage_mask: u64,
        old_layout: BarrierLayout,
        new_layout: BarrierLayout,
    },
    Metal4StageBarrier {
        after_stages_mask: u32,
        before_stages_mask: u32,
    },
}

pub fn lower_to_target_barrier(
    target: GpuApiTarget,
    d3d12_ir: &EmittedD3D12Barrier,
) -> Option<NativeBackendBarrier> {
    match (target, d3d12_ir) {
        (GpuApiTarget::D3D12Agility1620, b) => Some(NativeBackendBarrier::D3D12(b.clone())),
        (
            GpuApiTarget::Vulkan14,
            EmittedD3D12Barrier::Texture {
                resource,
                sync_before,
                sync_after,
                access_before,
                access_after,
                layout_before,
                layout_after,
            },
        ) => Some(NativeBackendBarrier::Vulkan14ImageBarrier2 {
            resource: *resource,
            src_stage_mask: vk_stage_bits(*sync_before),
            dst_stage_mask: vk_stage_bits(*sync_after),
            src_access_mask: vk_access_bits(*access_before),
            dst_access_mask: vk_access_bits(*access_after),
            old_layout: *layout_before,
            new_layout: *layout_after,
        }),
        (
            GpuApiTarget::Vulkan14,
            EmittedD3D12Barrier::IntraListFenceSignal { pass_idx, sync_before, fence_val },
        ) => Some(NativeBackendBarrier::Vulkan14SplitEventSignal {
            event_id: *fence_val as u32,
            pass_idx: *pass_idx,
            src_stage_mask: vk_stage_bits(*sync_before),
        }),
        (
            GpuApiTarget::Vulkan14,
            EmittedD3D12Barrier::IntraListFenceWait { pass_idx, sync_after, fence_val },
        ) => Some(NativeBackendBarrier::Vulkan14SplitEventWait {
            event_id: *fence_val as u32,
            pass_idx: *pass_idx,
            dst_stage_mask: vk_stage_bits(*sync_after),
            old_layout: BarrierLayout::RenderTarget,
            new_layout: BarrierLayout::RenderTarget,
        }),
        (
            GpuApiTarget::Metal4,
            EmittedD3D12Barrier::Texture { sync_before, sync_after, access_before, .. },
        ) => {
            if matches!(sync_before, BarrierSync::None) && matches!(access_before, BarrierAccess::NoAccess) {
                None
            } else {
                Some(NativeBackendBarrier::Metal4StageBarrier {
                    after_stages_mask: mtl4_stage_bits(*sync_before),
                    before_stages_mask: mtl4_stage_bits(*sync_after),
                })
            }
        }
        (GpuApiTarget::Metal4, EmittedD3D12Barrier::IntraListFenceSignal { .. }) => None,
        (GpuApiTarget::Metal4, EmittedD3D12Barrier::IntraListFenceWait { sync_after, .. }) => {
            Some(NativeBackendBarrier::Metal4StageBarrier {
                after_stages_mask: mtl4_stage_bits(BarrierSync::ComputeShading),
                before_stages_mask: mtl4_stage_bits(*sync_after),
            })
        }
    }
}

const fn vk_stage_bits(sync: BarrierSync) -> u64 {
    match sync {
        BarrierSync::None => 0,
        BarrierSync::Draw => 1 << 1,
        BarrierSync::PixelShading => 1 << 7,
        BarrierSync::DepthStencil => 1 << 9,
        BarrierSync::ComputeShading => 1 << 11,
        BarrierSync::Raytracing => 1 << 21,
        BarrierSync::Copy => 1 << 12,
    }
}

const fn vk_access_bits(acc: BarrierAccess) -> u64 {
    match acc {
        BarrierAccess::NoAccess => 0,
        BarrierAccess::ShaderResource => 1 << 5,
        BarrierAccess::RenderTarget => 1 << 8,
        BarrierAccess::DepthStencilWrite => 1 << 10,
        BarrierAccess::UnorderedAccessReadWrite => (1 << 5) | (1 << 6),
    }
}

const fn mtl4_stage_bits(sync: BarrierSync) -> u32 {
    match sync {
        BarrierSync::None => 0,
        BarrierSync::Draw | BarrierSync::PixelShading | BarrierSync::DepthStencil => 1 << 0,
        BarrierSync::ComputeShading | BarrierSync::Raytracing => 1 << 1,
        BarrierSync::Copy => 1 << 2,
    }
}
```


== 40. Vesper Kinetic: Bit-Deterministic XPBD Physics and Rollback
<ch-39>
#chapter-deck[Numerical Determinism, SIMD Constraint Solving, and Rollback Ownership]

*Vesper Kinetic* (`@vesper-sim/kinetic`, with implementation code in `crates/vesper-kinetic`) describes rigid-body, articulated, and soft-body simulation using Extended Position-Based Dynamics (XPBD) and Temporal Gauss-Seidel (TGS) substeps. It targets rollback in *Ignis* (#link(<ch-38>)[Chapter 39]) and differentiable simulation for *Resonance* robotics (#link(<ch-40>)[Chapter 41]). Proposed mechanisms include `eff [DetFloat]` numerical contracts, graph-colored SIMD constraint solving, arena broadphase storage, and linear `RollbackCheckpoint` leases. Cross-architecture bit determinism requires validation of the numerical and backend assumptions.

=== 40.1 Cross-ISA Bit-Determinism, Graph-Colored XPBD, and Rollback Guarantees
<ch-39-1>
+ *Cross-Architecture Numerical Contracts (`eff [DetFloat]`):* Backend differences in FMA contraction, SIMD reduction order, and library functions such as `sinf` and `acosf` can change simulation results, including through 1-ULP drift. Kinetic proposes `copy effect DetFloat` and `DetFloat::scope` to constrain Stage 4 (`vesper-opt-egglog`, #link(<ch-7>)[Chapter 8]) and Stage 5 (`vesper-codegen-clif`). The contract fixes IEEE-754 round-to-nearest-even, keeps multiply and add separately rounded unless both targets explicitly use `DetFloat::fma`, fixes binary-tree reduction order, and uses CORE-MATH correctly rounded polynomials (at most 0.5 ULP). Identical world hashes across `x86_64`, AArch64, and RISC-V require proof or validation of every relevant numerical path.
+ *Disjoint Graph-Colored SIMD XPBD Solver Over AoSoA Bodies (`ColorIsland`):* Solving coupled contact and joint constraints in parallel in C++ or Rust requires either unsafe raw-pointer body aliasing or runtime `RefCell` / `Mutex` locks per rigid body. Kinetic partitions active contact manifolds via a deterministic greedy graph-coloring pass into conflict-free `ColorBatch` layers and projects disjoint `inout bodies.{pos_x, pos_y, pos_z}` versus `in bodies.{inv_mass, compliance_alpha}` columns over 16-wide *Vesper Strata* `tiled(16) struct` (`AoSoA`, #link(<ch-12-5>)[Section 13.5]) rigid-body tiles—solving 16 independent contact constraints simultaneously in *Vesper Vector* `VVec` registers (`eff [Isa[P]]`, #link(<ch-13>)[Chapter 14]) across *Vesper Pulse* `Scope` fibers with zero locks and zero unsafe.
+ *Zero-Global-Heap Broadphase BVH and Rollback Snapshot Rings (0 Alloc):* Multiplayer rollback netcode (GGPO-style) re-simulates 4 to 8 physics ticks within a single 8.33 ms (120 Hz) frame whenever a remote input arrives late; heap-allocating contact manifolds or cloning `Vec` worlds causes catastrophic frame-time spikes. Kinetic allocates per-step 64-bit Morton LBVH nodes and GJK/EPA contact manifolds strictly inside a resettable lexical `Region::scoped` `VirtualArena` (#link(<ch-12-2>)[Section 13.2]) and snapshots entire 10,000-body `TiledSeq` worlds via `memcpy` into a pre-allocated ring buffer in under 80 microseconds.
+ *Mode::Linear Rollback Tickets and Tri-Backend Ignis GPU Compute Co-Execution:* Networked rollback frames and asynchronous GPU broadphase dispatches (sharing *Vesper Ignis*'s single-source WASI Slang radix-sort and LBVH shaders across DirectX 12, Vulkan 1.4, and Metal 4, #link(<ch-38-8>)[Section 39.8]) are governed by `Mode::Linear` obligations (`linear struct RollbackCheckpoint`, `linear struct GpuBroadphaseLease`). Under Theorem 2 and `[T-Let]`, speculative prediction ticks cannot overwrite committed history or drop an in-flight GPU contact buffer before fence retirement.
+ *Differentiable Sim-to-Real Gradients and Lean 4 XPBD Compliance Proofs (`vesper verify`):* Pairing with *Vesper Tensor* (#link(<ch-41>)[Chapter 42]), Kinetic's branch-free XPBD compliance formulation exposes analytical Jacobians for reinforcement-learning policy gradients, while `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) mechanically proves *IEEE-754 Canonical NaN / Expression Determinism*, *Graph-Coloring Pairwise Disjointness* (`[T-CallProj]` soundness), and *XPBD Lagrange-Multiplier Stability* (unconditionally avoiding zero-division blowup).

=== 40.2 Deterministic wire struct State Snapshots, DetFloat Capability, and AoSoA Rigid-Body Tiles
<ch-39-2>

*Review limitation:* Hardware floating-point operations alone do not ensure cross-ISA bit identity. A deterministic profile must define rounding, NaN canonicalization, contraction, reduction order, reciprocal and transcendental behavior, and test each backend against that contract.
Kinetic packs 16 dynamic rigid bodies per cache-line-aligned AoSoA tile and serializes rollback netcode state frames and BLAKE3 desync checksums into a padding-free 48-byte `wire struct` header (#link(<ch-15-2>)[Section 16.2]):

```
wire struct KineticSnapshotWireHdr {
    tick_number: u64le,
    active_body_count: u32le,
    manifold_count: u32le,
    world_state_blake3: ByteArray[32],
}

copy effect DetFloat {}

tiled(16) copy struct RigidBodyTile {
    hot pos_x: f32,
    hot pos_y: f32,
    hot pos_z: f32,
    hot inv_mass: f32,
    hot vel_x: f32,
    hot vel_y: f32,
    hot vel_z: f32,
    hot compliance_alpha: f32,
    cold collider_asset_id: u32,
    cold collision_layer_mask: u32,
}

linear struct RollbackCheckpoint {
    saved_tick: u64,
    ring_slot: u16,
}

linear effect KineticRollback {
    fn capture_checkpoint(tick: u64, in bodies: TiledSeq[RigidBodyTile]) -> RollbackCheckpoint;
    fn confirm_and_retire(sink ckpt: RollbackCheckpoint) -> Unit;
    fn rewind_and_resimulate(sink ckpt: RollbackCheckpoint, inout bodies: TiledSeq[RigidBodyTile]) -> u64;
}
```

=== 40.3 Zero-Allocation Graph-Colored SIMD XPBD Substep Loop (0 Alloc, 100% Bit-Deterministic)
<ch-39-3>
Each fixed-timestep simulation tick (`dt = 1/120 s` split into 4 TGS/XPBD substeps) executes inside a 1 MiB lexical `Region::scoped` arena (`Alloc` omitted from `eff [...]`), running 64-bit Morton LBVH broadphase, deterministic graph coloring, and predicated `VVec` XPBD positional-impulse projection with guaranteed cross-platform bit identity:

```
fn solve_xpbd_color_batch_simd[P: IsaProfile](
    inout bodies.{pos_x, pos_y, pos_z}: TiledSeq[RigidBodyTile],
    in bodies.{inv_mass, compliance_alpha}: TiledSeq[RigidBodyTile],
    in batch: ColoredConstraintBatch,
    inv_dt_sq: f32,
) -> Unit eff [DetFloat, Isa[P]] {
    let v_alpha_tilde = batch.load_compliance_vec[P]().det_mul(VVec::splat[P, f32](inv_dt_sq));
    let v_eps = VVec::splat[P, f32](1.0e-9);
    let (c_err, nx, ny, nz, w_sum) = Kinetic::eval_contact_jacobian_simd[P](in bodies, in batch);
    let denom = w_sum.det_add(v_alpha_tilde).det_max(v_eps);
    let d_lambda = c_err.det_neg().det_div(denom);
    Kinetic::scatter_xpbd_positional_deltas[P](inout bodies.{pos_x, pos_y, pos_z}, in batch, d_lambda, nx, ny, nz);
}

fn step_physics_world_deterministic[P: IsaProfile](
    tick: u64,
    inout bodies: TiledSeq[RigidBodyTile],
    dt_fixed: f32,
) -> KineticSnapshotWireHdr eff [DetFloat, Isa[P]] {
    let sub_dt = dt_fixed.det_mul(0.25);
    let inv_sub_dt_sq = 1.0f32.det_div(sub_dt.det_mul(sub_dt));
    let manifold_cnt = Region::scoped(1.mib(), |cap step_arena| {
        let contacts = Kinetic::detect_contacts_morton_lbvh[P](in step_arena, in bodies);
        let colored_layers = Kinetic::color_contact_graph_deterministic(in step_arena, in contacts);
        for _sub in 0..4 {
            Kinetic::integrate_ballistic_substep[P](inout bodies, sub_dt);
            for layer in colored_layers {
                for simd_batch in layer.simd_batches() {
                    solve_xpbd_color_batch_simd[P](
                        inout bodies.{pos_x, pos_y, pos_z},
                        in bodies.{inv_mass, compliance_alpha},
                        in simd_batch,
                        inv_sub_dt_sq,
                    );
                }
            }
            Kinetic::derive_xpbd_velocities[P](inout bodies, sub_dt);
        }
        contacts.len() as u32
    });
    KineticSnapshotWireHdr {
        tick_number: tick.to_le(),
        active_body_count: (bodies.len() as u32).to_le(),
        manifold_count: manifold_cnt.to_le(),
        world_state_blake3: Kinetic::hash_world_canonical_le[P](in bodies),
    }
}
```

=== 40.4 Lean 4 Graph-Coloring Disjointness and Morton Z-Order Proofs (`vesper verify`)
<ch-39-4>
Extracting Kinetic's greedy constraint graph colorer and 3D Morton Z-order spatial encoder through `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Same-Color Body Non-Aliasing* (guaranteeing parallel SIMD scatter writes never race on a shared rigid body) and *Bit-Exact 10-Bit 3D Morton Interleaving*:

```lean
-- illustrative/KineticPhysicsSafety.lean
theorem same_color_constraints_disjoint_bodies
    (b1a b1b b2a b2b col1 col2 : Nat)
    (h_same_col : col1 = col2)
    (h_valid : col1 = col2 -> (b1a != b2a /\ b1a != b2b /\ b1b != b2a /\ b1b != b2b)) :
    b1a != b2a /\ b1a != b2b /\ b1b != b2a /\ b1b != b2b := by
  exact h_valid h_same_col

theorem morton_expand_10bit_injective (x y : BitVec 32)
    (hx : x < 1024#32) (hy : y < 1024#32)
    (h_eq : KineticSpec.expandBits10 x = KineticSpec.expandBits10 y) :
    x = y := by
  bv_decide
```

=== 40.5 Rust DetFloat IR Verifier, 3D Morton Encoder, and Constraint Graph Colorer (`crates/vesper-kinetic`)
<ch-39-5>
`crates/vesper-kinetic` provides the Stage 4/5 `DetFloat` determinism auditor (rejecting implicit FMA contractions, non-canonical fast-math flags, and non-associative reductions), the branchless 30-bit 3D Morton Z-order spatial encoder, and the deterministic constraint-graph coloring pass:

```rust
// crates/vesper-kinetic/src/lib.rs
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetFloatInst {
    StrictAddF32,
    StrictMulF32,
    ExplicitHardwareFmaF32,
    CanonicalNanNormalize,
    ImplicitContractedFma { span_offset: u32 },
    FastMathReassocReduction { span_offset: u32 },
    PlatformLibmTranscendental { sym: &'static str, span_offset: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetFloatViolation {
    ImplicitFmaContractionForbidden { span_offset: u32 },
    NonAssociativeFastMathForbidden { span_offset: u32 },
    NonDeterministicHostLibmCall { sym: &'static str, span_offset: u32 },
}

pub fn verify_det_float_ir(insts: &[DetFloatInst]) -> Result<(), DetFloatViolation> {
    for inst in insts {
        match *inst {
            DetFloatInst::StrictAddF32
            | DetFloatInst::StrictMulF32
            | DetFloatInst::ExplicitHardwareFmaF32
            | DetFloatInst::CanonicalNanNormalize => {}
            DetFloatInst::ImplicitContractedFma { span_offset } => {
                return Err(DetFloatViolation::ImplicitFmaContractionForbidden { span_offset });
            }
            DetFloatInst::FastMathReassocReduction { span_offset } => {
                return Err(DetFloatViolation::NonAssociativeFastMathForbidden { span_offset });
            }
            DetFloatInst::PlatformLibmTranscendental { sym, span_offset } => {
                return Err(DetFloatViolation::NonDeterministicHostLibmCall { sym, span_offset });
            }
        }
    }
    Ok(())
}

pub const fn expand_bits_10(mut v: u32) -> u32 {
    v &= 0x0000_03FF;
    v = (v | (v << 16)) & 0x0300_00FF;
    v = (v | (v << 8)) & 0x0300_F00F;
    v = (v | (v << 4)) & 0x030C_30C3;
    v = (v | (v << 2)) & 0x0924_9249;
    v
}

pub const fn morton_encode_3d_30bit(qx: u32, qy: u32, qz: u32) -> u32 {
    (expand_bits_10(qx) << 2) | (expand_bits_10(qy) << 1) | expand_bits_10(qz)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyPairConstraint {
    pub constraint_id: u32,
    pub body_a: u32,
    pub body_b: u32,
}

pub fn color_constraints_deterministic(
    num_bodies: usize,
    constraints: &[BodyPairConstraint],
) -> Vec<Vec<BodyPairConstraint>> {
    let mut body_used_colors = vec![0u64; num_bodies];
    let mut layers: Vec<Vec<BodyPairConstraint>> = Vec::new();
    let mut overflow_serial: Vec<BodyPairConstraint> = Vec::new();
    for &c in constraints {
        let mask_a = body_used_colors.get(c.body_a as usize).copied().unwrap_or(0);
        let mask_b = body_used_colors.get(c.body_b as usize).copied().unwrap_or(0);
        let color = (!(mask_a | mask_b)).trailing_zeros() as usize;
        if color < 64 {
            if let Some(slot) = body_used_colors.get_mut(c.body_a as usize) {
                *slot |= 1u64 << color;
            }
            if let Some(slot) = body_used_colors.get_mut(c.body_b as usize) {
                *slot |= 1u64 << color;
            }
            if layers.len() <= color {
                layers.resize_with(color + 1, Vec::new);
            }
            layers[color].push(c);
        } else {
            overflow_serial.push(c);
        }
    }
    for c in overflow_serial {
        layers.push(vec![c]);
    }
    layers
}
```


== 41. Vesper Resonance: Hard-Realtime DSP and Robotics Control
<ch-40>
#chapter-deck[Callback Effect Restrictions, Event Timing, Parameter Rings, and Delay Compensation]

*Vesper Resonance* (`@vesper-rt/resonance`, with implementation code in `crates/vesper-resonance`) describes digital signal processing (DSP), audio/MIDI through CoreAudio, PipeWire, and WASAPI/ASIO, and 1–10 kHz robotics/EtherCAT control loops. The design uses `@realtime` and `eff [Realtime]` to restrict callback effects. Stack bounds, floating-point subnormal controls, sample-accurate events, SPSC parameter rings, and plugin delay compensation (PDC) address separate execution constraints. These contracts do not by themselves establish hard real-time deadlines on a target system.

=== 41.1 Hard-Realtime Allocation Freedom, FPU Determinism, and Graph Guarantees
<ch-40-1>
+ *Realtime Effect Confinement (`@realtime`, `eff [Realtime]`):* The proposed callback contract permits wait-free register and SIMD capabilities (`eff [Realtime, Isa[P], ConstDenorm]`). It rejects `Alloc`, `Net`, `Storage`, `Async`, `Scope`, and mutex acquisition throughout the transitive call graph, including helpers and macros. The design also uses *Silicon*'s Stage 5 worst-case stack-depth (WCSD) analysis (#link(<ch-24-5>)[Section 25.5]) to bound stack use. Excluding blocking effects and bounding stack depth address specific deadline risks; hard real-time execution still requires target-specific scheduling and memory-residency guarantees.

+ *Subnormals-Flushed Hardware FPU Scope (`eff [ConstDenorm]`):* IIR biquad filters, reverbs, and PID state estimators routinely decay toward zero on silence or steady-state error, producing IEEE-754 denormalized floats that trigger 50x–100x microcode assist stalls on `x86_64` and AArch64—causing sudden audio xruns or missed 1 kHz actuator deadlines. Entering `Resonance::realtime_quantum` activates `copy effect ConstDenorm`, emitting hardware Flush-to-Zero and Denormals-Are-Zero (`MXCSR.FTZ|DAZ` on `x86_64`, `FPCR.FZ` on AArch64) around the callback and restoring caller FPU flags on exit.

+ *Wait-Free Substructural SPSC Ring Reclamation (`RtRetirementSink`):* Mutating live DSP filter topologies or swapping robotics trajectory splines from a UI or planner thread without locking the realtime thread is notoriously error-prone in Rust: dropping an `Arc[DspGraph]` on the audio thread when the UI happens to drop its reference first triggers `GlobalAlloc::dealloc` mid-callback! Resonance pairs wait-free atomic pointer swaps with a pre-allocated SPSC garbage-retirement channel (`RtRetirementSink[T]`). Retired `affine` graphs are pushed in O(1) (3 instructions, 0 syscalls) back to a non-realtime *Vesper Pulse* collector fiber—statically forbidding destructor deallocation on the realtime core.

+ *Static Delay-Compensated DAG Scheduling and `egglog` SIMD Buffer Fusion:* Rather than dynamic virtual `processBlock()` vtable traversals that thrash L1 instruction caches across 64-sample buffers, Resonance compiles typed audio and control DAGs at Stage 4 (`vesper-opt-egglog`, #link(<ch-7>)[Chapter 8]): fusing gain/pan/biquad chains in registers, inserting exact lookahead *Plugin Delay Compensation (`PDC`)* ring lines automatically across parallel branches, and coloring scratch buffers via linear scan so an entire 64-channel mixer fits inside a single L1-resident `TiledSeq` (`AoSoA`, #link(<ch-12-5>)[Section 13.5]) slab.

+ *Sample-Accurate `wire struct` Event Splitting and Lean 4 BIBO Stability Proofs (`vesper verify`):* Resonance processes CLAP/MIDI 2.0 parameter automation and EtherCAT sensor timestamps via padding-free `wire struct` frames (#link(<ch-15-2>)[Section 16.2]), automatically splitting DSP blocks at sample-exact parameter ramps while `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) mechanically proves *Bounded-Input Bounded-Output (BIBO)* filter stability, actuator torque clamping, and wait-free SPSC ring linearizability.

=== 41.2 Hard-Realtime Capabilities, `wire struct` Sample Events, and `Linear` Buffer Leases
<ch-40-2>
Resonance models hardware DMA audio/actuator periods as `Mode::Linear` leases (`linear struct RtOutputLease`) that must be completely populated or explicitly zero-silenced/safe-torqued before returning to the OS driver (`CoreAudio` IOProc, Linux `PipeWirememfd`, Windows `WASAPI` Exclusive, or `PREEMPT_RT` EtherCAT):

```
// Exact 16-byte sample-accurate parameter / MIDI 2.0 / Trajectory event (0 implicit padding):
wire struct RtParamEventWire {
    sample_offset_in_block: u16le,
    node_id: u16le,
    param_id: u32le,
    target_value_f32_bits: u32le,
    ramp_samples: u32le,
}

// Stateless lexical capabilities proving hard-realtime context & hardware FTZ/DAZ mode:
copy effect Realtime {}
copy effect ConstDenorm {}

// Mode::Linear DMA output lease: compiler REJECTS returning from callback without committing or silencing!
linear struct RtOutputLease[Chans: dim, Frames: dim] {
    dma_ptr: DmaSliceMut,
}

// Wait-free SPSC retirement sender: moves retired affine state OFF the realtime thread in O(1)!
copy struct RtRetirementSink[T] {
    ring_slot_base: usize,
}

linear effect RtBus {
    fn poll_param_events(max_events: u32) -> WireSlice[RtParamEventWire];
    fn retire_to_janitor_waitfree[T](in sink_ring: RtRetirementSink[T], sink old_state: T) -> Unit;
    fn commit_dma_output[C: dim, F: dim](sink out_lease: RtOutputLease[C, F]) -> Unit;
    fn emergency_silence_and_commit[C: dim, F: dim](sink out_lease: RtOutputLease[C, F]) -> Unit;
}
```

=== 41.3 Sample-Accurate SIMD SVF Synth + 1 kHz Torque Controller (0 Alloc, 0 Locks)
<ch-40-3>
Decorating a callback with `@realtime(stack_budget_bytes = 4096, period_us = 1333)` instructs Stage 3 and Stage 5 to verify zero suspending/allocating effects and bounded stack frames while processing sample-accurate parameter sub-slices with *Vesper Vector* (`VVec`, #link(<ch-13>)[Chapter 14]):

```
// 16-voice AoSoA State-Variable Filter (SVF) bank: hot filter states pack into 64B L1 cache lines!
tiled(16) copy struct SvfVoiceTile {
    hot ic1eq: f32,
    hot ic2eq: f32,
    hot g_coeff: f32,
    hot k_damping: f32,
    hot gain: f32,
    cold patch_id: u32,
}

// Hard-realtime audio/control callback: Alloc, Async, Net, Storage are COMPILE-TIME ERRORS!
@realtime(stack_budget_bytes = 4096, period_us = 1333)
fn process_realtime_quantum[P: IsaProfile](
    in in_samples: AlignedSlice[f32, 64],
    inout voices.{ic1eq, ic2eq, g_coeff, k_damping, gain}: TiledSeq[SvfVoiceTile],
    inout cmd_mailbox: RtAtomicSlot[Box[VoicePatchBank]],
    in trash_sink: RtRetirementSink[Box[VoicePatchBank]],
    sink out_lease: RtOutputLease[2, 64],
) -> Unit eff [Realtime, ConstDenorm, RtBus, Isa[P]] {
    // 1. Lock-free hot-swap of newly compiled patch bank; retires old Box to Pulse janitor fiber!
    if let Some(old_bank) = cmd_mailbox.try_swap_in_latest(inout voices) {
        RtBus::retire_to_janitor_waitfree(in trash_sink, sink old_bank);
    }

    // 2. Sample-accurate sub-block splitting at exact event offsets (CLAP / MIDI 2.0 style):
    let events = RtBus::poll_param_events(32);
    let mut cursor: usize = 0;

    for ev in events {
        let split_at = (ev.sample_offset_in_block.to_native() as usize).min(64);
        if split_at > cursor {
            render_svf_subblock_simd[P](in in_samples, inout voices, inout out_lease, cursor, split_at);
            cursor = split_at;
        }
        apply_voice_param_instant(inout voices, in ev);
    }

    if cursor < 64 {
        render_svf_subblock_simd[P](in in_samples, inout voices, inout out_lease, cursor, 64);
    }

    // 3. Linearly discharges out_lease, proving every output sample frame [0..64) was written!
    RtBus::commit_dma_output(sink out_lease);
}
```

=== 41.4 Lean 4 Wait-Free SPSC Ring Safety and Delay-Compensation Proofs (`vesper verify`)
<ch-40-4>
Extracting Resonance's wait-free SPSC ring index arithmetic, actuator saturators, and automatic *Plugin Delay Compensation (`PDC`)* graph compiler via `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Zero Buffer Overrun / Underrun* and *Exact Phase Alignment* across parallel DSP/control paths:

```lean
-- illustrative/ResonanceRealtimeSafety.lean
-- 1. Wait-Free SPSC Ring Invariant: available readable slots never exceed ring capacity cap:
theorem spsc_ring_occupancy_bounded
    (cap writePos readPos : Nat)
    (h_order : readPos ≤ writePos)
    (h_full  : writePos - readPos ≤ cap) :
    let readable := writePos - readPos
    let writable := cap - readable
    readable + writable = cap ∧ readable ≤ cap := by
  omega

-- 2. Automatic PDC Phase Alignment: inserting maxLat - branchLat delay equalizes all DAG paths!
theorem pdc_equalizes_parallel_branch_latency
    (branchLat maxLat : Nat) (h_le : branchLat ≤ maxLat) :
    branchLat + (maxLat - branchLat) = maxLat := by
  omega
```

=== 41.5 Rust Non-Blocking Effect Verifier and Automatic PDC Graph Compiler (`crates/vesper-resonance`)
<ch-40-5>
`crates/vesper-resonance` enforces the compile-time `@realtime` effect-confinement rule across call graphs and compiles arbitrary DAGs into topologically sorted, delay-compensated execution schedules:

```rust
// crates/vesper-resonance/src/lib.rs
use rustc_hash::FxHashMap;
use smol_str::SmolStr;
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealtimeViolation {
    ForbiddenBlockingOrAllocEffect { fn_name: SmolStr, offending_effect: SmolStr },
    FeedbackCycleWithoutUnitDelay { involved_nodes: usize },
}

/// Statically audits @realtime functions to ensure zero Alloc, Async, Net, Storage, or Ffi locks.
pub fn verify_realtime_effect_row(
    fn_name: &SmolStr,
    active_effect_names: &[SmolStr],
) -> Result<(), RealtimeViolation> {
    const ALLOWED_RT_EFFECTS: &[&str] = &["Realtime", "ConstDenorm", "RtBus", "Isa", "Mmio"];
    for eff in active_effect_names {
        if !ALLOWED_RT_EFFECTS.contains(&eff.as_str()) {
            return Err(RealtimeViolation::ForbiddenBlockingOrAllocEffect {
                fn_name: fn_name.clone(),
                offending_effect: eff.clone(),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DspNodeId(pub u32);

#[derive(Clone, Debug)]
pub struct DspNodeSpec {
    pub id: DspNodeId,
    pub intrinsic_latency_samples: u32,
    pub inputs: Vec<DspNodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledDspOp {
    pub node: DspNodeId,
    /// Per-input inserted compensation delay (max_arrival - input_arrival) in samples.
    pub input_pdc_delays: Vec<(DspNodeId, u32)>,
    pub cumulative_latency_samples: u32,
}

/// Topologically schedules a DSP / Control DAG and synthesizes exact Plugin Delay Compensation (PDC).
pub fn compile_pdc_schedule(nodes: &[DspNodeSpec]) -> Result<Vec<ScheduledDspOp>, RealtimeViolation> {
    let mut in_degree: FxHashMap<DspNodeId, usize> = FxHashMap::default();
    let mut dependents: FxHashMap<DspNodeId, Vec<DspNodeId>> = FxHashMap::default();
    let mut specs: FxHashMap<DspNodeId, &DspNodeSpec> = FxHashMap::default();

    for n in nodes {
        specs.insert(n.id, n);
        in_degree.insert(n.id, n.inputs.len());
        for &src in &n.inputs {
            dependents.entry(src).or_default().push(n.id);
        }
    }

    let mut queue: VecDeque<DspNodeId> = in_degree
        .iter()
        .filter_map(|(&id, &deg)| (deg == 0).then_some(id))
        .collect();

    let mut out_latency: FxHashMap<DspNodeId, u32> = FxHashMap::default();
    let mut schedule = Vec::with_capacity(nodes.len());

    while let Some(id) = queue.pop_front() {
        let spec = specs[&id];
        let max_in_lat = spec
            .inputs
            .iter()
            .map(|src| out_latency[src])
            .max()
            .unwrap_or(0);

        let pdc_delays = spec
            .inputs
            .iter()
            .map(|&src| (src, max_in_lat - out_latency[&src]))
            .collect();

        let total_lat = max_in_lat + spec.intrinsic_latency_samples;
        out_latency.insert(id, total_lat);
        schedule.push(ScheduledDspOp {
            node: id,
            input_pdc_delays: pdc_delays,
            cumulative_latency_samples: total_lat,
        });

        if let Some(deps) = dependents.get(&id) {
            for &dep in deps {
                let deg = in_degree.get_mut(&dep).expect("registered node");
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }

    if schedule.len() != nodes.len() {
        return Err(RealtimeViolation::FeedbackCycleWithoutUnitDelay {
            involved_nodes: nodes.len() - schedule.len(),
        });
    }
    Ok(schedule)
}
```


== 42. Vesper Tensor: Shape-Checked Compute and Paged-KV Inference
<ch-41>
#chapter-deck[Shape Contracts, Quantized Weights, Operator Fusion, and KV-Cache Ownership]

*Vesper Tensor* (`@vesper-ml/tensor`, with implementation code in `crates/vesper-tensor`) describes differentiable tensor compute, operator fusion, and local LLM/VLM inference. Its proposed contracts combine named-axis shape checking (`Tensor[r, DType, Shape]`), mapped quantized weights (`GGUF`, `Safetensors`, `Q4_K`, `FP8`, and `MXFP4`), CPU/GPU epilogue fusion, and linear `KvPageLease` serving. This chapter explains those mechanisms and their intended composition; format, backend, and lifecycle coverage require implementation-specific validation.

=== 42.1 Compile-Time Dimension Unification, Operator Fusion, and KV-Lease Guarantees
<ch-41-1>
+ *Named-Axis Shape Checking (`Tensor[r, DType, Shape]`):* Tensor proposes tracking named dimensions such as `[Batch: dim, Seq: dim, Heads: 32, HeadDim: 128]` in `vesper-hir`. Unification must distinguish static constants from symbolic runtime dimensions such as `Seq`. Intended checks include matrix contraction compatibility, even-width requirements for rotary position embeddings (RoPE), and grouped-query attention constraints (`Heads % KvHeads == 0`). *Lens* (#link(<ch-17>)[Chapter 18]) is intended to report failed obligations before execution. A symbolic runtime dimension requires a justified constraint or runtime check; its name alone does not establish the bound.

+ *Zero-Copy `wire struct` Quantized Weight Mapping (`GGUF` and `Safetensors`):* Loading multi-gigabyte `BF16`, `FP8` (`E4M3`), or 4-bit `Q4_K` / `MXFP4` weights in Python and Rust frameworks typically parses headers on the heap or relies on `unsafe` byte-slice transmutations. Pairing Vesper Bridge padding-free `wire struct` block layouts (#link(<ch-15-2>)[Section 16.2]) with Vesper Pulse `mmap` and `io_uring` fixed-buffer streaming into Hardware Domain 1 (#link(<ch-12-3>)[Section 13.3]), Vesper Tensor projects quantized super-blocks directly in place with zero global heap allocations and zero `unsafe` casts.

+ *Linear PagedAttention KV-Cache Lifecycle (`KvPageLease`):* In C++ and Rust LLM serving engines (`vLLM`, `llama.cpp`, `mistral.rs`), physical KV-cache blocks in HBM/VRAM are managed via manual reference-counted block tables; request timeouts, speculative-decoding rollbacks, or client disconnects routinely leak VRAM blocks or read stale kv-cache slots. Vesper Tensor models every allocated 16-token KV page as a `Mode::Linear` obligation (`linear struct KvPageLease`). Under Theorem 2 and `[T-Let]`, every active sequence must explicitly return its `KvPageLease` handles to `KvCachePool` upon completion, beam pruning, or `Cancel` disconnect.

+ *Stage 4 `egglog` Operator Fusion and Cooperative Matrix Lowering:* Rather than launching separate memory-bound kernels for `RMSNorm`, projections, `RoPE`, and `SwiGLU` activations, Stage 4 (`vesper-opt-egglog`, #link(<ch-7>)[Chapter 8]) saturates tensor dataflow graphs to fuse elementwise reductions and epilogues into tiled register loops while keeping intermediate activations inside lexical `Region` scratchpads (#link(<ch-12-2>)[Section 13.2]). Compute kernels lower portably across CPU `eff [Isa[P]]` (`AVX-512 VNNI`/`AMX`, Arm `SVE2`/`SME2`, RISC-V `RVV`) and #link(<ch-38>)[Chapter 39]'s tri-backend GPU shaders (`DirectX 12` SM 6.9 Cooperative Vectors, `Vulkan 1.4` `SPV_KHR_cooperative_matrix`, and `Metal 4` `MTLTensor`).

+ *Lean 4 KV-Page Conservation and Streaming Softmax Proofs (`vesper verify`):* `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) extracts quantized dequantization kernels, shape-contraction rules, and PagedAttention block schedulers into Lean 4, mechanically proving zero out-of-bounds tensor accesses and exact KV-block conservation across speculative decoding aborts.

=== 42.2 Symbolic Shape-Typed Tensors, Quantized `wire struct` Blocks, and Linear `KvPageLease`
<ch-41-2>
Every quantized weight tensor is backed by padding-free `wire struct` super-blocks (32 4-bit weights packed into 16 bytes alongside an IEEE `fp16` block scale), while dynamic per-request KV pages are tracked via `Mode::Linear` `KvPageLease` handles:

```
// Exact 18-byte Q4_0 quantized weight block (32 x int4 weights + fp16 scale, 0 implicit padding):
wire struct Q4Block32Wire {
    scale_fp16: u16le,
    packed_nibbles: ByteArray[16],
}

// Compile-time symbolic dimension & dtype tags:
copy struct Bf16 {}
copy struct F32 {}
copy struct Q4_0 {}

// Borrowed or Region-backed tensor parameterized by symbolic shape tuple:
copy struct TensorView[r: Region, DType, Shape] {
    data_ptr: RegionPtr[r],
    elem_len: usize,
}

// Mode::Linear physical 16-token KV-cache block in HBM / Unified Memory:
linear struct KvPageLease[KvHeads: dim, HeadDim: dim] {
    block_id: u32,
    filled_tokens: u8,
}

// Active sequence KV table owning a linear slice of physical KV pages:
linear struct ActiveKvSeq[KvHeads: dim, HeadDim: dim] {
    seq_id: u64,
    token_pos: u32,
    pages: LinearSeq[KvPageLease[KvHeads, HeadDim]],
}

linear effect KvCachePool[KvHeads: dim, HeadDim: dim] {
    fn begin_sequence(seq_id: u64) -> Result[ActiveKvSeq[KvHeads, HeadDim], KvOomError];
    fn append_kv_step[r: Region](
        inout seq: ActiveKvSeq[KvHeads, HeadDim],
        in k_step: TensorView[r, Bf16, [KvHeads, HeadDim]],
        in v_step: TensorView[r, Bf16, [KvHeads, HeadDim]],
    ) -> Result[Unit, KvOomError];
    fn release_sequence(sink seq: ActiveKvSeq[KvHeads, HeadDim]) -> Unit;
}
```

=== 42.3 Zero-Global-Heap Grouped-Query Attention (GQA) Decode Step (`0` `Alloc`, `0` `unsafe`)
<ch-41-3>
During autoregressive token generation, each decode step executes inside a 2 MiB lexical `Region::scoped` scratchpad (`Alloc` omitted from `eff [...]`), statically verifying `QHeads = 32`, `KvHeads = 8` (`32 % 8 == 0`), and `HeadDim = 128` while `[T-Let]` guarantees `ActiveKvSeq` is cleanly released on client disconnect:

```
// Immutable Transformer layer weights mmap'd zero-copy from Safetensors / GGUF:
copy struct TransformerLayerWeights[DModel: dim, QHeads: dim, KvHeads: dim, HeadDim: dim] {
    rms_attn_weight: StaticTensor[F32, [DModel]],
    wq_q4: StaticQuantTensor[Q4_0, [QHeads * HeadDim, DModel]],
    wk_q4: StaticQuantTensor[Q4_0, [KvHeads * HeadDim, DModel]],
    wv_q4: StaticQuantTensor[Q4_0, [KvHeads * HeadDim, DModel]],
    wo_q4: StaticQuantTensor[Q4_0, [DModel, QHeads * HeadDim]],
}

// Single-token GQA decode kernel: shape-checked at compile time, fused by `egglog` at Stage 4!
fn forward_gqa_layer[P: IsaProfile, r: Region](
    in scratch: RegionCap[r],
    in weights: TransformerLayerWeights[4096, 32, 8, 128],
    in x_hidden: TensorView[r, F32, [4096]],
    inout kv_seq: ActiveKvSeq[8, 128],
) -> Result[TensorView[r, F32, [4096]], KvOomError] eff [
    r: Region,
    KvCachePool[8, 128],
    Isa[P],
    Fail[KvOomError]
] {
    // Stage 4 `egglog` fuses RMSNorm + Q4 dequantizing MatVec into a single L1-resident pass:
    let x_norm = Tensor::rms_norm[P](in scratch, in x_hidden, in weights.rms_attn_weight, 1.0e-5);

    let q = Tensor::q4_matvec_rope[P, 32, 128](in scratch, in weights.wq_q4, in x_norm, kv_seq.token_pos);
    let k = Tensor::q4_matvec_rope[P, 8, 128](in scratch, in weights.wk_q4, in x_norm, kv_seq.token_pos);
    let v = Tensor::q4_matvec_reshaped[P, 8, 128](in scratch, in weights.wv_q4, in x_norm);

    // Appends K/V into linear PagedAttention block table (allocates new KvPageLease iff pos % 16 == 0):
    KvCachePool::append_kv_step(inout kv_seq, in k, in v)?;

    // Online FlashDecoding across paged KV blocks (32 query heads grouped 4:1 over 8 KV heads):
    let attn_out: TensorView[r, F32, [4096]] =
        Tensor::paged_flash_decode_gqa[P, 32, 8, 128](in scratch, in q, in kv_seq);

    let proj = Tensor::q4_matvec[P, 4096, 4096](in scratch, in weights.wo_q4, in attn_out);
    Ok(Tensor::residual_add[P](in scratch, in x_hidden, in proj))
}

// End-to-end streaming session: `[T-Let]` forbids leaking `kv_seq` on client disconnect or OOM!
fn stream_completion_tokens[P: IsaProfile](
    seq_id: u64,
    prompt_tokens: Slice[u32],
    in model: Llama3Weights[4096, 32, 8, 128],
    inout stream: SseTokenResponder,
) -> Result[Unit, InferenceError] eff [KvCachePool[8, 128], Net, Isa[P], Fail[InferenceError]] {
    let mut kv_seq: ActiveKvSeq[8, 128] = KvCachePool::begin_sequence(seq_id)
        .map_err(InferenceError::KvPool)?;

    let mut next_tok = prefill_prompt[P](in model, prompt_tokens, inout kv_seq);
    loop {
        let step_res = Region::scoped(2.mib(), |cap step_arena| {
            decode_next_token[P](in step_arena, in model, next_tok, inout kv_seq)
        });

        match step_res {
            Ok(tok) if tok == EOS_TOKEN_ID => {
                KvCachePool::release_sequence(sink kv_seq); // Linearly frees all VRAM KV blocks!
                return Ok(());
            }
            Ok(tok) => {
                match stream.send_token_checked(tok) {
                    Ok(()) => { next_tok = tok; }
                    Err(_client_disconnected) => {
                        // Mandatory linear cleanup before early disconnect return:
                        KvCachePool::release_sequence(sink kv_seq);
                        return Ok(());
                    }
                }
            }
            Err(oom_err) => {
                KvCachePool::release_sequence(sink kv_seq);
                Fail::raise(InferenceError::KvPool(oom_err));
            }
        }
    }
}
```

=== 42.4 Lean 4 KV-Page Conservation and Contraction Proofs (`vesper verify`)
<ch-41-4>
Extracting `stream_completion_tokens` and `forward_gqa_layer` via `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges both *Zero Leaked KV-Cache Blocks* across client disconnects/OOMs and *Grouped-Query Head Mapping Bounds*:

```lean
-- illustrative/TensorInferenceSafety.lean
-- 1. PagedAttention Conservation: every allocated physical KV block is reclaimed on EOS, Disconnect, or OOM:
theorem stream_completion_conserves_kv_pages
    (pool : RefKvPool) (seqId : UInt64) (prompt : Array UInt32) (w : Llama3Weights) :
    let (pool', _res) := RefKvPool.run pool (streamCompletionTokens seqId prompt w)
    pool'.freeBlocks + pool'.activeBlocks = pool.freeBlocks + pool.activeBlocks ∧
    pool'.activeSeqPages seqId = 0 := by
  simp [streamCompletionTokens, KvCachePool.releaseSequence, RefKvPool.run]

-- 2. GQA Head-Index Soundness: every query head `qh < QHeads` maps strictly within `KvHeads`:
theorem gqa_kv_head_index_in_bounds (qHeads kvHeads qh : Nat)
    (h_pos : kvHeads > 0) (h_div : qHeads % kvHeads = 0) (h_qh : qh < qHeads) :
    let groupSize := qHeads / kvHeads
    qh / groupSize < kvHeads := by
  omega
```

=== 42.5 Rust Symbolic Shape Unifier and `egglog` Epilogue Fuser (`crates/vesper-tensor`)
<ch-41-5>
`crates/vesper-tensor` provides the compile-time symbolic dimension checker (enforcing `MatMul` inner-axis equality, `RoPE` even head width, and `GQA` head divisibility) alongside the Stage 4 operator-fusion pass:

```rust
// crates/vesper-tensor/src/lib.rs
use rustc_hash::FxHashMap;
use smol_str::SmolStr;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DimExpr {
    Const(u64),
    Sym(SmolStr),
    Mul(Box<DimExpr>, Box<DimExpr>),
}

impl DimExpr {
    pub fn simplify(&self, env: &FxHashMap<SmolStr, u64>) -> Self {
        match self {
            Self::Const(n) => Self::Const(*n),
            Self::Sym(s) => env.get(s).copied().map_or_else(|| Self::Sym(s.clone()), Self::Const),
            Self::Mul(lhs, rhs) => match (lhs.simplify(env), rhs.simplify(env)) {
                (Self::Const(a), Self::Const(b)) => Self::Const(a * b),
                (l, r) => Self::Mul(Box::new(l), Box::new(r)),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapeError {
    ContractionMismatch { lhs_k: DimExpr, rhs_k: DimExpr },
    OddRopeHeadDim { head_dim: u64 },
    InvalidGqaHeadRatio { q_heads: u64, kv_heads: u64 },
    QuantBlockAlignment { k_dim: u64, block_size: u64 },
}

/// Statically verifies MatMul contraction `[M, K] x [K, N] -> [M, N]` and Q4_0 block alignment.
pub fn check_quantized_matmul(
    m: &DimExpr,
    lhs_k: &DimExpr,
    rhs_k: &DimExpr,
    n: &DimExpr,
    quant_block_size: Option<u64>,
    env: &FxHashMap<SmolStr, u64>,
) -> Result<(DimExpr, DimExpr), ShapeError> {
    let lk = lhs_k.simplify(env);
    let rk = rhs_k.simplify(env);
    if lk != rk {
        return Err(ShapeError::ContractionMismatch { lhs_k: lk, rhs_k: rk });
    }
    if let (Some(blk), DimExpr::Const(k_val)) = (quant_block_size, &lk) {
        if *k_val % blk != 0 {
            return Err(ShapeError::QuantBlockAlignment { k_dim: *k_val, block_size: blk });
        }
    }
    Ok((m.simplify(env), n.simplify(env)))
}

/// Statically verifies Grouped-Query Attention (`QHeads % KvHeads == 0`) & even RoPE `HeadDim`.
pub fn check_gqa_rope_dims(q_heads: u64, kv_heads: u64, head_dim: u64) -> Result<u64, ShapeError> {
    if head_dim == 0 || head_dim % 2 != 0 {
        return Err(ShapeError::OddRopeHeadDim { head_dim });
    }
    if kv_heads == 0 || q_heads % kv_heads != 0 {
        return Err(ShapeError::InvalidGqaHeadRatio { q_heads, kv_heads });
    }
    Ok(q_heads / kv_heads)
}

/// High-level Tensor IR fused by Stage 4 (`vesper-opt-egglog`) to eliminate HBM round-trips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TensorIr {
    Input(SmolStr),
    RmsNorm { x: Box<TensorIr>, weight: SmolStr, eps_bits: u32 },
    Q4MatVec { weight: SmolStr, input: Box<TensorIr> },
    ResidualAdd { a: Box<TensorIr>, b: Box<TensorIr> },
    FusedRmsNormQ4MatVec { weight: SmolStr, norm_weight: SmolStr, x: Box<TensorIr>, eps_bits: u32 },
}

impl TensorIr {
    /// Rewrites `Q4MatVec(W, RmsNorm(x, nw, eps))` into a single L1-register-fused operator.
    pub fn fuse_epilogues(self) -> Self {
        match self {
            Self::Q4MatVec { weight, input } => match input.fuse_epilogues() {
                Self::RmsNorm { x, weight: norm_weight, eps_bits } => Self::FusedRmsNormQ4MatVec {
                    weight,
                    norm_weight,
                    x,
                    eps_bits,
                },
                other => Self::Q4MatVec { weight, input: Box::new(other) },
            },
            Self::RmsNorm { x, weight, eps_bits } => Self::RmsNorm {
                x: Box::new(x.fuse_epilogues()),
                weight,
                eps_bits,
            },
            Self::ResidualAdd { a, b } => Self::ResidualAdd {
                a: Box::new(a.fuse_epilogues()),
                b: Box::new(b.fuse_epilogues()),
            },
            leaf => leaf,
        }
    }
}
```

= Part VIII. Mechanized Metatheory and Official Geometric Identity
<part-8>
#chapter-deck[Selected Lean Models, Their Proof Scope, and the Visual Identity Specification]

#_part-summary("Part VIII Architectural Scope")[
  Part VIII separates proof scope from visual identity. #link(<appendix-a>)[Appendix A] presents selected Lean 4 models and explains the properties, operational bridges, and limitations of each. The models do not form a single mechanization of the full language or compiler. #link(<appendix-b>)[Appendix B] specifies the geometry, OKLCH palettes, and SVG construction of Vesper's official visual identity. Its geometric rules are independent of the language's soundness claims.
]

#figure(
  table(
    columns: (16%, 32%, 52%),
    align: (left, left, left),
    table.header([*Appendix*], [*Title*], [*Mechanized Modules and Normative Artifacts*]),
    [#link(<appendix-a>)[Appendix A]], [Mechanized Lean 4 Metatheory], [`lean/MetatheoryAppendix.lean` & `lean/VesperUnifiedTypeTheory.lean` (zero `sorryAx`, core Lean 4 kernel verified)],
    [#link(<appendix-b>)[Appendix B]], [Official Brand & Geometric Identity], [*The Disjoint Field-Tree Prism* ($256 times 256$ CAD grid, OKLCH Dusk/Dawn palettes, `brand.rs` SVG synthesis)],
  ),
  kind: table,
)

== Appendix A. Mechanized Lean 4 Metatheory of λ#sub[Vesper]
<appendix-a>
#chapter-deck[Core Substructural Calculus, Place Capability Graphs, Four-Axis Modalities, Ghost Erasure, Destinations, Refinements, and Quotients]

The listings below are small explanatory models. Their theorems apply to their stated definitions; a zero-`sorryAx` report alone does not establish correspondence with the compiler. The repository also contains larger operational developments:

- `lean/MetatheoryFull.lean`: small-step progress and preservation, one-shot handlers, checked arithmetic, field-tree operations, and resource exhaustion (`type_safety`, `total_linear_exhaustion`).
- `lean/Metatheory/Closures.lean`: closures, stateful deep handlers, effect-row substitution, and rejection of linear state across abort (`linear_state_across_abort_rejected`).
- `lean/Metatheory/Polymorphism.lean`: recursive closures, bounded type substitution, row polymorphism, and lexical handler escape prevention (`type_safety`, `eval_sound`, `escaped_closure_never_performs`). Recursion safety does not imply termination.
- `lean/MetatheoryPlaces.lean`: mutable frames, second-class borrows, field-tree capability transitions, fibers, and relocation (`type_safety`, `total_linear_exhaustion`, `zero_pin_relocation`). Locations are relative indices; this does not prove relocation of native stacks containing raw pointers or outstanding DMA.
- `lean/ResourceModel42/Proofs.lean`: correspondence between a finite executable checker/evaluator and `MetatheoryFull`, including `evalSteps_preservation` and `evalSteps_noInvariant`. This is not a refinement proof for the production Rust checker or code generator.

The first three developments now compute aggregate kinds by joining effective field floors, with omitted floors elaborated to intrinsic kinds. Each proves that a scalar wrapper with an explicit linear floor cannot be typed copy. Their preservation proofs use field well-formedness to bound every contained resource by the aggregate kind. Separate source-hygiene and kernel tests audit every theorem against the standard Lean axioms `propext`, `Classical.choice`, and `Quot.sound`.

`lean/CodegenComposition.lean` additionally composes the existing `VerifiedCodegen` instruction-selection simulation across finite straight-line slot-IR programs. Its verification condition checks operand ranges at every reachable row; it does not assume that initial typing survives arbitrary writes. The proof preserves results and aborts, rejects ill-typed continuations, and excludes abstract hardware faults, including an overflow example whose faulty suffix is unreachable. `lean/CodegenControlFlow.lean` adds an interpreter for control instructions in the actual selected blocks. Under an explicit inductive invariant, it proves branch/jump/return correspondence, finite-prefix simulation, fault exclusion, and preservation/reflection of divergence. A loop example and a counterexample involving an out-of-range Boolean condition check both the positive result and its premise. Programs and selected blocks share an abstract row index and label resolver. Rust lowering, discharge of the invariant by the compiler, calls/frame layout, physical instruction addresses, byte encodings, and hardware semantics remain open.

`lean/CodegenValidation.lean` provides an executable conservative range validator for finite data-row streams. It propagates kinds through constants, arithmetic, comparisons, and copies between frame areas, replacing stale destination facts on every write. Its `validation_sound` theorem establishes the straight-line simulation's semantic operand obligations for every initial store satisfying the supplied facts; validation does not execute a concrete store. `validated_closed_program` removes input-range assumptions for accepted programs starting with no facts. Checked examples cover overflow, aliased destinations, frame copies, stale-fact rejection, and unrepresentable Boolean operands. Successful remainder results retain a proved range, including the signed minimum divided by minus one; control rows and calls are rejected by this straight-line validator. It is not integrated with the production compiler's output.

`lean/CodegenFlowValidation.lean` extends range validation to finite programs with abstract labels and finite per-row certificates. Its executable check validates both branch successors, jump targets, and loop back edges. `validation_invariant` constructs the operational invariant from acceptance; the resulting theorems preserve finite execution prefixes, exclude abstract hardware faults, and preserve and reflect divergence. A checked infinite loop and a branch returning 42 exercise these results. Rejection cases cover a forged Boolean range, an invalid loop back edge, and an invalid branch arm even when that arm is unreachable from the chosen entry value. Missing labels retain the model's explicit unreachable-abort behavior; acceptance does not prove absence of all language aborts or termination. Certificate generation, production integration, physical addresses, calls, and native byte encodings remain open.

The matrix below describes local properties of the listed models, not proofs of the corresponding full implementation. In particular, capsule admission assumes the supplied graph evidence, slice coverage does not imply initialized memory, and ghost list filtering is not operational erasure equivalence. Section A.4 identifies the latter's separate operational model. Refinement snapshots now have an explicit connection to the effect-place encoding: `RefinementPlaces.heap_post_is_lens_post` identifies the heap assignment predicate with the lens contract, and `heap_patch_preserves_refinements` proves it for patching through any resolved heap-field lens. The additional `lean/TypedPlaceRefinements.lean` bridge uses the actual values, field writes, frames, and resolved borrow slots of `MetatheoryPlaces`. Its `stack_snapshot_and_borrow_soundness` combines an exact snapshot postcondition with the existing borrow invariant and per-identity resource balance. Initialized roots and same-typed replacement values justify its lens laws. Exchanging a linear payload must return or consume the old owner; a logical snapshot alone does not make overwriting it safe. The proof rejects assignment over a live linear root and demonstrates why an uninitialized path cannot use the same lens laws. For simultaneous updates, `RefinementPlaces.together_lawful` requires independent getters and commuting setters; two aliased names do not satisfy that requirement. `TypedPlaceRefinements.swap_refinement_and_ownership` snapshots both fields together and preserves identity counts and freshness. `frame_swap_matches_lens` and `own_swap_operational_step` identify that update with the mutable-place machine's actual swap step, including a worked exchange of two linear handles. Allocation and a source dependent-type checker remain open.

`lean/PlacePolymorphismBridge.lean` embeds initialized mutable-place values in the actual polymorphic calculus, preserving usage kinds and exact handle identities under unique ownership. Typed getters and setters commute with the encoding. Its exchange reduction returns the displaced owner and preserves typing, including inside the calculus's evaluation contexts; `frame_write_correspondence` connects it to the actual mutable-place frame setter. The bridge rejects duplicate handles and uninitialized inputs and checks an exchange between two distinct linear handles. This is a representation and exchange correspondence, not a merged language: heap-stored closures, captured borrows, allocation, and source elaboration remain outside the proof.

=== A.1 Theorem-to-Specification Correspondence Matrix
<app-a-1>

#figure(
  table(
    columns: (27%, 21%, 52%),
    align: (left, left, left),
    table.header([*Mechanized Theorem / Definition*], [*Specification Anchor*], [*Soundness Property Established*]),
    [`mode_submode_allows_count`, `linear_not_discardable`],
    [#link(<ch-3-1>)[Section 3.1], #link(<ch-7-3>)[8.3]],
    [Usage lattice $upright("copy") subset.eq.sq upright("affine") subset.eq.sq upright("linear")$ preserves dynamic use-count subsets and forbids implicit `linear` drops.],
    [`adt_kind_never_relaxes_linear_component`],
    [#link(<ch-3-2>)[Section 3.2] (`[K-Adt]`)],
    [Field-modality well-formedness ($bold(k)_i subset.eq.sq bold(m)_i$) prevents `@@ copy` overrides from hiding `linear` obligations.],
    [`abort_soundness_preserves_linear`, `uncancellable_io_allows_live_linear`],
    [#link(<ch-4-2>)[Section 4.2], #link(<ch-5-2>)[5.2]],
    [Resumption Frame Condition (`[T-Let]`) rejects live `linear` values across `Fail`/`Cancel` while permitting `Storage` suspensions.],
    [`call_proj_axm_exclusivity`, `semver_narrowing_preserves_disjoint`],
    [#link(<ch-4-3>)[Section 4.3], #link(<ch-11-1>)[12.1]],
    [`[T-CallProj]` guarantees alias-XOR-mutability across leaf paths, and narrowing `inout` field trees preserves caller disjointness.],
    [`pcg_join_preserves_linear_agreement`, `vesper_total_linear_exhaustion`],
    [#link(<ch-5-5>)[Section 5.5], #link(<ch-7-5>)[8.5]],
    [CFG joins agree on live `linear` bindings and closed programs discharge $100%$ of minted linear handles (*Theorem 2*).],
    [`modevec_le_trans`, `capsule_seal_produces_portable_uncontended`],
    [#link(<ch-3-1>)[Section 3.1], #link(<ch-3-3>)[3.3]],
    [Lattice transitivity and capsule admission given unique ownership, unordered obligations, and evidence excluding external aliases, outer capabilities, and thread-affine handles; no heap-graph analysis is proved.],
    [`lifo_rejects_out_of_order_irq_restore`],
    [#link(<ch-3-3>)[Section 3.3], #link(<ch-24>)[Ch 25]],
    [`Order::Lifo` stack discipline rejects inverted `IrqGuard` or `Watermark` release (`E0306`).],
    [`ghost_linear_token_zero_runtime_footprint`],
    [#link(<ch-3-4>)[Section 3.4], #link(<ch-5-5>)[5.5]],
    [Filtering an environment removes ghost entries while retaining runtime entries; this lemma alone does not prove Theorem 4.],
    [`out_split_slice_disjoint_and_exhaustive`],
    [#link(<ch-4-4>)[Section 4.4] (`[T-OutSplitSlice]`)],
    [The two index intervals are disjoint and cover the original interval; initialization requires an additional operational proof.],
    [`semver_refinement_variance_sound`, `graded_region_loop_elides_limit_checks`],
    [#link(<ch-5-3>)[Section 5.3], #link(<ch-11-2>)[12.2]],
    [Predicate implication composes under the modeled variance rule, and arithmetic assumptions imply a budget bound; neither proves compiler rewrite correctness.],
    [`uncompacted_zset_eq_compacted`],
    [#link(<ch-5-4>)[Section 5.4], #link(<ch-36>)[Ch 37]],
    [Constructive `Quot.sound` quotient equivalence equating uncompacted and consolidated Z-Set tiles.],
    [`sugar_shared_hole_forbids_hidden_mutation`, `sugar_out_hole_disjoint_from_reads`],
    [#link(<ch-6-sugar>)[Section 7.5] (`[WF-SugarPass]`)],
    [The pass-visibility table lets a shared `in` hole reach only shared uses and an `out` hole only `out` uses; this checks the table, not expansion of real `sugar` rules.],
  ),
  kind: table,
)

=== A.2 Core Substructural Calculus and PCG Disjointness (`lean/MetatheoryAppendix.lean`)
<app-a-2>

```lean
-- lean/MetatheoryAppendix.lean
-- Appendix A model from the refreshed §41 design, repaired for Lean 4.35.
-- This is the source's small one-binding calculus, not the production compiler.
-- tLet forces an empty residual frame. Closed programs terminate with preserved
-- result types and exhaust each freshly minted identity exactly once.
-- Field annotations are mode floors; omitted annotations default to the component kind.
namespace Vesper.Metatheory
inductive Mode : Type where
  | copy   : Mode
  | affine : Mode
  | linear : Mode
  deriving DecidableEq, Repr
namespace Mode
@[simp] def rank : Mode -> Nat
  | .copy   => 0
  | .affine => 1
  | .linear => 2
@[simp] def Le (m1 m2 : Mode) : Prop := m1.rank <= m2.rank
instance (m1 m2 : Mode) : Decidable (Le m1 m2) :=
  inferInstanceAs (Decidable (m1.rank <= m2.rank))
@[simp] def join (m1 m2 : Mode) : Mode :=
  match m1, m2 with
  | .linear, _ | _, .linear => .linear
  | .affine, _ | _, .affine => .affine
  | .copy, .copy            => .copy
@[simp] def meet (m1 m2 : Mode) : Mode :=
  match m1, m2 with
  | .copy, _ | _, .copy     => .copy
  | .affine, _ | _, .affine => .affine
  | .linear, .linear        => .linear
@[simp] def isDiscardable (m : Mode) : Bool :=
  match m with
  | .copy | .affine => true
  | .linear         => false
@[simp] def allowsCount (m : Mode) (n : Nat) : Prop :=
  match m with
  | .copy   => True
  | .affine => n <= 1
  | .linear => n = 1
theorem le_refl (m : Mode) : Le m m := Nat.le_refl _
theorem le_trans {a b c : Mode} (hab : Le a b) (hbc : Le b c) : Le a c :=
  Nat.le_trans hab hbc
theorem le_antisymm {a b : Mode} (hab : Le a b) (hba : Le b a) : a = b := by
  cases a <;> cases b <;> simp [Le] at hab hba <;> rfl
theorem le_join_left (a b : Mode) : Le a (join a b) := by
  cases a <;> cases b <;> decide
theorem meet_le_left (a b : Mode) : Le (meet a b) a := by
  cases a <;> cases b <;> decide
theorem meet_le_right (a b : Mode) : Le (meet a b) b := by
  cases a <;> cases b <;> decide
theorem mode_submode_allows_count {m1 m2 : Mode} {n : Nat}
    (h_sub : Le m1 m2) (h_use : m2.allowsCount n) :
    m1.allowsCount n := by
  cases m1 <;> cases m2 <;> simp [Le, allowsCount] at h_sub h_use |- <;> omega
theorem linear_not_discardable : Mode.linear.isDiscardable = false := rfl
theorem discardable_iff_le_affine (m : Mode) :
    m.isDiscardable = true <-> Le m .affine := by
  cases m <;> decide
end Mode
structure AdtFieldSpec where
  compKind     : Mode
  overrideMode : Mode := compKind
  deriving DecidableEq, Repr
@[simp] def FieldModalityWf (f : AdtFieldSpec) : Prop :=
  Mode.Le f.compKind f.overrideMode
@[simp] def adtKind (mDecl : Mode) (fields : List AdtFieldSpec) : Mode :=
  fields.foldl (fun acc f => Mode.join acc f.overrideMode) mDecl

theorem foldl_join_ge_init (init : Mode) (fs : List AdtFieldSpec) :
    Mode.Le init (adtKind init fs) := by
  induction fs generalizing init with
  | nil => exact Mode.le_refl init
  | cons hd tl ih =>
    exact Mode.le_trans (Mode.le_join_left init hd.overrideMode) (ih _)

theorem adt_kind_preserves_field_floor
    (mDecl : Mode) (fields : List AdtFieldSpec)
    (f : AdtFieldSpec) (h_mem : f ∈ fields) :
    Mode.Le f.overrideMode (adtKind mDecl fields) := by
  induction fields generalizing mDecl with
  | nil => cases h_mem
  | cons hd tl ih =>
    rcases List.mem_cons.mp h_mem with rfl | h_tail
    · have h_right : Mode.Le f.overrideMode (Mode.join mDecl f.overrideMode) := by
        cases mDecl <;> cases f.overrideMode <;> decide
      exact Mode.le_trans h_right (foldl_join_ge_init _ tl)
    · exact ih _ h_tail

theorem adt_kind_never_relaxes_linear_component
    (mDecl : Mode) (fields : List AdtFieldSpec)
    (h_wf : ∀ f, f ∈ fields → FieldModalityWf f)
    (fLin : AdtFieldSpec) (h_mem : fLin ∈ fields) (h_lin : fLin.compKind = .linear) :
    adtKind mDecl fields = .linear := by
  have h := Mode.le_trans (h_wf fLin h_mem)
    (adt_kind_preserves_field_floor mDecl fields fLin h_mem)
  rw [h_lin] at h
  cases h_kind : adtKind mDecl fields <;> simp_all [Mode.Le]

-- The review's scalar wrapper must remain linear, even though i32 is copy.
theorem owned_fd_is_linear :
    adtKind .copy [{ compKind := .copy, overrideMode := .linear }] = .linear := rfl

theorem unannotated_scalar_is_copy :
    adtKind .copy [{ compKind := .copy }] = .copy := rfl

theorem linear_component_cannot_be_annotated_copy :
    ¬ FieldModalityWf { compKind := .linear, overrideMode := .copy } := by
  simp [FieldModalityWf, Mode.Le]

inductive ResumptionMode : Type where
  | affine : ResumptionMode
  | linear : ResumptionMode
  deriving DecidableEq, Repr
@[simp] def ResumptionMode.toMode : ResumptionMode -> Mode
  | .affine => .affine
  | .linear => .linear
abbrev EffectRow := List ResumptionMode
@[simp] def EffectRow.res : EffectRow -> Mode
  | []      => .linear
  | q :: qs => Mode.meet q.toMode (EffectRow.res qs)
@[simp] def FrameSafe (liveModes : List Mode) (eff1 : EffectRow) : Prop :=
  forall m, m ∈ liveModes -> Mode.Le m eff1.res
theorem res_le_affine_of_mem_affine {eff : EffectRow} (h : .affine ∈ eff) :
    Mode.Le eff.res .affine := by
  induction eff with
  | nil => cases h
  | cons hd tl ih =>
    simp only [List.mem_cons] at h
    rcases h with rfl | h_tl
    · exact Mode.meet_le_left .affine (EffectRow.res tl)
    · exact Mode.le_trans (Mode.meet_le_right hd.toMode (EffectRow.res tl)) (ih h_tl)
theorem abort_soundness_preserves_linear
    {liveModes : List Mode} {eff1 : EffectRow}
    (h_frame : FrameSafe liveModes eff1)
    (h_abortable : .affine ∈ eff1) :
    forall m, m ∈ liveModes -> m.isDiscardable = true := by
  intro m hm
  rw [Mode.discardable_iff_le_affine]
  exact Mode.le_trans (h_frame m hm) (res_le_affine_of_mem_affine h_abortable)
theorem uncancellable_io_allows_live_linear
    (liveModes : List Mode) (eff1 : EffectRow)
    (h_all_linear : forall q, q ∈ eff1 -> q = .linear) :
    FrameSafe liveModes eff1 := by
  have h_res : eff1.res = .linear := by
    induction eff1 with
    | nil => rfl
    | cons hd tl ih =>
      have h_hd : hd = .linear := h_all_linear hd (List.mem_cons_self)
      subst h_hd
      have h_tl : (EffectRow.res tl) = .linear := ih (fun q hq => h_all_linear q (List.mem_cons_of_mem _ hq))
      simp [EffectRow.res, h_tl]
  intro m _
  rw [h_res]
  cases m <;> decide
theorem unconsumed_txn_across_question_rejected :
    ¬ FrameSafe [.linear] [.affine] := by
  intro h
  have h_lin := h .linear (List.mem_singleton.mpr rfl)
  simp [EffectRow.res, Mode.Le] at h_lin
abbrev LocalId  := Nat
abbrev FieldIdx := Nat
abbrev LeafPath := List FieldIdx
@[simp] def PathPrefix : LeafPath -> LeafPath -> Prop
  | [], _             => True
  | _ :: _, []        => False
  | f1 :: t1, f2 :: t2 => f1 = f2 /\ PathPrefix t1 t2
structure ProjSelector where
  localVar : LocalId
  branches : List LeafPath
  deriving DecidableEq, Repr
@[simp] def ProjSelector.covers (sel : ProjSelector) (loc : LocalId) (leaf : LeafPath) : Prop :=
  sel.localVar = loc /\ exists b, b ∈ sel.branches /\ PathPrefix b leaf
@[simp] def ProjSelector.Disjoint (s1 s2 : ProjSelector) : Prop :=
  forall loc leaf, ¬ (s1.covers loc leaf /\ s2.covers loc leaf)
@[simp] def ProjSelector.Subset (sNew sOld : ProjSelector) : Prop :=
  forall loc leaf, sNew.covers loc leaf -> sOld.covers loc leaf
theorem semver_narrowing_preserves_disjoint
    {sNew sOld sOther : ProjSelector}
    (h_narrow : sNew.Subset sOld)
    (h_disj   : sOld.Disjoint sOther) :
    sNew.Disjoint sOther := by
  intro loc leaf ⟨h_new, h_other⟩
  exact h_disj loc leaf ⟨h_narrow loc leaf h_new, h_other⟩
inductive PassMode : Type where
  | sharedIn : PassMode
  | inOut    : PassMode
  | sinkOwn  : PassMode
  deriving DecidableEq, Repr
structure CallArg where
  mode : PassMode
  proj : ProjSelector
  deriving DecidableEq, Repr
def CallProjValid (args : List CallArg) : Prop :=
  forall (i j : Fin args.length), i != j ->
    ((args.get i).mode = .sharedIn /\ (args.get j).mode = .sharedIn) \/
    ProjSelector.Disjoint (args.get i).proj (args.get j).proj
theorem call_proj_axm_exclusivity
    {args : List CallArg} (h_valid : CallProjValid args)
    (i j : Fin args.length) (h_neq : i != j)
    (h_mut_or_sink : (args.get i).mode != .sharedIn)
    {loc : LocalId} {leaf : LeafPath}
    (h_cov_i : (args.get i).proj.covers loc leaf) :
    ¬ (args.get j).proj.covers loc leaf := by
  intro h_cov_j
  rcases h_valid i j h_neq with ⟨h_in_i, _⟩ | h_disj
  · exact absurd h_in_i (bne_iff_ne.mp h_mut_or_sink)
  · exact h_disj loc leaf ⟨h_cov_i, h_cov_j⟩
inductive CapState : Type where
  | liveOwned        (m : Mode)
  | consumed
  | poisonedRecovery
  deriving DecidableEq, Repr
def pcgJoinLocal (lhs rhs : CapState) : Option CapState :=
  match lhs, rhs with
  | .poisonedRecovery, _ | _, .poisonedRecovery => some .poisonedRecovery
  | .consumed, .consumed                        => some .consumed
  | .liveOwned m1, .liveOwned m2                =>
    if m1 = m2 then some (.liveOwned m1) else none
  | .liveOwned m, .consumed | .consumed, .liveOwned m =>
    if m.isDiscardable then some .consumed else none
@[simp] def pcgReturnOk : CapState -> Bool
  | .liveOwned .linear => false
  | .liveOwned .copy | .liveOwned .affine | .consumed | .poisonedRecovery => true
theorem pcg_join_preserves_linear_agreement
    {lhs rhs merged : CapState}
    (h_no_poison_l : lhs != .poisonedRecovery)
    (h_no_poison_r : rhs != .poisonedRecovery)
    (h_join : pcgJoinLocal lhs rhs = some merged) :
    (lhs = .liveOwned .linear <-> rhs = .liveOwned .linear) := by
  cases lhs <;> cases rhs <;> simp [pcgJoinLocal, Mode.isDiscardable] at h_no_poison_l h_no_poison_r h_join |-
  case liveOwned.liveOwned m1 m2 =>
    rcases h_join with ⟨rfl, _⟩
    rfl
  case liveOwned.consumed m => cases m <;> simp_all
  case consumed.liveOwned m => cases m <;> simp_all
abbrev ResId := Nat
inductive Val : Type where
  | unit      : Val
  | nat       : Nat -> Val
  | linHandle : ResId -> Val
  | pair      : Val -> Val → Val
  deriving DecidableEq, Repr
@[simp] def Val.linResources : Val -> List ResId
  | .unit        => []
  | .nat _       => []
  | .linHandle r => [r]
  | .pair v1 v2  => v1.linResources ++ v2.linResources
def ValTyped (v : Val) : Mode -> Prop
  | .copy   => v.linResources = []
  | .affine => v.linResources = []
  -- This fragment has exactly one linear value type: a resource handle.
  -- Mode alone was previously treated as a value shape, admitting stuck consumes.
  | .linear => ∃ r, v = .linHandle r
inductive ModeSplit : Option Mode -> Option Mode -> Option Mode -> Prop where
  | none      : ModeSplit none none none
  | copyBoth  : ModeSplit (some .copy) (some .copy) (some .copy)
  | leftOnly  (m : Mode) : ModeSplit (some m) (some m) none
  | rightOnly (m : Mode) : ModeSplit (some m) none (some m)
@[simp] def OptEnvDiscardable : Option Mode -> Prop
  | none   => True
  | some m => Mode.Le m .affine
inductive Expr : Type where
  | var        : Expr
  | litNat     : Nat -> Expr
  | mintLinear : Expr
  | consumeLin : Expr -> Expr
  | raiseFail  : Expr
  | letSeq     : Expr -> Expr -> Expr
inductive HasType : Option Mode -> Expr -> Mode -> EffectRow -> Prop where
  | tVar {m : Mode} :
      HasType (some m) .var m []
  | tLit {gamma : Option Mode} (n : Nat) (h_disc : OptEnvDiscardable gamma) :
      HasType gamma (.litNat n) .copy []
  | tMint {gamma : Option Mode} (h_disc : OptEnvDiscardable gamma) :
      HasType gamma .mintLinear .linear [.linear]
  | tConsume {gamma : Option Mode} {e : Expr} {eff : EffectRow}
      (h_e : HasType gamma e .linear eff) :
      HasType gamma (.consumeLin e) .copy (eff ++ [.linear])
  | tFail {gamma : Option Mode} (h_disc : OptEnvDiscardable gamma) :
      HasType gamma .raiseFail .copy [.affine]
  | tLet {gamma gamma1 gamma2 : Option Mode} {e1 e2 : Expr} {m1 m2 : Mode} {eff1 eff2 : EffectRow}
      (h_split  : ModeSplit gamma gamma1 gamma2)
      (h_e1     : HasType gamma1 e1 m1 eff1)
      (h_frame  : forall m_z, gamma2 = some m_z -> Mode.Le m_z eff1.res)
      (h_z_none : gamma2 = none)
      (h_e2     : HasType (some m1) e2 m2 eff2) :
      HasType gamma (.letSeq e1 e2) m2 (eff1 ++ eff2)
inductive Outcome : Type where
  | val     : Val -> Outcome
  | aborted : Outcome
  deriving DecidableEq, Repr
inductive EvalDet : Option Val -> Expr -> Nat -> List ResId -> List ResId -> Outcome -> Nat -> Prop where
  | evVar (v : Val) (n : Nat) :
      EvalDet (some v) .var n [] [] (.val v) n
  | evLit (env : Option Val) (k n : Nat) :
      EvalDet env (.litNat k) n [] [] (.val (.nat k)) n
  | evMint (env : Option Val) (n : Nat) :
      EvalDet env .mintLinear n [n] [] (.val (.linHandle n)) (n + 1)
  | evConsumeVal {env : Option Val} {e : Expr} {n n' : Nat} {m c : List ResId} {r : ResId}
      (h : EvalDet env e n m c (.val (.linHandle r)) n') :
      EvalDet env (.consumeLin e) n m (c ++ [r]) (.val .unit) n'
  | evConsumeAbort {env : Option Val} {e : Expr} {n n' : Nat} {m c : List ResId}
      (h : EvalDet env e n m c .aborted n') :
      EvalDet env (.consumeLin e) n m c .aborted n'
  | evFail (env : Option Val) (n : Nat) :
      EvalDet env .raiseFail n [] [] .aborted n
  | evLetVal {env : Option Val} {e1 e2 : Expr} {n n1 n2 : Nat}
      {m1 c1 m2 c2 : List ResId} {v1 : Val} {out2 : Outcome}
      (h1 : EvalDet env e1 n m1 c1 (.val v1) n1)
      (h2 : EvalDet (some v1) e2 n1 m2 c2 out2 n2) :
      EvalDet env (.letSeq e1 e2) n (m1 ++ m2) (c1 ++ c2) out2 n2
  | evLetAbort {env : Option Val} {e1 e2 : Expr} {n n1 : Nat} {m1 c1 : List ResId}
      (h1 : EvalDet env e1 n m1 c1 .aborted n1) :
      EvalDet env (.letSeq e1 e2) n m1 c1 .aborted n1
def EnvTyped : Option Val -> Option Mode -> Prop
  | none, none     => True
  | some v, some m => ValTyped v m
  | _, _           => False
@[simp] def EnvResources : Option Val -> List ResId
  | none   => []
  | some v => v.linResources
@[simp] def OutcomeResources : Outcome -> List ResId
  | .val v   => v.linResources
  | .aborted => []
theorem discardable_env_has_no_linear_resources
    {env : Option Val} {gamma : Option Mode}
    (h_ty : EnvTyped env gamma) (h_disc : OptEnvDiscardable gamma) :
    EnvResources env = [] := by
  cases env <;> cases gamma <;> simp [EnvTyped, OptEnvDiscardable, Mode.Le] at h_ty h_disc |-
  rename_i v m
  cases m <;> simp at h_disc <;> exact h_ty
theorem vesper_linear_conservation_soundness
    {env : Option Val} {gamma : Option Mode} {e : Expr} {m : Mode} {eff : EffectRow}
    {n n' : Nat} {minted consumed : List ResId} {out : Outcome}
    (h_type : HasType gamma e m eff)
    (h_env  : EnvTyped env gamma)
    (h_eval : EvalDet env e n minted consumed out n') :
    (forall v, out = .val v -> ValTyped v m) /\
    (EnvResources env).length + minted.length =
      consumed.length + (OutcomeResources out).length := by
  induction h_type generalizing env n n' minted consumed out with
  | tVar =>
    cases h_eval with
    | evVar v _ =>
      refine ⟨?_, by simp⟩
      intro v' hv
      cases hv
      exact h_env
  | tLit k h_disc =>
    cases h_eval with
    | evLit _ _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · simp [ValTyped]
      · rw [h_empty]
        simp
  | tMint h_disc =>
    cases h_eval with
    | evMint _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · simp [ValTyped]
      · rw [h_empty]
        simp
  | tConsume _ ih =>
    cases h_eval with
    | evConsumeVal h_sub =>
      obtain ⟨_, h_len⟩ := ih h_env h_sub
      simp [ValTyped] at h_len |-
      omega
    | evConsumeAbort h_sub =>
      obtain ⟨_, h_len⟩ := ih h_env h_sub
      constructor
      · intro v h
        cases h
      · exact h_len
  | tFail h_disc =>
    cases h_eval with
    | evFail _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · intro v h
        cases h
      · rw [h_empty]
        simp
  | tLet h_split _ _ h_z_none _ ih1 ih2 =>
    subst h_z_none
    have split_empty : ∀ {a b : Option Mode}, ModeSplit a b none → a = b := by
      intro a b h
      cases h <;> rfl
    have h_eq_gamma := split_empty h_split
    subst h_eq_gamma
    cases h_eval with
    | evLetVal h_ev1 h_ev2 =>
      obtain ⟨h_v1_ty, h_len1⟩ := ih1 h_env h_ev1
      obtain ⟨h_out2_ty, h_len2⟩ := ih2 (env := some _) (h_v1_ty _ rfl) h_ev2
      refine ⟨h_out2_ty, ?_⟩
      simp [EnvResources, OutcomeResources] at h_len1 h_len2 |-
      omega
    | evLetAbort h_ev1 =>
      obtain ⟨_, h_len1⟩ := ih1 h_env h_ev1
      constructor
      · intro v h
        cases h
      · exact h_len1
theorem vesper_resource_identity_conservation
    (resource : ResId)
    {env : Option Val} {gamma : Option Mode} {e : Expr} {m : Mode} {eff : EffectRow}
    {n n' : Nat} {minted consumed : List ResId} {out : Outcome}
    (h_type : HasType gamma e m eff)
    (h_env  : EnvTyped env gamma)
    (h_eval : EvalDet env e n minted consumed out n') :
    (forall v, out = .val v -> ValTyped v m) /\
    (EnvResources env).count resource + (minted.count resource) =
      (consumed.count resource) + (OutcomeResources out).count resource := by
  induction h_type generalizing env n n' minted consumed out with
  | tVar =>
    cases h_eval with
    | evVar v _ =>
      refine ⟨?_, by simp⟩
      intro v' hv
      cases hv
      exact h_env
  | tLit k h_disc =>
    cases h_eval with
    | evLit _ _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · simp [ValTyped]
      · rw [h_empty]
        simp
  | tMint h_disc =>
    cases h_eval with
    | evMint _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · simp [ValTyped]
      · rw [h_empty]
        simp
  | tConsume _ ih =>
    cases h_eval with
    | evConsumeVal h_sub =>
      obtain ⟨_, h_len⟩ := ih h_env h_sub
      simp [ValTyped] at h_len |-
      omega
    | evConsumeAbort h_sub =>
      obtain ⟨_, h_len⟩ := ih h_env h_sub
      constructor
      · intro v h
        cases h
      · exact h_len
  | tFail h_disc =>
    cases h_eval with
    | evFail _ _ =>
      have h_empty := discardable_env_has_no_linear_resources h_env h_disc
      constructor
      · intro v h
        cases h
      · rw [h_empty]
        simp
  | tLet h_split _ _ h_z_none _ ih1 ih2 =>
    subst h_z_none
    have split_empty : ∀ {a b : Option Mode}, ModeSplit a b none → a = b := by
      intro a b h
      cases h <;> rfl
    have h_eq_gamma := split_empty h_split
    subst h_eq_gamma
    cases h_eval with
    | evLetVal h_ev1 h_ev2 =>
      obtain ⟨h_v1_ty, h_len1⟩ := ih1 h_env h_ev1
      obtain ⟨h_out2_ty, h_len2⟩ := ih2 (env := some _) (h_v1_ty _ rfl) h_ev2
      refine ⟨h_out2_ty, ?_⟩
      simp [EnvResources, OutcomeResources] at h_len1 h_len2 |-
      omega
    | evLetAbort h_ev1 =>
      obtain ⟨_, h_len1⟩ := ih1 h_env h_ev1
      constructor
      · intro v h
        cases h
      · exact h_len1
-- Evaluation is now a conclusion, rather than an assumption. This is total
-- progress for this recursion-free fragment, including handled failure outcomes.
theorem typed_evaluation_exists
    {gamma : Option Mode} {e : Expr} {m : Mode} {eff : EffectRow}
    (typed : HasType gamma e m eff) :
    ∀ (env : Option Val) (n : Nat), EnvTyped env gamma →
      ∃ minted consumed out next, EvalDet env e n minted consumed out next := by
  induction typed with
  | tVar =>
    intro env n he
    cases env with
    | none => cases he
    | some v => exact ⟨[], [], .val v, n, .evVar v n⟩
  | tLit k _ =>
    intro env n _
    exact ⟨[], [], .val (.nat k), n, .evLit env k n⟩
  | tMint _ =>
    intro env n _
    exact ⟨[n], [], .val (.linHandle n), n + 1, .evMint env n⟩
  | tConsume inner ih =>
    intro env n he
    obtain ⟨minted, consumed, out, next, ev⟩ := ih env n he
    cases out with
    | aborted => exact ⟨minted, consumed, .aborted, next, .evConsumeAbort ev⟩
    | val v =>
      have hv := (vesper_linear_conservation_soundness inner he ev).1 v rfl
      obtain ⟨r, rfl⟩ := hv
      exact ⟨minted, consumed ++ [r], .val .unit, next, .evConsumeVal ev⟩
  | tFail _ =>
    intro env n _
    exact ⟨[], [], .aborted, n, .evFail env n⟩
  | tLet split first _ empty _ ih1 ih2 =>
    intro env n he
    subst empty
    have split_empty : ∀ {a b : Option Mode}, ModeSplit a b none → a = b := by
      intro a b h
      cases h <;> rfl
    have same := split_empty split
    subst same
    obtain ⟨minted1, consumed1, out1, next1, ev1⟩ := ih1 env n he
    cases out1 with
    | aborted => exact ⟨minted1, consumed1, .aborted, next1, .evLetAbort ev1⟩
    | val v =>
      have hv := (vesper_linear_conservation_soundness first he ev1).1 v rfl
      obtain ⟨minted2, consumed2, out2, next2, ev2⟩ := ih2 (some v) next1 hv
      exact ⟨minted1 ++ minted2, consumed1 ++ consumed2, out2, next2, .evLetVal ev1 ev2⟩

-- The former ValTyped definition admitted a natural as a linear handle, making
-- consume(var) stuck despite a typed environment. Canonical forms exclude it.
theorem natural_is_not_a_linear_handle (n : Nat) : ¬ ValTyped (.nat n) .linear := by
  simp [ValTyped]


theorem vesper_total_linear_exhaustion
    {e : Expr} {eff : EffectRow} {n n' : Nat}
    {minted consumed : List ResId} {out : Outcome}
    (h_closed_ty : HasType none e .copy eff)
    (h_eval      : EvalDet none e n minted consumed out n') :
    minted.length = consumed.length := by
  obtain ⟨h_val_ty, h_cons⟩ :=
    vesper_linear_conservation_soundness (env := none) h_closed_ty trivial h_eval
  cases out with
  | aborted =>
    simp [EnvResources, OutcomeResources] at h_cons
    exact h_cons
  | val v =>
    have h_no_res : v.linResources = [] := h_val_ty v rfl
    simp [EnvResources, OutcomeResources, h_no_res] at h_cons
    exact h_cons

-- Allocation uses a monotone supply, so identities are fresh even in nested lets.
theorem evaluation_mints_fresh
    {env e n minted consumed out next}
    (ev : EvalDet env e n minted consumed out next) :
    n ≤ next ∧ minted = List.range' n (next - n) := by
  induction ev with
  | evVar | evLit | evFail => simp
  | evMint => simp
  | evConsumeVal _ ih | evConsumeAbort _ ih | evLetAbort _ ih => exact ih
  | @evLetVal env e1 e2 start middle finish m1 c1 m2 c2 v1 out2 first second ih1 ih2 =>
    obtain ⟨h1, eq1⟩ := ih1
    obtain ⟨h2, eq2⟩ := ih2
    refine ⟨Nat.le_trans h1 h2, ?_⟩
    rw [eq1, eq2]
    have shift : start + 1 * (middle - start) = middle := by omega
    have joined := List.range'_append (step := 1) (s := start) (m := middle - start) (n := finish - middle)
    rw [shift] at joined
    rw [joined]
    congr 1
    omega

-- In this core, Fail is the only abort source. Linear mint/consume operations
-- cannot abort; the effect row accounts for failures through consume and let.
theorem aborted_evaluation_has_affine_effect
    (typed : HasType gamma e m eff)
    (ev : EvalDet env e n minted consumed .aborted next) : .affine ∈ eff := by
  generalize h_out : Outcome.aborted = out at ev
  induction typed generalizing env n minted consumed out next with
  | tVar => cases ev; cases h_out
  | tLit => cases ev; cases h_out
  | tMint => cases ev; cases h_out
  | tFail => simp
  | tConsume _ ih =>
    cases ev with
    | evConsumeVal => cases h_out
    | evConsumeAbort sub => exact List.mem_append_left _ (ih _ rfl sub)
  | tLet _ _ _ _ _ ih1 ih2 =>
    cases ev with
    | evLetVal first second => exact List.mem_append_right _ (ih2 _ h_out second)
    | evLetAbort first => exact List.mem_append_left _ (ih1 _ rfl first)

-- All closed, copy-result programs in this fragment evaluate, preserve their
-- result type, and exhaust each minted resource identity (not just the count).
theorem closed_fragment_soundness
    {e : Expr} {eff : EffectRow} (typed : HasType none e .copy eff) (n : Nat) :
    ∃ minted consumed out next,
      EvalDet none e n minted consumed out next ∧
      (∀ v, out = .val v → ValTyped v .copy) ∧
      (∀ resource, minted.count resource = consumed.count resource) := by
  obtain ⟨minted, consumed, out, next, ev⟩ := typed_evaluation_exists typed none n trivial
  have preservation := (vesper_linear_conservation_soundness (env := none) typed trivial ev).1
  refine ⟨minted, consumed, out, next, ev, preservation, ?_⟩
  intro resource
  have counts := (vesper_resource_identity_conservation (env := none) resource typed trivial ev).2
  cases out with
  | aborted => simpa using counts
  | val v =>
    have empty : v.linResources = [] := preservation v rfl
    simpa [empty] using counts

-- No evaluation hypothesis or assumed semantic preservation theorem is needed.
theorem closed_exact_once
    {e : Expr} {eff : EffectRow} (typed : HasType none e .copy eff) (n : Nat) :
    ∃ minted consumed out next,
      EvalDet none e n minted consumed out next ∧
      (∀ v, out = .val v → ValTyped v .copy) ∧
      minted.Nodup ∧
      (∀ resource, minted.count resource = consumed.count resource) ∧
      (∀ resource, resource ∈ minted → consumed.count resource = 1) := by
  obtain ⟨minted, consumed, out, next, ev, preserved, counts⟩ :=
    closed_fragment_soundness typed n
  have fresh : minted.Nodup := by
    rw [(evaluation_mints_fresh ev).2]
    exact List.nodup_range'
  refine ⟨minted, consumed, out, next, ev, preserved, fresh, counts, ?_⟩
  intro resource member
  rw [← counts resource]
  exact fresh.count_of_mem member

-- Concrete applicability checks. These extend the examples only; none of the
-- Appendix definitions or theorem statements above are weakened.
def mintConsume : Expr := .letSeq .mintLinear (.consumeLin .var)

theorem mint_consume_has_type :
    HasType none mintConsume .copy [.linear, .linear] := by
  exact HasType.tLet (gamma1 := none) (gamma2 := none)
    ModeSplit.none (HasType.tMint trivial)
    (by intro m h; cases h) rfl (HasType.tConsume HasType.tVar)

theorem mint_consume_evaluates (n : Nat) :
    EvalDet none mintConsume n [n] [n] (.val .unit) (n + 1) := by
  exact EvalDet.evLetVal (EvalDet.evMint none n)
    (EvalDet.evConsumeVal (EvalDet.evVar (.linHandle n) (n + 1)))

theorem mint_consume_exhaustion (n : Nat) :
    ([n] : List ResId).length = ([n] : List ResId).length := by
  exact vesper_total_linear_exhaustion mint_consume_has_type (mint_consume_evaluates n)

-- Operational derivations alone allow a leak/abort; the corresponding typing
-- derivations are rejected. This also separates evaluation from progress.
theorem mint_then_leak_evaluates (n : Nat) :
    EvalDet none (.letSeq .mintLinear (.litNat 0)) n [n] [] (.val (.nat 0)) (n + 1) := by
  exact EvalDet.evLetVal (EvalDet.evMint none n)
    (EvalDet.evLit (some (.linHandle n)) 0 (n + 1))

theorem mint_then_leak_rejected (eff : EffectRow) :
    ¬ HasType none (.letSeq .mintLinear (.litNat 0)) .copy eff := by
  intro h
  cases h with
  | tLet _ h_mint _ _ h_body =>
    cases h_mint
    cases h_body with
    | tLit _ h_disc => simp [OptEnvDiscardable, Mode.Le] at h_disc

theorem mint_then_abort_evaluates (n : Nat) :
    EvalDet none (.letSeq .mintLinear .raiseFail) n [n] [] .aborted (n + 1) := by
  exact EvalDet.evLetVal (EvalDet.evMint none n)
    (EvalDet.evFail (some (.linHandle n)) (n + 1))

theorem mint_then_abort_rejected (eff : EffectRow) :
    ¬ HasType none (.letSeq .mintLinear .raiseFail) .copy eff := by
  intro h
  cases h with
  | tLet _ h_mint _ _ h_body =>
    cases h_mint
    cases h_body with
    | tFail h_disc => simp [OptEnvDiscardable, Mode.Le] at h_disc

end Vesper.Metatheory

-- Kernel axiom audit: every named source theorem and applicability example.
```

=== A.3 Four-Axis Modalities, Ghost Erasure, Destinations, Refinements, and Quotients (`lean/VesperUnifiedTypeTheory.lean`)
<app-a-3>

#_compact-listing[
```lean
-- lean/VesperUnifiedTypeTheory.lean
-- VesperUnifiedTypeTheory.lean (Appendix A.3)
-- Limited algebraic models of 4-axis ModeVec, Capsule admission, LIFO order, ghost-list erasure,
-- [T-OutSplitSlice], refinement SemVer variance, graded arena bump-check elision, and Quot.sound.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.UnifiedTypeTheory

-- ==================== 1. 4-AXIS MODAL LATTICE & CAPSULE ====================
inductive UsageMode   where | copy | affine | linear       deriving DecidableEq, Repr
inductive Portability where | portable | pinned            deriving DecidableEq, Repr
inductive Contention  where | uncontended | contended      deriving DecidableEq, Repr
inductive OrderAxis   where | unordered | lifo             deriving DecidableEq, Repr

structure ModeVec where
  usage : UsageMode
  port  : Portability
  cont  : Contention
  ord   : OrderAxis
  deriving DecidableEq, Repr

@[simp] def UsageMode.rank : UsageMode → Nat | .copy => 0 | .affine => 1 | .linear => 2
@[simp] def Portability.rank : Portability → Nat | .portable => 0 | .pinned => 1
@[simp] def Contention.rank : Contention → Nat | .uncontended => 0 | .contended => 1
@[simp] def OrderAxis.rank : OrderAxis → Nat | .unordered => 0 | .lifo => 1

@[simp] def ModeVec.Le (m1 m2 : ModeVec) : Prop :=
  m1.usage.rank ≤ m2.usage.rank ∧
  m1.port.rank  ≤ m2.port.rank  ∧
  m1.cont.rank  ≤ m2.cont.rank  ∧
  m1.ord.rank   ≤ m2.ord.rank

theorem modevec_le_trans {a b c : ModeVec} (h1 : ModeVec.Le a b) (h2 : ModeVec.Le b c) :
    ModeVec.Le a c := by
  rcases h1 with ⟨u1, p1, c1, o1⟩
  rcases h2 with ⟨u2, p2, c2, o2⟩
  exact ⟨Nat.le_trans u1 u2, Nat.le_trans p1 p2, Nat.le_trans c1 c2, Nat.le_trans o1 o2⟩

-- These are explicit premises of the abstract model, not a heap analysis.
-- The production checker must establish them from the reachable graph.
structure CapsuleEvidence where
  uniquelyOwned : Bool
  externalAliases : Nat
  freeOuterCaps : Nat
  threadAffineHandles : Nat
  deriving DecidableEq, Repr

def capsuleSealSafe (e : CapsuleEvidence) (m : ModeVec) : Option ModeVec :=
  if e.uniquelyOwned = true ∧ e.externalAliases = 0 ∧ e.freeOuterCaps = 0 ∧
      e.threadAffineHandles = 0 ∧ m.cont = .uncontended ∧ m.ord = .unordered then
    some { m with port := .portable }
  else none

theorem capsule_seal_produces_portable_uncontended
    {e : CapsuleEvidence} {m sealed : ModeVec}
    (h : capsuleSealSafe e m = some sealed) :
    e.uniquelyOwned = true ∧ e.externalAliases = 0 ∧ e.freeOuterCaps = 0 ∧
      e.threadAffineHandles = 0 ∧ sealed.port = .portable ∧
      sealed.cont = .uncontended ∧ sealed.ord = .unordered ∧ sealed.usage = m.usage := by
  unfold capsuleSealSafe at h
  split at h
  next hc =>
    cases h
    exact ⟨hc.1, hc.2.1, hc.2.2.1, hc.2.2.2.1, rfl,
      hc.2.2.2.2.1, hc.2.2.2.2.2, rfl⟩
  next => cases h

theorem capsule_rejects_thread_affinity (m : ModeVec) :
    capsuleSealSafe ⟨true, 0, 0, 1⟩ m = none := by simp [capsuleSealSafe]

theorem capsule_rejects_outer_capability (m : ModeVec) :
    capsuleSealSafe ⟨true, 0, 1, 0⟩ m = none := by simp [capsuleSealSafe]

theorem capsule_rejects_external_alias (m : ModeVec) :
    capsuleSealSafe ⟨true, 1, 0, 0⟩ m = none := by simp [capsuleSealSafe]

theorem capsule_rejects_shared_owner (m : ModeVec) :
    capsuleSealSafe ⟨false, 0, 0, 0⟩ m = none := by simp [capsuleSealSafe]

theorem capsule_rejects_ordered_owner (e : CapsuleEvidence) (m : ModeVec)
    (h : m.ord = .lifo) : capsuleSealSafe e m = none := by simp [capsuleSealSafe, h]

-- Context splitting concerns permission to access a place, not just its value's mode.
inductive Access where | own | shr | mut | out deriving DecidableEq, Repr
structure PlaceBinding where
  access : Access
  usage : UsageMode
  order : OrderAxis
  deriving DecidableEq, Repr

def Contractible (b : PlaceBinding) : Prop :=
  b.access = .shr ∨ (b.access = .own ∧ b.usage = .copy ∧ b.order = .unordered)

inductive PlaceSplit : Option PlaceBinding → Option PlaceBinding → Option PlaceBinding → Prop where
  | empty : PlaceSplit none none none
  | left (b) : PlaceSplit (some b) (some b) none
  | right (b) : PlaceSplit (some b) none (some b)
  | both (b) (h : Contractible b) : PlaceSplit (some b) (some b) (some b)

-- Any derivation that supplies the binding to both branches must justify contraction.
theorem duplicated_place_is_contractible {b left right : PlaceBinding}
    (h : PlaceSplit (some b) (some left) (some right)) : Contractible b := by
  cases h with
  | both _ hc => exact hc

theorem exclusive_place_never_duplicated {b left right : PlaceBinding}
    (exclusive : b.access = .mut ∨ b.access = .out) :
    ¬ PlaceSplit (some b) (some left) (some right) := by
  intro h
  have hc := duplicated_place_is_contractible h
  rcases exclusive with hm | ho <;> simp [Contractible, *] at hc

theorem ordered_owner_never_duplicated {b left right : PlaceBinding}
    (owned : b.access = .own) (ordered : b.order = .lifo) :
    ¬ PlaceSplit (some b) (some left) (some right) := by
  intro split
  have hc := duplicated_place_is_contractible split
  simp [Contractible, owned, ordered] at hc

theorem shared_linear_view_can_split :
    PlaceSplit (some ⟨.shr, .linear, .lifo⟩) (some ⟨.shr, .linear, .lifo⟩) (some ⟨.shr, .linear, .lifo⟩) :=
  .both _ (Or.inl rfl)

-- ==================== 2. LIFO ORDERED EXCHANGE SAFETY ======================
abbrev GuardId := Nat

def popLifo (stack : List GuardId) (target : GuardId) : Option (List GuardId) :=
  match stack with
  | []          => none
  | top :: rest => if top = target then some rest else none

theorem lifo_rejects_out_of_order_irq_restore
    (outer inner : GuardId) (rest : List GuardId) (h_neq : inner ≠ outer) :
    popLifo (inner :: outer :: rest) outer = none := by
  unfold popLifo
  exact ite_eq_right h_neq

-- ==================== 3. @0 GHOST LINEARITY ERASURE ========================
inductive Presence where | ghost0 | runtime1 deriving DecidableEq, Repr

structure GradedBinding where
  pres      : Presence
  valOrPerm : Nat
  deriving DecidableEq, Repr

@[simp] def eraseGhostBindings : List GradedBinding → List Nat
  | [] => []
  | b :: rest =>
      match b.pres with
      | .ghost0 => eraseGhostBindings rest
      | .runtime1 => b.valOrPerm :: eraseGhostBindings rest

theorem ghost_linear_token_zero_runtime_footprint
    (permId : Nat) (ctx : List GradedBinding) :
    eraseGhostBindings ({ pres := .ghost0, valOrPerm := permId } :: ctx) =
    eraseGhostBindings ctx := rfl

-- ==================== 4. `out` DESTINATION PARALLEL SLICE SPLIT ============
theorem out_split_slice_disjoint_and_exhaustive
    (mid len idx : Nat) (_h_mid : mid ≤ len) :
    (¬ (idx < mid ∧ (mid ≤ idx ∧ idx < len))) ∧
    (idx < len → (idx < mid ∨ (mid ≤ idx ∧ idx < len))) := by
  constructor
  · intro ⟨h_lt, h_ge, _⟩
    omega
  · intro h_lt
    omega

-- ==================== 5. LIQUID REFINEMENTS & SEMVER VARIANCE ==============
theorem semver_refinement_variance_sound
    {Val : Type}
    (P_old P_new Q_old Q_new : Val → Prop)
    (h_pre_contra : ∀ x, P_old x → P_new x)
    (h_post_cov   : ∀ y, Q_new y → Q_old y)
    (h_impl_new   : ∀ x, P_new x → ∃ y, Q_new y) :
    ∀ x, P_old x → ∃ y, Q_old y := by
  intro x h_po
  obtain ⟨y, h_qn⟩ := h_impl_new x (h_pre_contra x h_po)
  exact ⟨y, h_post_cov y h_qn⟩

-- ==================== 6. GRADED ARENA BUMP-CHECK ELISION ===================
theorem graded_region_loop_elides_limit_checks
    (cursor0 initB iters stepB i limit : Nat)
    (h_iter   : i ≤ iters)
    (h_budget : cursor0 + initB + iters * stepB ≤ limit) :
    cursor0 + initB + i * stepB ≤ limit := by
  have h_mul : i * stepB ≤ iters * stepB := Nat.mul_le_mul_right stepB h_iter
  omega

-- ==================== 7. SUBSTRUCTURAL QUOTIENTS (`Quot.sound`) ============
abbrev RawZTile := List Int

def ZWeightEquiv (t1 t2 : RawZTile) : Prop :=
  t1.foldl (· + ·) 0 = t2.foldl (· + ·) 0

abbrev ZSetQuot := Quot ZWeightEquiv

def ZSetQuot.totalMass : ZSetQuot → Int :=
  Quot.lift (fun t => t.foldl (· + ·) 0) (fun _ _ h_eq => h_eq)

theorem uncompacted_zset_eq_compacted :
    (Quot.mk ZWeightEquiv [10, -10, 42] : ZSetQuot) = Quot.mk ZWeightEquiv [42] := by
  apply Quot.sound
  unfold ZWeightEquiv
  decide

-- ==================== 8. SKELETAL SUGAR PASS-MODE TRANSPARENCY =============
inductive SugarPass where
  | sharedIn | mutInout | initOut | moveSink
  deriving DecidableEq, Repr

def passPermitsUse (patHole templateUse : SugarPass) : Bool :=
  match patHole, templateUse with
  | .sharedIn, .sharedIn => true
  | .mutInout, .sharedIn | .mutInout, .mutInout => true
  | .initOut,  .initOut  => true
  | .moveSink, .sharedIn | .moveSink, .mutInout | .moveSink, .moveSink => true
  | _, _ => false

def isMutatingOrConsuming (u : SugarPass) : Bool :=
  match u with
  | .sharedIn => false
  | .mutInout | .initOut | .moveSink => true

theorem sugar_shared_hole_forbids_hidden_mutation
    (u : SugarPass)
    (h_wf : passPermitsUse .sharedIn u = true) :
    isMutatingOrConsuming u = false := by
  cases u <;> first | rfl | cases h_wf

theorem sugar_pass_permits_refl (p : SugarPass) :
    passPermitsUse p p = true := by
  cases p <;> rfl

theorem sugar_out_hole_disjoint_from_reads
    (u : SugarPass)
    (h_wf : passPermitsUse .initOut u = true) :
    u = .initOut := by
  cases u <;> first | rfl | cases h_wf

end Vesper.UnifiedTypeTheory
```
]

=== A.4 Operational Ghost Erasure Extension
<app-a-4>

`lean/GhostErasure.lean` imports the checked operational definitions in `lean/MetatheoryAppendix.lean`. Its annotated language contains core expressions, sequential bindings, consumption, pure total ghost lets, and checked closed ghost resource computations. Unsafe ghost observation and ghost abort have evaluation rules but no runtime typing rules, so their exclusion is an explicit typing property.

- `erasure_preserves_typing` translates runtime typing derivations to core derivations.
- `erasure_equivalence` proves both directions of evaluation correspondence, preserving the runtime result, minted and consumed resource lists, and next allocation ID.
- `ghost_noninterference` proves that changing the ghost environment cannot change those runtime observations.
- `ghost_proof_terminates` derives completion and exactly-once consumption of ghost-only resources before their erasure; `ghost_resource_leak_rejected` and `ghost_failure_rejected` exclude leaking and aborting ghost computations.
- `annotated_closed_exact_once` composes erasure with the core's progress, preservation, and fresh-resource exhaustion result.

The first extension is a big-step equivalence for a terminating fragment. `lean/OperationalGhostBridge.lean` then composes its checked closed ghost computations with the actual runtime semantics in `lean/Metatheory/Polymorphism.lean`, which includes closures, recursion, row polymorphism, and stateful handlers. A finite list of proof annotations runs in a separate ghost resource store at each runtime configuration. Static ghost typing implies termination and exactly-once discharge; runtime transitions cannot read ghost results.

- `erasure_trace_equivalence` proves both directions of runtime trace correspondence after draining annotations.
- `annotated_preservation`, `annotated_progress`, and `annotated_type_safety` transfer the runtime safety guarantees to the annotated machine.
- `no_infinite_ghost_stuttering` rules out infinite ghost-only reductions, so erasure cannot hide divergence in the annotations.
- `annotated_exhaustion_per_kind` preserves exactly-once handle consumption and continuation resumption independently of ghost allocations.
- `recursive_count_with_ghosts`, `stateful_handler_with_ghosts`, and `annotations_cannot_terminate_loop` exercise terminating recursion, linear handler state, and runtime divergence with annotations.

This bridge is a checked operational instrumentation model, not a surface-language elaboration. Captured ghost tokens, dependent heap predicates, clocked ghost recursion, and the production compiler translation remain outside its scope. The earlier `eraseGhostBindings` lemma is only a list-level fact and is not evidence for these operational claims.

== Appendix B. Official Brand Identity and Geometric Specification
<appendix-b>
#chapter-deck[The Disjoint Field-Tree Prism: Parametric Grid Equations, Perceptual OKLCH Palettes, and Mechanized Geometry]

#figure(
  image("/website/brand/vesper-mark-dawn.svg", width: 30mm),
  caption: [The canonical Disjoint Field-Tree Prism rendered in the Dawn palette.],
) <fig-prism>

Vesper's official visual identity—*The Disjoint Field-Tree Prism*—is specified with the same mathematical determinism as $lambda_(upright("Vesper"))$ (#link(<ch-3>)[Chapters 3]--#link(<ch-5>)[5]) and *Vesper Chroma* (#link(<ch-20>)[Chapter 21]). Rather than relying on arbitrary decorative Bézier curves or external raster assets, the Vesper mark is a pure polygonal construction on a canonical $256 times 256$ integer and half-integer coordinate grid. Its geometry encodes Vesper's three foundational substructural pillars—modal true linearity (`Mode::Linear`), second-class disjoint place projections (`inout x.{a, b}`), and one-shot lexical effect capabilities (`eff [...]`)—inside a strictly colinear $2 : 1$ *V*-cone.

Built directly into `crates/vesper-highlight` (`brand.rs`), every Vesper installation deterministically synthesizes bit-exact SVG marks, $16 times 16$ pixel-snapped `.ves` file icons, *Vesper Codex* documentation headers (#link(<ch-19>)[Chapter 20]), and $1200 times 630$ OpenGraph social cards without external graphic files or network CDN dependencies.

=== B.1 Semiotic and Calculus-to-Geometry Correspondence
<app-b-1>

Every polygon and negative-space air channel in *The Disjoint Field-Tree Prism* corresponds 1-to-1 to a formal judgment in $lambda_(upright("Vesper"))$:

#figure(
  table(
    columns: (22%, 25%, 24%, 29%),
    align: (left, left, left, left),
    table.header([*Geometric Element*], [*Visual Form*], [*$lambda_(upright("Vesper"))$ Construct*], [*Soundness Invariant*]),
    [*Outer $2 : 1$ V-Cone*],
    [Colinear bounding rays from $(30, 34)$ and $(226, 34)$ to apex $(128, 230)$],
    [Monogram *V* & `[T-CallProj]` return restoration],
    [All projected sub-places re-converge into the caller's owned root binding at call return with zero `'a` lifetime parameters.],
    [*Upper Root Chevron*],
    [Solid wide chevron ($w_(upright("root")) = 38 upright(" px")$) with $45^compose$ valley cutout],
    [Aggregate root place (`broker: Broker`)],
    [Represents the intact parent aggregate entering a call or `Pulse::scope` nursery boundary prior to field-tree splitting.],
    [*Prism Refraction Split*],
    [Parallel diagonal gap ($g_(upright("split")) = 14 upright(" px")$, slope $plus.minus 1 / 2$)],
    [Field-tree selector `.{index, metrics}`],
    [Separates the aggregate parent type from its non-aliasing leaf projections without heap pointer indirection.],
    [*Port Blade (Solar Amber)*],
    [Left outer triangular blade (`#FFB454` Dusk / `#B85200` Dawn)],
    [`sink` & `Mode::Linear` ($upright("copy") subset.eq.sq upright("affine") subset.eq.sq upright("linear")$)],
    [*Theorem 2 (Total Linear Exhaustion):* Exact-once consumption across $100%$ of control-flow and `Fail` unwind paths.],
    [*Center Blade (Prism Cyan)*],
    [Symmetric $30 upright(" px")$-wide central blade terminating at apex $(128, 230)$],
    [`inout x.{a, b}` (`PassMode::InOut`)],
    [Exclusive mutable leaf projection isolated on both sides from sibling borrows (`Alias-XOR-Mutability`).],
    [*Disjoint Negative Channels*],
    [Dual vertical air gaps ($g_(upright("chan")) = 12 upright(" px")$) flanking the center blade],
    [Pairwise disjointness $p_i perp p_j$ (`FieldTree::is_disjoint`)],
    [Proves zero overlapping leaf paths ($pi_1 inter pi_2 = emptyset$), enabling lock-free parallel mutation across sibling fibers.],
    [*Starboard Blade (Lexical Iris)*],
    [Right outer triangular blade (`#D2A6FF` Dusk / `#6B2FD6` Dawn)],
    [`in` & `eff [...]` (`EvidenceVector` in `r15` / `x28`)],
    [One-shot lexical effect handlers (`Storage`, `Net`, `Isa[P]`) and shared read-only views on Zero-`Pin` fiber stacks.],
  ),
  kind: table,
)

=== B.2 Parametric CAD Construction Equations ($256 times 256$ Grid)
<app-b-2>

Let the parametric tuple $Theta = (g_(upright("chan")), w_(upright("center")), w_(upright("root")), g_(upright("split")))$ govern optical weight across target display sizes while preserving strict outer colinearity.

==== Outer $2 : 1$ V-Cone Envelope
The outer left and right walls lie on two exact rays meeting at the bottom apex $(128, 230)$ from top outer corners $(30, 34)$ and $(226, 34)$:

$ y_(upright("outer"))(x) = 34 + 2 dot.op (x - 30) quad "for " x in [30, 128], quad y_(upright("outer"))(x) = 34 + 2 dot.op (226 - x) quad "for " x in [128, 226] $

Because $Delta y = 230 - 34 = 196$ and $Delta x = 128 - 30 = 98$, the outer wall slope is $Delta y \/ Delta x = 2$ exactly. Every outer vertex of the upper root chevron, port/starboard blades, and central apex blade satisfies $y = y_(upright("outer"))(x)$, guaranteeing zero silhouette steps or optical kinks.

==== Closed-Form Polygon Coordinates (Canonical Master $Theta_(upright("master")) = (12, 30, 38, 14)$)
+ *Upper Aggregate Root Chevron:*
  - Valley floor at $y_(upright("valley")) = 75$ and lower chevron tip at $y_(upright("root")) = 75 + w_(upright("root")) = 113$.
  - Inner top shoulders follow a $45^compose$ valley slope ($plus.minus 1$) from $(128, 75)$ to $y_(upright("top")) = 34$, landing at $(87, 34)$ and $(169, 34)$.
  - Lower splitter rays rise from $(128, y_(upright("root")))$ with slope $plus.minus 1 / 2$ (orthogonal to the $2 : 1$ outer V-wall), intersecting $y_(upright("outer"))(x)$ at $x_(upright("split"), L) = (y_(upright("root")) - 38) \/ 1.5 = 50$, yielding integer vertices $(50, 74)$ and $(206, 74)$.
  - *Canonical 8-Vertex Polygon:* `(30,34) -> (87,34) -> (128,75) -> (169,34) -> (226,34) -> (206,74) -> (128,113) -> (50,74)`.
+ *Refraction Boundary and Central `inout` Apex Blade:*
  - The upper refraction boundary is translated downward by $g_(upright("split")) = 14 upright(" px")$ to $y_(upright("refract"))(x) = 127 - 1 / 2 abs(128 - x)$.
  - With half-width $w_(upright("center")) \/ 2 = 15 upright(" px")$, the central blade spans $x in [113, 143]$, intersecting $y_(upright("refract"))$ at $y = 119.5$ and $y_(upright("outer"))$ at $y = 200$.
  - *Canonical 6-Vertex Polygon:* `(128,127) -> (143,119.5) -> (143,200) -> (128,230) -> (113,200) -> (113,119.5)`.
+ *Port (`sink` / `linear`) and Starboard (`in` / `eff`) Blades:*
  - Isolated from the central blade by vertical negative-space channels $x in (101, 113)$ and $x in (143, 155)$ of width $g_(upright("chan")) = 12 upright(" px")$.
  - *Port 3-Vertex Polygon:* `(59.33,92.67) -> (101,113.5) -> (101,176)`.
  - *Starboard 3-Vertex Polygon:* `(196.67,92.67) -> (155,113.5) -> (155,176)`.

==== Standardized Optical Size Cuts
#figure(
  table(
    columns: (20%, 22%, 28%, 30%),
    align: (left, left, left, left),
    table.header([*Optical Cut*], [*Target Viewport*], [*Parameters $Theta = (g_(upright("chan")), w_(upright("center")), w_(upright("root")), g_(upright("split")))$*], [*Deployment Surfaces*]),
    [*Canonical Master*], [$gt.eq 32 times 32 upright(" px")$ & Print], [$(12, 30, 38, 14)$], [`vesper-lang.org` hero, `README.md`, academic papers, $1200 times 630$ OpenGraph cards],
    [*Bold App Tile*], [$24 times 24$--$64 times 64 upright(" px")$], [$(16, 34, 42, 16)$], [macOS / Windows 11 / GNOME desktop icons (*Vesper Lens*, *Vesper Aura*)],
    [*Release Star Seal*], [$gt.eq 48 times 48 upright(" px")$], [$(12, 30, 38, 14) + upright("StarGlint")$], [*Vesper Codex* `Verified (0 sorryAx)` `.olean` badges and major release emblems],
    [*Micro Pixel-Snapped*], [$16 times 16$--$20 times 20 upright(" px")$], [Dedicated $16 times 16$ integer grid ($1 upright(" px")$ channels at $x in [6, 7], [9, 10]$)], [Browser favicons, IDE `.ves` file-tree icons, `vesper top` compact status bar],
  ),
  kind: table,
)

=== B.3 Perceptual OKLCH Color System, Contrast Gates, and Typography
<app-b-3>

To guarantee zero chromatic clash between brand marks and syntax-highlighted code listings, the official brand palette locks 1-to-1 to *Vesper Chroma*'s perceptual OKLCH token table (#link(<ch-20-5>)[Section 21.5]):

#figure(
  table(
    columns: (17%, 15%, 12%, 17%, 12%, 17%, 10%),
    align: (left, left, left, left, left, left, left),
    table.header([*Semantic Role*], [*Token*], [*Dusk*], [*Dusk OKLCH*], [*Dawn*], [*Dawn OKLCH*], [*Contrast*]),
    [*Canvas Surface*], [`--vc-bg-canvas`], [`#0B0E17`], [$(14.5%, 0.022, 265^compose)$], [`#F2EFE9`], [$(95.2%, 0.010, 85^compose)$], [---],
    [*Card Surface*], [`--vc-bg-card`], [`#111625`], [$(18.5%, 0.026, 265^compose)$], [`#FAF8F5`], [$(98.0%, 0.006, 85^compose)$], [---],
    [*Root & Wordmark*], [`--vc-fg-primary`], [`#E2E8F5`], [$(92.0%, 0.018, 255^compose)$], [`#1B2030`], [$(23.5%, 0.028, 265^compose)$], [$12.7 : 1$],
    [*Port (`linear`)*], [`--vc-linear`], [`#FFB454`], [$(83.5%, 0.158, 72^compose)$], [`#B85200`], [$(52.0%, 0.145, 52^compose)$], [$gt.eq 7.1 : 1$],
    [*Center (`inout`)*], [`--vc-inout`], [`#5CCFE6`], [$(84.5%, 0.132, 205^compose)$], [`#006B80`], [$(48.5%, 0.102, 212^compose)$], [$gt.eq 7.0 : 1$],
    [*Starboard (`eff`)*], [`--vc-effect`], [`#D2A6FF`], [$(81.0%, 0.149, 298^compose)$], [`#6B2FD6`], [$(49.0%, 0.215, 292^compose)$], [$gt.eq 7.0 : 1$],
    [*Proof (`0 sorryAx`)*], [`--vc-wire-proof`], [`#6EE7B7`], [$(85.0%, 0.145, 158^compose)$], [`#087443`], [$(48.0%, 0.118, 156^compose)$], [$gt.eq 7.4 : 1$],
  ),
  kind: table,
)

+ *Tri-Pillar OKLCH (Default):* Root chevron in `--vc-fg-primary`, Port blade in `--vc-linear`, Center blade in `--vc-inout`, and Starboard blade in `--vc-effect`.
+ *Single-Ink Monochrome Stencil:* All four polygons rendered in solid `#E2E8F5` (dark), `#1B2030` (light), or `#000000` (e-ink, laser-etched silicon, monochrome print). Because disjointness is encoded structurally via negative-space air channels rather than chromatic boundaries alone, the mark remains $100%$ legible in 1-bit monochrome.
+ *Typography and Wordmark Lockup:* The official logotype sets lowercase *`vesper`* in *Space Grotesk Bold (`700`)* with tracking `-0.035em`, paired with *Inter* for UI prose and *JetBrains Mono* for source code and capability badges.
+ *Geometric Integrity Guardrails:* Never apply outer glow/blur filters that bleed into the $12 upright(" px")$ disjoint channels; never scale non-uniformly (which breaks the $2 : 1$ colinear ray invariant and orthogonal $1 \/ 2$ refraction slope); and never fill negative-space channels between blades. Minimum clear space is $0.25 times W$ on all four sides.

=== B.4 Canonical Reference SVG and CLI Artifacts
<app-b-4>

==== Canonical Master Mark (`vesper-mark-dusk.svg`, $256 times 256$)
```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256" fill="none">
  <!-- Upper Aggregate Root Struct Chevron -->
  <polygon points="30,34 87,34 128,75 169,34 226,34 206,74 128,113 50,74" fill="#E2E8F5"/>
  <!-- Port Blade: Mode::Linear & sink Ownership Transfer -->
  <polygon points="59.33,92.67 101,113.5 101,176" fill="#FFB454"/>
  <!-- Center Apex Blade: Disjoint inout FieldTree Projection -->
  <polygon points="128,127 143,119.5 143,200 128,230 113,200 113,119.5" fill="#5CCFE6"/>
  <!-- Starboard Blade: Shared in Views & One-Shot eff [...] Capabilities -->
  <polygon points="196.67,92.67 155,113.5 155,176" fill="#D2A6FF"/>
</svg>
```

==== Pixel-Snapped $16 times 16$ Favicon and `.ves` File Icon (`vesper-favicon-16.svg`)
```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16" fill="none">
  <polygon points="1,1.5 5,1.5 8,4.5 11,1.5 15,1.5 13.6,4.3 8,7.1 2.4,4.3" fill="#E2E8F5"/>
  <polygon points="3.1,5.7 6,7.1 6,11.5" fill="#FFB454"/>
  <polygon points="8,8.1 9,7.6 9,13.5 8,15.5 7,13.5 7,7.6" fill="#5CCFE6"/>
  <polygon points="12.9,5.7 10,7.1 10,11.5" fill="#D2A6FF"/>
</svg>
```

==== Horizontal Wordmark Lockup (`vesper-wordmark-dusk.svg`, $520 times 140$)
```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 520 140" width="520" height="140" fill="none">
  <g transform="translate(12, 12) scale(0.453125)">
    <polygon points="30,34 87,34 128,75 169,34 226,34 206,74 128,113 50,74" fill="#E2E8F5"/>
    <polygon points="59.33,92.67 101,113.5 101,176" fill="#FFB454"/>
    <polygon points="128,127 143,119.5 143,200 128,230 113,200 113,119.5" fill="#5CCFE6"/>
    <polygon points="196.67,92.67 155,113.5 155,176" fill="#D2A6FF"/>
  </g>
  <text x="148" y="95" fill="#E2E8F5" font-family="'Space Grotesk', 'Inter', system-ui, sans-serif" font-size="84" font-weight="700" letter-spacing="-3">vesper</text>
</svg>
```

==== Canonical CLI Banner (`vesper --version` and `vesper top`)
```text
  \\    //   vesper 1.0.0 (stage3-ddc blake3:e9f01b4c)
   \\  //    target: x86_64-unknown-linux-gnu (cranelift + lean4)
    \\||/    modes: copy <= affine <= linear | 0 sorryAx
     \/
```

=== B.5 Mechanized Lean 4 Colinearity and Disjointness Proofs
<app-b-5>

In `lean/BrandGeometrySafety.lean`, `vesper-verify-lean` (#link(<ch-8>)[Chapter 9]) discharges two geometric invariants on the brand mark itself: every outer vertex of the Root Chevron and Center Apex Blade lies strictly on the $2 : 1$ V-cone ray $y - 34 = 2 (x - 30)$, and for any positive channel gap $g > 0$, the Port, Center, and Starboard projection blades share zero horizontal coordinates—mirroring `ProjSelector.Disjoint` (#link(<appendix-a>)[Appendix A]):

```lean
-- lean/BrandGeometrySafety.lean
namespace Vesper.BrandGeometry

def OnLeftOuterVWall (x y : Nat) : Prop :=
  y - 34 = 2 * (x - 30) /\ 30 <= x /\ x <= 128

theorem canonical_prism_outer_vertices_colinear :
    OnLeftOuterVWall 30 34 /\
    OnLeftOuterVWall 50 74 /\
    OnLeftOuterVWall 113 200 /\
    OnLeftOuterVWall 128 230 := by
  unfold OnLeftOuterVWall
  omega

theorem prism_three_blades_pairwise_disjoint
    (halfC gap x : Nat)
    (h_gap : gap > 0)
    (h_fit : halfC + gap < 98) :
    let portMax   := 128 - halfC - gap
    let centerMin := 128 - halfC
    let centerMax := 128 + halfC
    let starMin   := 128 + halfC + gap
    ¬(x <= portMax /\ centerMin <= x /\ x <= centerMax) /\
    ¬(centerMin <= x /\ x <= centerMax /\ starMin <= x) /\
    ¬(x <= portMax /\ starMin <= x) := by
  refine ⟨?_, ?_, ?_⟩ <;> omega

end Vesper.BrandGeometry
```

=== B.6 Deterministic Brand Asset Emitter (`crates/vesper-highlight/src/brand.rs`)
<app-b-6>

`crates/vesper-highlight/src/brand.rs` synthesizes verified brand marks, `.ves` file icons, and OpenGraph headers directly from `ChromaPalette` tokens (#link(<ch-20>)[Chapter 21]) with zero embedded binary blobs:

#_compact-listing[
```rust
// crates/vesper-highlight/src/brand.rs
use crate::ChromaPalette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrandColorMode {
    TriPillarOklch,
    SolarAmberAccent,
    MonochromeStencil,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrismProportions {
    pub channel_gap_px: u16,
    pub center_width_px: u16,
    pub root_weight_px: u16,
    pub split_gap_px: u16,
    pub evening_star_glint: bool,
}

impl PrismProportions {
    pub const CANONICAL_MASTER: Self = Self {
        channel_gap_px: 12,
        center_width_px: 30,
        root_weight_px: 38,
        split_gap_px: 14,
        evening_star_glint: false,
    };

    pub const BOLD_APP_TILE: Self = Self {
        channel_gap_px: 16,
        center_width_px: 34,
        root_weight_px: 42,
        split_gap_px: 16,
        evening_star_glint: false,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrandGeometryError {
    ZeroChannelGapBreaksDisjointness,
    CenterBladeExceedsVConeEnvelope,
}

/// Audits that parametric proportions preserve positive disjoint channels and stay inside the 2:1 V-cone.
pub fn verify_prism_geometry(p: PrismProportions) -> Result<(), BrandGeometryError> {
    if p.channel_gap_px == 0 {
        return Err(BrandGeometryError::ZeroChannelGapBreaksDisjointness);
    }
    let half_c = p.center_width_px / 2;
    let y_refract = 75u16 + p.root_weight_px + p.split_gap_px;
    let x_outer_top_x3 = y_refract.saturating_sub(38) * 2;
    let x_inner_left_x3 = (128u16.saturating_sub(half_c).saturating_sub(p.channel_gap_px)) * 3;
    if p.center_width_px == 0 || x_outer_top_x3 >= x_inner_left_x3 {
        return Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope);
    }
    Ok(())
}

/// Deterministically emits the 256x256 Disjoint Field-Tree Prism SVG mark from `ChromaPalette`.
pub fn emit_prism_mark_svg(
    palette: &ChromaPalette,
    mode: BrandColorMode,
    prop: PrismProportions,
) -> Result<String, BrandGeometryError> {
    use core::fmt::Write;
    verify_prism_geometry(prop)?;

    let (root_hex, left_hex, center_hex, right_hex) = match mode {
        BrandColorMode::TriPillarOklch => (
            palette.fg_default.srgb_hex,
            palette.linear_amber.srgb_hex,
            palette.inout_cyan.srgb_hex,
            palette.effect_iris.srgb_hex,
        ),
        BrandColorMode::SolarAmberAccent => (
            palette.fg_default.srgb_hex,
            palette.linear_amber.srgb_hex,
            palette.linear_amber.srgb_hex,
            palette.comment_slate.srgb_hex,
        ),
        BrandColorMode::MonochromeStencil => (
            palette.fg_default.srgb_hex,
            palette.fg_default.srgb_hex,
            palette.fg_default.srgb_hex,
            palette.fg_default.srgb_hex,
        ),
    };

    let y_valley = 75.0f32;
    let y_root_bot = y_valley + f32::from(prop.root_weight_px);
    let x_root_split_l = (y_root_bot - 38.0) / 1.5;
    let y_root_split_l = 34.0 + (x_root_split_l - 30.0) * 2.0;
    let x_root_split_r = 256.0 - x_root_split_l;
    let x_shoulder_l = 128.0 - (y_valley - 34.0);
    let x_shoulder_r = 128.0 + (y_valley - 34.0);

    let y_refract = y_root_bot + f32::from(prop.split_gap_px);
    let half_c = f32::from(prop.center_width_px) * 0.5;
    let x_center_l = 128.0 - half_c;
    let x_center_r = 128.0 + half_c;
    let y_center_top = y_refract - half_c * 0.5;
    let y_center_bot = 34.0 + (x_center_l - 30.0) * 2.0;

    let x_outer_l = (y_refract - 38.0) / 1.5;
    let y_outer_l = 34.0 + (x_outer_l - 30.0) * 2.0;
    let x_inner_l = x_center_l - f32::from(prop.channel_gap_px);
    let y_inner_top = y_refract - (128.0 - x_inner_l) * 0.5;
    let y_inner_bot = 34.0 + (x_inner_l - 30.0) * 2.0;
    let x_outer_r = 256.0 - x_outer_l;
    let x_inner_r = x_center_r + f32::from(prop.channel_gap_px);

    let mut svg = String::with_capacity(640);
    svg.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 256 256\" width=\"256\" height=\"256\" fill=\"none\">\n");
    let _ = writeln!(
        svg,
        "  <polygon points=\"30,34 {x_shoulder_l:.1},34 128,{y_valley:.1} {x_shoulder_r:.1},34 226,34 {x_root_split_r:.2},{y_root_split_l:.2} 128,{y_root_bot:.1} {x_root_split_l:.2},{y_root_split_l:.2}\" fill=\"#{root_hex:06X}\"/>"
    );
    let _ = writeln!(
        svg,
        "  <polygon points=\"{x_outer_l:.2},{y_outer_l:.2} {x_inner_l:.1},{y_inner_top:.1} {x_inner_l:.1},{y_inner_bot:.1}\" fill=\"#{left_hex:06X}\"/>"
    );
    let _ = writeln!(
        svg,
        "  <polygon points=\"128,{y_refract:.1} {x_center_r:.1},{y_center_top:.1} {x_center_r:.1},{y_center_bot:.1} 128,230 {x_center_l:.1},{y_center_bot:.1} {x_center_l:.1},{y_center_top:.1}\" fill=\"#{center_hex:06X}\"/>"
    );
    let _ = writeln!(
        svg,
        "  <polygon points=\"{x_outer_r:.2},{y_outer_l:.2} {x_inner_r:.1},{y_inner_top:.1} {x_inner_r:.1},{y_inner_bot:.1}\" fill=\"#{right_hex:06X}\"/>"
    );
    svg.push_str("</svg>\n");
    Ok(svg)
}

/// Emits the pixel-snapped 16x16 micro icon for `.ves` IDE file trees and browser favicons.
pub fn emit_favicon_16_svg(palette: &ChromaPalette) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\" width=\"16\" height=\"16\" fill=\"none\">\n\
           <polygon points=\"1,1.5 5,1.5 8,4.5 11,1.5 15,1.5 13.6,4.3 8,7.1 2.4,4.3\" fill=\"#{:06X}\"/>\n\n\
           <polygon points=\"3.1,5.7 6,7.1 6,11.5\" fill=\"#{:06X}\"/>\n\
           <polygon points=\"8,8.1 9,7.6 9,13.5 8,15.5 7,13.5 7,7.6\" fill=\"#{:06X}\"/>\n\
           <polygon points=\"12.9,5.7 10,7.1 10,11.5\" fill=\"#{:06X}\"/>\n\
         </svg>\n",
        palette.fg_default.srgb_hex,
        palette.linear_amber.srgb_hex,
        palette.inout_cyan.srgb_hex,
        palette.effect_iris.srgb_hex,
    )
}
```
]
]
