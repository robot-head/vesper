# Implementation coverage

This checkpoint implements the core compiler and runtime baseline from sections 1–7 of the design. The current [expanded specification](specification.txt) also contains sections 8–19: Loom, SemVer checking, workspace architecture, a larger Lean extractor, Pulse, Bridge, Vector, Strata, Pact/Forge, Prism, Aura, and Ignis. Selected additions now have bounded workspace library implementations listed below; source-interface comparison and offline-resolution CLI adapters are now implemented, while other source integrations remain future work. The [original core baseline](core-specification-baseline.txt) is retained for comparison.

The first workspace extraction now separates `vesper-core`, `vesper-syntax`, and `vesper-pulse` while preserving the root facade. See [parallel lanes](parallel-work.md) for active assignments and handoff prompts.

## Pipeline and source layout

| Component | Implementation |
| --- | --- |
| Lexing | `crates/vesper-syntax/src/lexer.rs`: Logos tokens, preserved whitespace/comments, nested block comments, byte ranges, lexical diagnostics. |
| Parsing | `crates/vesper-syntax/src/parser.rs`: recovering recursive descent, Pratt precedence, generic/index distinction, Rowan CST plus source-ranged surface HIR. |
| Types/modes | `src/check.rs`: declaration resolution, generic substitution/bounds, composite modal kinding, type compatibility, function modes, inference from arguments and constructors. |
| Effects | Lexical capability resolution, explicit effect rows and row-variable instantiation, `Fail`/`?`, frame checks, handler/continuation checking, closure capability escape checks. |
| Ownership/places | Path-sensitive environments, branch merging, moves, exact-once obligations, nested pattern coverage, shared/mutable/sink argument checking, nested active argument loans, projection permissions. |
| Incrementality | `src/database.rs`: Salsa source inputs and memoized parse/check/diagnostic queries at module granularity. |
| Tooling | Ariadne CLI diagnostics; `src/lsp.rs` provides stdio initialization, shutdown, open/change/close, UTF-16 edits, and diagnostic publication. |
| Shared IR | `src/ir.rs`: basic blocks, hygienic slots, branches, match edges, typed block parameters, place arguments, move summaries, and path-sensitive state-token parameters/phis and ordered call/write/effect tokens; Petgraph field ancestry graph. `E0411` independently validates evidence flow. |
| Optimization | `src/optimize.rs`: pure integer equality saturation with `egg`, wrapping constant folding, and `src/ir_opt.rs` effectful `egglog` saturation. State-token SSA and PCG disjointness guard read forwarding; calls and overlapping writes invalidate observations. |
| Evaluator | `src/runtime.rs`: explicit CEK control/frame machine, values, closures, mutable place slots, methods, pattern matching, stateful handlers, aborts, and one-shot delimited continuations. |
| Native backend | `src/native.rs`: Cranelift lowering from shared Place IR, concrete inline record/enum layouts, monomorphization, JIT/object emission, mutable place ABI, stack-resident closure specialization, native one-shot return dispatch, and abort/Fail exits. |
| Verification backend | `src/lean.rs`: Lean data declarations, `src/lean_ir.rs` extracts pure CFGs with live-slot block arguments, closure lifting, and explicit mutable state returns. `src/lean.rs` emits data/effect declarations and the remaining effect-evidence translations. |
| Formal foundations | `lean/Foundations.lean`: checked mode lattice, submoding, exact-use, and one-shot transition lemmas. |
| Local artifact audits | `src/proof_commands.rs` and `tools/proof-audit/Audit.lean`: independent Lean kernel replay, explicit transitive axiom policy and input inventories for existing local artifacts; no reusable package proof certificate. |

The executable pipeline checks HIR, runs CFG ownership analysis on typed Place IR, optimizes pure expressions, rechecks, then executes or generates Cranelift from that IR. The evaluator and native backend share hygienic slot identities, value types, control flow, and projected call arguments. Lexical handler and closure installation remain explicit high-level nodes in the IR; their bodies and clauses are independently lowered, checked, optimized, and executed as shared CFGs. Native effect-helper specialization now lowers its shared CFG to a local return join with scoped capability evidence. State tokens have separate block parameters and predecessor phis, checked before and after effectful optimization. Native one-shot continuations use resume blocks and return dispatch within a specialized activation. Source-generated external parking remains pending. The independent fiber ABI prototype parks hand-written CLIF entries on pinned machine stacks; it does not relocate physical stacks.

## Expanded-design library milestones

| Workspace library | Implemented and validated | Remaining connection |
| --- | --- | --- |
| `vesper-semver` | Versioned interface DTOs, conservative compatibility rules, checked source export, `vesper semver` and proposed release-version checks | Final `.vhir` schema, richer scoped contracts, canonical MIF, executable witnesses |
| `vesper-ffi` | Validated wire layouts, endian access, safe zero-copy byte views | Source syntax, HIR/PCG, CLIF, `.vsys`, hardware isolation |
| `vesper-loom-calculus` | Manifests, monotonic feature closure, exact-version solution/declared ceiling validation and offline `vesper graph` inspection | Compiler authority audit, registry/trust/build execution |
| `vesper-loom-solver` | Bounded iterative offline resolution, transitive features/native requirements, `vesper resolve`, atomic solution snapshots and read-only `--locked` validation against current inputs | CDCL/Max-SMT, authenticated lockfiles, optimization/trust policies |
| `vesper-alloc` | Externally implemented host-region allocator; registered for workspace testing after upstream merge | Source Region/Alloc typing/lowering, exhaustion policy, ShardSlab and collection integration |
| `vesper-simpulse` | Seeded scheduling, virtual timers, actual completion ownership, strict journal replay | Production adapters, source/CLI simulation, DPOR, network/WAL models |
| `vesper-fiber-abi` | Versioned evidence ABI, pinned-stack native CLIF park/resume prototype and Pulse adapter | Compiler source lowering, stack growth, portable scheduling, physical relocation |
| `vesper-pact` | Canonical trait goals, bounded tabled solver, named models, coherence and proof DTOs | Source traits/models, checker integration, complete higher-rank reasoning |
| `vesper-loom-cas` | BLAKE3 blob/tree store, atomic ingestion, independent materialization, distinct artifact/proof records | CLI/build graph, compiler-derived keys, registry/proof verification |
| `vesper-loom-sandbox` | Real Wasmtime component execution, denied ambient access, deterministic inputs, fuel/memory limits | Forge hygiene/provenance, Loom/CAS integration, enforced disk quota |
| `vesper-ignis` | Deterministic resource hazards, enhanced-barrier plans and queue fence streams | Windows/D3D12 executor and hardware validation, source lowering, full engine |

The [contract audit](rfcs/parallel-contracts.md) records checked-arithmetic migration, scoped child loans, native relocation/DMA rules, region aliasing/rewind, and distinct interface/artifact/proof cache keys. These are integration gates, not new proof claims.

## Backend boundaries

| Feature | Evaluator | Native | Lean extraction |
| --- | --- | --- | --- |
| Scalar arithmetic, functions, branching, recursion | Yes | Yes | Yes |
| Structs/enums, destructuring, methods | Yes | Yes, inline layouts and return buffers | Core cases supported |
| Generic functions | Checked and evaluated | Monomorphized scalar/data instantiations | Type parameters |
| Scalar `inout` | Yes | Yes, pointer ABI | Explicit state returns in arbitrary pure expressions |
| Field-tree projections | Yes | Yes | CFG state passing, including disjoint arguments sharing a root |
| Closures | Yes, captures are value snapshots | Stack-resident descriptors, factories, higher-order calls, mutable parameters | Pure/effectful closures, higher-order calls, returned factories, mutable pure parameters |
| Tail-resumptive handlers | Yes | Yes, shared CFGs and native branches | Lexical CPS handlers |
| Aborting/general one-shot handlers | Yes | Native return dispatch, including non-tail resumption and linear state | Lexical CPS handlers, including non-tail resumption, mutable operation arguments and handler frames |
| Open effect rows | Argument-driven row instantiation | Specialized row evidence and effectful callbacks | Monadic evidence, open callback rows, explicit row arguments |
| `Fail` and `?` | Yes | Yes | CPS Fail handlers |

Native effectful helpers specialize into the enclosing lexical handler. Generic functions are instantiated for concrete argument types (including records and floats), with a 1,024-instance guard against unbounded polymorphic recursion. Effect specialization has a 64-inline-frame guard. Records and enums use aligned inline layouts, preserve mutable value semantics, and return aggregates through caller-owned buffers. Recursive inline types require a future owning indirection. An exported dynamic closure ABI, unbounded recursive effect specialization, and native runtime suspension remain pending. One-shot return dispatch and mutable place synchronization support non-tail and nested native resumptions without evaluator fallback. Both x86_64 and AArch64 objects are generated. All backend differential cases also emit AArch64 ELF objects; all eight examples have been linked and executed under QEMU using `scripts/test-aarch64.sh`.

Lean extraction supports the cases in `tests/lean.rs`, including nested branch/match mutation, calls returning both values and state, disjoint mutable arguments sharing a root, pure closures and higher-order functions, closure capture snapshots, generic ADT dependencies, and mutually recursive structs/enums. Scalar entry points are executed in Lean and compared with the reference evaluator. It uses executable `partial def` translations and does not establish termination. Lexical handlers, division/indexing failures, named forwarding, and effectful helpers have executable CPS translations. Effectful callbacks, open rows, and deferred factories are supported, including lifting outer callbacks through nested handlers. Mutable operation arguments now use explicit state returns and resumption lenses, including nested handlers and direct non-tail resumption. Writes after resumption through helpers/callbacks/closures and first-class continuations with nonempty mutable frames are rejected. Recursive negative data types and callback aggregates across monads still require more lowering; extraction is not a proof certificate for arbitrary accepted source.

## Remaining work required for the full specification

1. Finish general bidirectional metavariable/effect-row unification. Factory results, function values, higher-order callbacks, annotated generic containers, assignments, and mutable argument boundaries now preserve lexical capability identities. Modal kinding distinguishes recursive instantiations by argument modes and validates nested generic bounds. Ambiguous unconstrained values still require annotations.
2. Complete the remaining lexical/effect lowering and heap-layout cases. Integer literal ranges, strict signed/unsigned type compatibility, wrapping u8/u32/u64/i64 arithmetic, unsigned comparisons/division, signed minima, and floating remainder are now covered by interpreter/native differential tests. Integer literals use contextual types and default to i64 (u64 above i64::MAX).
3. Lower the remaining high-level handler/closure nodes fully, add drop elaboration and owning heap indirections, and extend CFG/PCG analysis to the runtime representations. Shared IR execution, native record/enum layouts, monomorphization, and linear-temporary CFG analysis are implemented.
4. Extend effectful `egglog` optimization beyond scalar read forwarding and PCG-disjoint writes to full evidence specialization and remaining high-level handlers. Path-sensitive state tokens and independently validated predecessor phis are implemented; opaque effectful operations remain barriers.
5. Complete the exported dynamic closure ABI, unbounded effectful/higher-order recursion, and connect native suspension to the runtime components. `crates/vesper-pulse/src/frames.rs` implements uniquely owned segmented data frames with checked relative addresses and relocation; `lean/RuntimeFrames.lean` proves read preservation for that representation. `crates/vesper-pulse/src/scheduler.rs` implements one-shot completion scheduling, nursery joins, cancellation, and error ownership. These components do not yet replace native machine activations. Inline aggregate ABIs, stack-resident closures, and one-shot return dispatch are implemented.
6. Integrate the implemented Linux io_uring driver with native parked continuations, and add source-level nursery/allocator handlers, buffers/collections, and a defined standard library. `Runtime::begin_external` and `src/io_session.rs` now park and resume the reference evaluator through the completion scheduler, including typed I/O errors and cancellation while linear obligations remain live. The driver owns requests and continuations through I/O completion and cancellation, preserves completions on polling errors, and returns all tokens during explicit shutdown. The document's `Storage`, `BufferPoolSlice`, `RecordBatch`, and broker methods are example APIs, not supplied standard-library definitions. Users can declare and handle their own effects now.
7. Complete Lean extraction for the remaining effects/recursive representations, and formalize the language operational semantics and the document's preservation, exhaustion, and relocation theorems. Foundation lemmas and compiler tests do not prove these theorems.
8. Add incremental per-item dependency tracking, performance benchmarks, broader LSP features, cross-platform runtimes, and broader native AArch64 validation.

## Specification choices

The original core document left literal formats, operator precedence, integer overflow, standard-library APIs, binary layouts, and several elaboration details unspecified. Expanded §11.2 now requires checked arithmetic; coordinated migration across evaluator, optimizer, native and Lean remains pending. This checkpoint retains wrapping integer overflow and rejects implicit conversions between concrete integer types. It adds conventional arithmetic precedence, unary operators, boolean short-circuiting, strings, nested comments, scientific float literals, and qualified enum constructors. The evaluator bounds execution using `--fuel`; native programs are not subject to that limit.

The originally supplied field formula permitted `@@ copy` to hide a linear payload. The user approved a correction: each field must satisfy `k_i ⊑ m_i` under its declaration's generic bounds. Both the Google Doc and local specification now contain that premise. `E0105` implements it; `lean/SpecificationCounterexample.lean` demonstrates why the original unrestricted rule was insufficient.

`Option`, `Result`, `RuntimeError`, and `Fail` are built in. Integer division/remainder and dynamic indexing require `Fail[RuntimeError]` unless a constant or a simple nonzero guard proves the operation safe. Inout calls and assignments invalidate those facts. Primitive failures receive the same linear-frame and handler-state checks as explicit abortable operations. Floating division keeps IEEE semantics. Native source strings use a closed-module inline capacity determined by its string literals and patterns; they require no runtime allocation. Re-declaring them is a duplicate-declaration error. Values have no first-class reference type. Dynamic tuple indexing is restricted to homogeneous copy elements; owned non-copy aggregates must be destructured. Match coverage specializes constructor/payload matrices, with a conservative recursion-depth bound.

The Rust evaluator uses managed host values and reference-counted frame/slot objects. That is a reference implementation of language execution, not the document's promised allocation-free native fiber runtime. Native scalar programs and specialized scalar handlers do not require those evaluator objects.

## Host validation constraints

A direct `io_uring_setup(2)` probe on this managed host returns `EPERM` (operation not permitted). Native kernel-driver integration cannot be exercised on this host. The request ledger and cancellation/error ownership tests pass, but the actual kernel round-trip test is skipped on this host. The driver, scheduler, nursery, frame storage, and reference-evaluator I/O bridge are implemented. Mock completion tests exercise source suspension with live linear resources; actual kernel integration tests are skipped here. The native fiber prototype can exercise the scheduler with fixture completions here; source-to-native suspension integration remains pending.

Checked source-interface and offline-resolution usage, exit behavior, and format limits are documented in [package-workflows.md](package-workflows.md).
