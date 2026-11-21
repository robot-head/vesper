# Self-hosting plan

> Written 2026-11-19 against `main` at `9e801e7` (claim #1122). Updated
> 2026-11-19 at `b35b515` with the SH0 results and the SH1 plan (part of
> #1130), and 2026-11-20 at `bb5cee6` with the checker census and the SH2
> plan (#1130), and 2026-11-20 at `736d756` with the SH2 scoreboard (from the
> lanes' reports) and the SH3 plan (#1187). The
> [manuscript](../manuscript/README.md) is the sole specification; §10
> ("Verified Bootstrap") defines Stage 0 to Stage 2. This plan orders the work
> to get there. Confirm live ownership on the issue board before acting.

## Status (2026-11-20)

SH0 has merged except the `vesper-diagnostics` port (#1128 part 2), which
moved into SH1 as lane 1. SH1 is running
([plan](../agent-prompts/self-host-sh1/README.md), #1128, #1145-#1157).
The checker and IR census (#1130, SH1 lane 15) is done: five samples of real
compiler code (layout, the IR check, region type checks, the type table and
`World`, and a native lowering family through the Cranelift shim) match Rust
exactly on 272 corpus worlds, and the checker samples run at 0.34x to 0.87x of
Rust's time ([report](../parallel-results/self-host-census.md)). Its gaps are
row S19 below. SH2 is planned in
[self-host-sh2](../agent-prompts/self-host-sh2/README.md) (#1173-#1187).
The SH0 reports are `docs/parallel-results/self-host-*.md`; their gaps are
S12-S18.

**SH3 started 2026-11-20 on top of open SH2 work, at the owner's request.**
SH2's ports match Rust exactly wherever they have a differential and run at
0.28x to 1.2x of Rust's time when they keep the census data design; one swap
(#1232) is in review and the rest follow in each lane's later PRs
([scoreboard](../parallel-results/self-host-sh2-scoreboard.md), assembled
from the lanes' reports). SH3 is planned in
[self-host-sh3](../agent-prompts/self-host-sh3/README.md) (#1240-#1253):
the Place IR data package and swap point, an e-graph, six gap lanes (row S20),
the IR pipeline ports, and the single-build re-measurement of SH2 and SH3
with the SH4 plan last.

**SH4 planned 2026-11-21 on top of open SH3 work, at the owner's request**
(part of #1253; the single-build re-measure stays with #1253). SH4 ports the
backends, about 49k lines, in
[self-host-sh4](../agent-prompts/self-host-sh4/README.md) (#1275-#1292):
a codegen swap point that lends Cranelift's builder to Vesper so native
lowering swaps one family at a time, encode-once swap inputs, three gap
lanes (S21), nine native lowering families, the wasm and eBPF backends, the
checks on emitted code, and the SH4 scoreboard with the SH5 plan last.

## Where we are

Vesper is about 4% of the way to a self-hosted compiler, measured in lines.

- **The compiler is Rust.** The root `vesper` package is about 157k lines of
  non-test Rust in `src/`. The 12 toolchain crates it links add about 42k more.
  So about 200k lines of Rust make up the compiler and its own tools.
- **The Vesper compiler package is a Stage 1 front end.**
  `packages/vesper-compiler` is 8.4k lines: a lexer, a parser and part of a
  checker. It was written before sequences and strings existed, so it reads
  its input through `source_byte(i)` and folds the CST into a digest. The
  Stage 1 lock (`bootstrap/stage1.lock`, #891) freezes it, and the verified
  backend compiles it.
- **The libraries are mostly ported.** 28 library crates have Vesper ports
  on `main`. Aura, Simpulse, Pact, the Loom solver and Chorus's front end are
  Vesper-only. The rest keep Rust as the default until they reach parity, under
  the owner's standing rule.
- **The compiler links 18 frozen Rust seed copies** of ported crates (about
  113k lines). It links them because a Rust compiler cannot link a Vesper
  object that it would need itself to build (the seed-copy rule in
  [porting-a-crate.md](porting-a-crate.md)). Eight of them are used only by
  tool subcommands, so they can leave the compiler binary before Stage 2
  (lane SH0-2).

"Fully self-hosted" in this plan means three things:

1. The compiler that builds `main` is a Vesper program. It was built by an
   earlier Vesper compiler, and it rebuilds itself to a bit-identical fixed
   point (Stage 2 in §10.2).
2. The only Rust left in the compiler's closure is behind an `extern`
   boundary, in the class this repository already accepts: vetted crypto,
   OS drivers, wasmtime hosts and the DDC auditor (`vesper-codegen-verified`,
   which stays Rust on purpose as the diverse path).
3. Cranelift is in that class too. **Owner decision, 2026-11-19:** Stage 2
   drives Cranelift through a C-ABI shim (lane SH0-4). A Vesper-native
   backend is a later wave (SH7) and replaces Cranelift only once it matches
   Cranelift.

The standing rule applies throughout: no Rust code is deleted, and no
default build changes, until the Vesper build matches it in both function
and performance.

## The root compiler, by phase

Non-test lines in `src/` (code above the first `#[cfg(test)]`, without
`tests.rs` files), grouped by the phase the self-hosted compiler needs.

| Phase | Lines | Modules | External Rust it uses | Wave |
|---|--:|---|---|---|
| Native lowering (Cranelift) | 38,400 | `native*`, `seq/`, `vector`, `tiled`, `ffi_lowering`, `freestanding`, `native_export` | cranelift-*, target-lexicon, object | SH4 |
| Checker and elaboration | 28,100 | `check`, `capability_effects`, `trace_effects`, `regions`, `integer_widths`, `strings`, `dyn_trait`, `atomics`, `tensor_types`, `loops`, `float_math`, `fip`, `stack_bound`, `layout`, `io_session` | petgraph | SH2 |
| Editor and test tooling | 17,100 | `lsp`, `tooling`, `doctest`, `test_runner` | salsa | SH5 |
| Package and front-end glue | 12,200 | `packages`, `package_graph`, `interface`, `database`, `pact_frontend`, `forge_frontend` | salsa, serde_json | SH1/SH2 |
| CLI commands | 12,200 | `*_commands`, `main`, `lib` | serde_json | SH0-2, SH5 |
| Test oracles | 11,700 | `src/bin/*42` | logos | stays Rust (diverse oracles) |
| IR and optimizer | 10,200 | `ir`, `ir_check`, `ir_opt`, `ir_inline`, `optimize`, `bounds_hoist` | egg, egglog | SH3 |
| Other backends | 9,500 | `wasm_backend`, `ebpf_backend` | wasm-encoder, wit-component | SH4 |
| Verified backend | 9,200 | `verified_codegen` | none | SH6 (see below) |
| Lean extraction and proofs | 8,700 | `lean*`, `verify_commands`, `proof_commands` | none | SH5 |
| Runtime traces | 1,300 | `runtime` | none | SH4 |

Generated runtime: the AOT sequence runtime (`src/seq/aot_runtime.rs`) is
emitted as Cranelift IR into each object, so compiled programs already link
only the C library. That part needs no Rust runtime port.

## Toolchain crates the compiler links

Non-test lines in `src/`. "Dependents" are workspace crates with a normal
dependency on it.

| Crate | Lines | External deps | Dependents | Blocking gaps | Wave |
|---|--:|---|---|---|---|
| `vesper-syntax` | 6,800 | logos, rowan, blake3, serde_json | vesper, doc, fmt, forge, highlight | lossless CST on `ChunkSeq`; parse speed vs logos | SH0-7, SH1 |
| `vesper-core` | 5,800 | serde | vesper, diagnostics, doc, lint, pact-binding, syntax | recursive AST enums with `Box`, interning (SH0-5) | SH1 |
| `vesper-forge` | 5,600 | serde, serde_json | vesper, forge-runner | macro hygiene sets; wasm macro host stays extern | SH1 |
| `vesper-doc` | 7,400 | serde, serde_json | vesper | JSON out via the serde port; fs (SH0-3) | SH5 |
| `vesper-highlight` | 4,500 | rowan, tiny-skia, rustybuzz, png, ttf-parser | vesper | lexical part ports; rasterizing stays extern | SH1 (lexical) |
| `vesper-pact-binding` | 3,400 | serde, sha2 | vesper, pact-bridge | DTOs disappear once the checker calls the Pact port directly | SH2 |
| `vesper-fmt` | 2,300 | blake3 | vesper | needs the new CST (SH0-7) | SH1 |
| `vesper-lsp` | 1,700 | serde, serde_json | vesper | JSON-RPC framing; stdio loop | SH5 |
| `vesper-lean-syntax` | 1,500 | none | vesper, fmt | none known | SH0-6 pilot |
| `vesper-lint` | 1,300 | serde, toml | vesper | TOML config via the loom-calculus port | SH1 |
| `vesper-span` | 600 | none | vesper, diagnostics, lsp | none known | SH0-6 pilot |
| `vesper-diagnostics` | 600 | ariadne, serde_json | vesper | ariadne-compatible rendering | SH0-6 pilot |
| `vesper-pact-bridge` | 200 | serde_json | (depends on vesper) | goes away with the Rust checker | SH2 |
| `vesper-sim-session` | 9,500 | serde, sha2 | (depends on vesper) | drives the compiler; ports after SH5 | SH5 |

## Seed copies the compiler links

Each is the Rust implementation of a crate whose Vesper port already exists.
Lines are the seed's `src/`.

| Seed | Lines | Used by (in `src/`) | Kind | Leaves the compiler when |
|---|--:|---|---|---|
| `vesper-aegis-seed` | 10,000 | `tee_commands` | tool only | out since #1133 (`vesper-tee`) |
| `vesper-bench-seed` | 4,200 | `bench_commands`, via `vesper-bench-pmu` | tool only | out since #1133 |
| `vesper-crucible-seed` | 4,700 | `crucible_commands`, `native_coverage` | mostly tool | SH0-2 (coverage hook stays until SH4) |
| `vesper-grammar-seed` | 6,400 | `grammar_commands` | tool only | out since #1133 |
| `vesper-serde-seed` | 8,600 | `grammar_commands`, `json_commands`, `vesper-lsp` | tool + LSP | SH0-2, then SH5 |
| `vesper-loom-audit-seed` | 1,700 | `audit_commands`, `publish_commands` | tool only | out since #1133 |
| `vesper-loom-solver-seed` | 1,200 | `package_commands` | package tool | out since #1133 |
| `vesper-semver-seed` | 2,100 | `publish_commands`, `package_commands`, `tooling/lint` | package tool + lint | SH0-2, lint part in SH1 |
| `vesper-loom-calculus-seed` | 1,700 | `packages`, `package_graph`, `interface`, LSP, `forge_frontend` | compiler semantics (manifests) | Stage 2 (SH6) |
| `vesper-pact-seed` | 2,700 | `pact_frontend` | compiler semantics (trait resolution) | SH2: the Vesper checker calls `@vesper/pact` directly |
| `vesper-alloc-seed` | 9,500 | `regions` (evaluator, native runtime) | compiler semantics | SH2/SH4 |
| `vesper-tensor-seed` | 8,900 | `tensor_types` (evaluator, wire) | compiler semantics | SH2 |
| `vesper-beacon-seed` | 1,800 | `trace_effects`, `trace_commands` | compiler semantics | SH2 |
| `vesper-ebpf-seed` | 14,300 | `ebpf_backend`, `ebpf_commands`, `stack_bound` | backend | SH4 |
| `vesper-silicon-seed` | 13,500 | `native_verifiers`, `freestanding`, `stack_bound` | backend verifiers | SH4 |
| `vesper-kinetic-seed` | 16,400 | `native_verifiers` | backend verifiers | SH4 |
| `vesper-resonance-seed` | 5,300 | `native_verifiers` | backend verifiers | SH4 |
| `vesper-simpulse-seed` | 500 | via chrono, doc, crucible | indirect | SH5 |

Once the compiler is a Vesper program (SH6), it links each `@vesper/<crate>`
package directly, because the earlier-stage compiler builds both. The seed
copies then have no user left. Each one is deleted only when its port is at
parity, under the standing rule.

## Stays in Rust behind `extern`

In the compiler's closure today, and not ported by this plan: `vesper-ffi`,
`vesper-pulse` (drivers), `vesper-fiber-abi`, `vesper-chrono`, `vesper-simd`,
`vesper-vault`, `vesper-loom-cas`, `vesper-loom-trust`, `vesper-loom-admission`,
`vesper-loom-sandbox`, `vesper-migrate` (lang-c/syn lifter), `vesper-bench-pmu`,
`vesper-codegen-verified`, and Cranelift itself. The survey
([vesper-port-survey.md](vesper-port-survey.md), "Stays in Rust behind
`extern`") gives the reasons. The compiler reaches each through `.vsys`
imports with linear handles (#1077). A thin C-ABI shim is needed where the
crate has only a Rust API (Cranelift in SH0-4, BLAKE3 in SH0-5).

## Gaps that block the compiler

Known from the library ports and the compiler's own source. Lane SH0-8
measures them on compiler code before SH1 and SH2 are claimed.

| # | Gap | Blocks | Status |
|---|---|---|---|
| S1 | **Module swap-in build.** A second compiler build where a module is a Vesper object, built by the default `vesper` binary, with no Cargo cycle | every later wave | SH0-1 |
| S2 | **std I/O.** Files, directories, processes, args, env | the driver, packages, linker calls, LSP | SH0-3 |
| S3 | **Cranelift from Vesper.** A C-ABI shim over the builder, module and object APIs | SH4 (38k lines) | SH0-4 |
| S4 | **Compiler data.** Graphs (SCC, dominators, toposort), interning, typed index arenas, BLAKE3 | checker, PCG, IR, packages | SH0-5 |
| S5 | **Real CST.** A lossless front end over `Str` into sequences, matching `vesper-syntax` | SH1, fmt, LSP | SH0-7 |
| S6 | **Compile speed.** Library ports run 0.3x to 5x of Rust. A compiler that is 3x slower fails the parity rule. Allocation per AST node, map lookups and string building are the likely hot spots | every wave's parity bar | measured per lane; scoreboard in SH0-8 |
| S7 | **E-graphs.** `optimize` uses egg and `ir_opt` uses egglog | SH3 | `@vesper/egraph` in Vesper, SH3-8 (#1247); the extern alternative is flagged for the owner if egg's extraction cannot be matched |
| S8 | **Incremental queries.** `database/` and the LSP use salsa | SH5 | memo tables in Vesper; the batch compiler does not need them |
| S9 | **Wasm encoding.** `wasm_backend` uses wasm-encoder and wit-component | SH4 | the encoder ports; WIT component wrapping may stay extern |
| S10 | **Verified backend breadth.** §10.1 builds `@vesper-lang/compiler` with the verified backend, which still refuses closures, strings, sequences, heap memory, resuming handlers and macOS/Windows | the DDC part of Stage 2 | separate track (SH6b); does not block a Cranelift-built Stage 2 |
| S11 | **Ergonomics found by ports.** Two-phase arguments (#1094), qualified paths (#1093), `continue` (#1092), fixed arrays (#1091) | all compiler code | merged (P3b gaps wave) |
| S12 | **Leaks on early exits.** `Fail::raise`, field returns and early returns leak a local struct's owned fields | every compiler module | SH1-2 (#1145) |
| S13 | **Byte loops.** Per-byte `try_get` keeps its bounds check (span lookup, the v2 lexer at 7.1x Ir); slice headers reloaded; guards do not reach loop bodies | front end speed (S6) | SH1-3 (#1146) |
| S14 | **String and binding costs.** Byte-by-byte `push_str`, UTF-8 re-validation and a `Vec` per aggregate result in bindings | every swap | SH1-4 (#1147) |
| S15 | **Small language gaps.** Or-patterns, `Str` literals, bound literal views, `cmp_bytes`, `checked_add_k`, fmt DepthLimit | all compiler code | SH1-5 (#1148) |
| S16 | **Compiler data across the ABI.** `World` and `ast::Type` cannot cross export ABI v3; `in` `Box` trees cannot be matched | every coarse swap (SH1-12 onward) | SH1-6 (#1149) |
| S17 | **`.vsys` in packages.** Imports only from the root module of a library; no `bool`, no `CStr` | the driver (SH5), Cranelift users (SH4) | SH1-7 (#1150) |
| S18 | **Compile time of large functions.** Superlinear `--no-opt` JIT, shared values rematerialized | compile-speed parity (S6) | SH1-8 (#1151) |
| S19 | **Checker and IR shapes.** Measured by the census: a struct that owns a sequence cannot be read out of a sequence (E1522) and std collections take only copy values; no `i128`/`u128`; `eff` cannot be a name; the checker's data (types, names, `World`, diagnostics) and a corpus harness exist only as local samples. The census samples keep a hash-consed type arena and interned names, which is why they beat Rust; SH2 keeps that design | SH2 | SH2-1 (#1173), SH2-2 (#1174), SH2-3 (#1175), SH2-4 (#1176) |
| S20 | **Gaps SH2's ports worked around.** Constants not importable (every port); disjoint element and field borrows; closure parameter modes; `for` over sequences and bound string views; `effect`/`handle`/pass-mode words as names, literals past `u64::MAX`, fmt on long chains; callee result ranges as facts; the native instance cap and three-word entries; the AST and IR crossing the ABI as hand-encoded words | SH3 ports, and SH2's speed (decode is 27-42% of some ports' time) | #1239 (constants); SH3-2 to SH3-7 (#1241-#1246); ABI v4 input (SH2-4 follow-up) and SH3-1 (#1240) for the IR |
| S21 | **Gaps SH3 found for the backends.** Native code reads 128-bit array and tuple elements wrong (#1270); 128-bit division is slow and export cells take no 128-bit values; lent callbacks keep parameters in stack slots (#1271); `ir_opt::optimize` (egglog) is 72% of a combined checker build; each swap re-encodes the `World` and the IR (encode 0.80x of `ir::lower_world`) | SH4's ports and the compiler's build time | SH4-3 (#1277), SH4-4 (#1278), SH4-5 (#1279, owner chose a native-path policy), SH4-2 (#1276) |
| S22 | **Gaps SH4 found.** Family hooks cannot reach the walk's maps or re-enter Rust; shim lacks `sext`/`uext`, function bytes, declaration lookup and function read-back; packages cannot import `.vsys`, extern types not nameable in `use`; parser: `while k < n {`, `&& if`/`&& match`, `copy`/`affine` as names, keyword cascades; guarded shifts and division still need handlers, no integer `!`; `Err(e)?` in the JIT, E0404, `ChunkSeq::last`, no 128-bit in check-data/check-layout; debug lowering stack depth; optimizer 27 min in a debug compiler; `World::mode` caps the build at 1.72x | SH4's remaining parts and the build-time target | SH5-1 to SH5-10 (#1324-#1333) |

## Waves

| Wave | What | Size (Rust lines replaced) | Starts |
|---|---|--:|---|
| **SH0** | Groundwork: hybrid build, tool split, std I/O, Cranelift shim, std data, toolchain pilots, front end v2, census | ~3k (pilots) | merged except #1128 part 2 and #1130, which moved into SH1 ([plan](../agent-prompts/self-host-sh0/README.md)) |
| **SH1** | Front end: `vesper-diagnostics`, the `vesper-core` AST, front end v2 speed and editor mode, `vesper-fmt`, lexical `vesper-highlight`, `vesper-lint`, `vesper-forge` + `forge_frontend` (the wasm macro host stays extern), the parser swap into the hybrid build; seven gap lanes for S12-S18; the census that plans SH2 | ~36k | now ([plan](../agent-prompts/self-host-sh1/README.md), #1128, #1145-#1157, #1130) |
| **SH2** | Checker: four gap lanes (element borrows and owning collections, `i128`, contextual `eff`, the checker data package and corpus harness); the layout and IR-check swaps the census finished; name resolution and packages, core typing and modes, effects, regions and sequences, loops and atomics, `dyn`, `fip`, Pact trait resolution, tensor types; scoreboard and SH3 plan | ~36k | gap lanes now; ports after SH2-4 and SH1's AST lane (9); swaps after SH1's parser swap (12) ([plan](../agent-prompts/self-host-sh2/README.md), #1173-#1187) |
| **SH3** | The Place IR data package, harness and native-path swap point; `@vesper/egraph`; six gap lanes (S20); ports of `ir_opt`, the AST optimizer and its certificates, the inliner, bounds hoisting and Place IR lowering (the IR check moved to SH2 lane 5); SH2 and SH3 scoreboard and SH4 plan | ~9.8k | started 2026-11-20 on top of open SH2 PRs ([plan](../agent-prompts/self-host-sh3/README.md), #1240-#1253) |
| **SH4** | Backends: a codegen swap point with lent Cranelift handles and a CLIF/object differential, encode-once swap inputs, three gap lanes (S21), nine native lowering families (scalars, calls and ABI, aggregates, sequences, control, numeric, export ABI, FFI lowering, freestanding and Windows), wasm, eBPF, checks on emitted code; SH4 scoreboard and SH5 plan | ~49k | started 2026-11-21 on top of open SH3 PRs ([plan](../agent-prompts/self-host-sh4/README.md), #1275-#1292) |
| **SH5** | Backend infrastructure SH4 could not finish in its own packages (lent walk state and re-entry, shim extras); six gap lanes (S22); build time and native code (callback follow-ups, `World::mode`, optimizer build time, shared swap world); WIT component wrapping (owner: measure first); driver and tools: the driver, CLI commands, test runner and doctests, docs, LSP core and features, Lean extraction, sim-session; SH4 and SH5 scoreboard and SH6 plan | ~50k | planned 2026-11-21 stacked on open SH4 PRs ([plan](../agent-prompts/self-host-sh5/README.md), #1324-#1343); code lanes start after the CI-trim PR #1322 |
| **SH6** | Stage 2: the Vesper compiler built by the Rust compiler (Stage 1) rebuilds itself; stage 2 and stage 3 are bit-identical; `bootstrap/stage2.lock` and a CI gate; compile-time parity scoreboard against the Rust compiler; seed copies lose their users. SH6b: widen the verified backend to the compiler for the DDC check of §10.2 | none new | after SH5's driver lane |
| **SH7** | Vesper-native backend (register allocation, instruction selection and encoding for x86-64, AArch64, RV64), growing out of the verified emitter's design. Replaces Cranelift only at parity | new code | after SH6 (owner decision 2026-11-19) |

**Order inside the compiler.** Each wave swaps modules into the hybrid build
(SH0-1) one at a time, front to back. A swapped module runs differentially
against the Rust module over the whole test suite and the bootstrap corpus.
The Rust module stays the default until the Vesper one matches it in
function and compile speed. This repeats the library port pattern
(non-default `vesper-port` builds) for the compiler.

**What "done" looks like for each wave.** Every module of the wave is
swapped into the hybrid build. The hybrid compiler passes the full test
suite on all four required platforms. Its compile-time scoreboard row is
recorded. Deleting the Rust module is a separate owner call per module, made
only at parity.

**SH1 compile-time scoreboard.** Each row is one swap into the hybrid build,
measured against the default (Rust) compiler on the hybrid differential
corpus: `vesper check` on 537 files under `bootstrap/corpus`, `examples/` and
`tests/`, port-compiler profile, Linux x86-64.

| Swap | Lane | Whole corpus wall | Whole corpus Ir | Swapped module alone | Report |
|---|---|--:|--:|---|---|
| `parse` (front end v2 + AST, read back into `Parse`) | SH1-12 (#1155) | 1.03x | 1.06x | 1.00x wall, 1.19x Ir over 911 sources | [self-host-parser-swap.md](../parallel-results/self-host-parser-swap.md) |
| `effects` (capability rules, `capability_effects::check_world`; differential only, swap pending) | SH2-8 (#1180) | not swapped yet | not swapped yet | 0.28x wall (0.49x with input encoding) over 680 corpus, fixture and mutant worlds | [self-host-check-effects.md](../parallel-results/self-host-check-effects.md) |
| `dyn` (`dyn_fn` + `dyn_trait`; differential only, swap is a follow-up) | SH2-11 (#1183) | not swapped | not swapped | 0.28x wall over 930 sources forced, 0.70x on the dyn suites | [self-host-check-dyn.md](../parallel-results/self-host-check-dyn.md) |
| `names_packages` (package assembler: imports, re-exports, renaming; not swapped yet) | SH2-6 (#1178) | not swapped | not swapped | 0.72x-0.87x wall and 1.54x Ir of `packages::load` at first (two parses per module until SH2-1); 1.62x Ir with value enums and constants (#1310) | [self-host-check-names-packages.md](../parallel-results/self-host-check-names-packages.md) |
| `fip` checks (`fip::check_world`; not swapped yet, part 1 of the lane) | SH2-12 (#1184) | not swapped | not swapped | 0.92x wall with decoding (0.56x checks only) over 315 worlds; encoding the input costs 2.5x of Rust's check | [self-host-check-fip.md](../parallel-results/self-host-check-fip.md) |
| tensor types (`infer`, `dim`, `types`; not `elaborate`; not swapped yet, part 1 of the lane) | SH2-14 (#1186) | not swapped | not swapped | 0.28x wall on 40 rejected fixtures, 0.49x on 852 corpus sources (parse + check) | [self-host-check-tensor-types.md](../parallel-results/self-host-check-tensor-types.md) |

**SH2 compile-time scoreboard.** The same corpus and profile, with two
commands: `vesper check --json`, and `vesper emit --emit object`, which
asks for many more layouts. Each ratio is against the default (Rust)
compiler, and the hybrid includes the swaps above it.

| Swap | Lane | `check` wall | `check` Ir | `emit object` wall | `emit object` Ir | Swapped module alone | Report |
|---|---|--:|--:|--:|--:|---|---|
| `Layout::of` (`@vesper/check-layout`) and `ir_check::validate` (`@vesper/ir-check`), both in one build | SH2-5 (#1177) | 1.08x | 1.070x | 1.14x | 1.312x | layout 5.43x per call (one call per type, world rebuilt each call); IR check 0.53x per call, boundary included | [self-host-layout-ir-check-swap.md](../parallel-results/self-host-layout-ir-check-swap.md) |

**SH2 checker ports.** Each row is one checker port. Until its swap into
the hybrid build lands, the numbers are the ported rules alone against the
Rust rules they replace, over lane 4's corpus (ABBA, release build, Vesper
time divided by Rust time, input decode subtracted).

| Port | Lane | Ported so far | Differential | Rules alone | Hybrid swap | Report |
|---|---|---|---|--:|---|---|
| regions, sequences, strings, arrays, tiled, vectors | SH2-9 (#1181) | type and world rules (part 1) | 0 of 282 worlds; E0200, E0792, E0797, E1404, E1501-E1503, E1520 on mutants | 1.12x types, 1.17x world | with the expression rules | [self-host-check-regions-seqs.md](../parallel-results/self-host-check-regions-seqs.md) |
| loops (control rules, `?` on `Option`), atomics, stack bound | SH2-10 (#1182) | control frames (E0213, E0200), atomics (E0820-E0824, preludes), stack bound | 0 of 903 control worlds, 535 atomics worlds, 4,000 call graphs | atomics 2.31x, stack bound 2.54x (decode included); control pass 80 ms over the corpus | stack bound swapped (`stack_bound_report`): 0 of 26 `--verify-stack` and `ebpf check` commands differ; control and atomics not yet | [self-host-check-loops-atomics.md](../parallel-results/self-host-check-loops-atomics.md) |

**SH3 IR ports.** Each row is one IR pass port, measured like the SH2
checker ports until its hybrid swap lands (the pass alone against the Rust
pass, ABBA, port-compiler profile, encode and decode listed separately).

| Port | Lane | Ported so far | Differential | Pass alone | Hybrid swap | Report |
|---|---|---|---|--:|---|---|
| `ir_opt` (memory forwarding, `@fip` reuse) | SH3-9 (#1248) | the whole pass; thread + validate stay Rust | 0 of 666 inputs (corpus, mutants, fip cases), both modes | 0.0036x (0.0087x with encode and decode) | next (SH3-1 part 2 merged) | [self-host-ir-opt.md](../parallel-results/self-host-ir-opt.md) |

**SH4 backend ports.** Each row is one backend port. Until its swap into the
hybrid build lands, the numbers are the port alone against the Rust code it
replaces (ABBA, port-compiler profile, Vesper time divided by Rust time).

| Port | Lane | Ported so far | Differential | Port alone | Hybrid swap | Report |
|---|---|---|---|--:|---|---|
| eBPF backend (`ebpf_backend` surface + lowering + assembler; stack bound with lane 17) | SH4-16 (#1290) | part 1: surface, lowering, assembler | 0 of 243 cases (137 accepted, ELF objects equal) and 0 of 292 corpus worlds | 3.61x backend alone; 0.93x with the verifier model, 1.10x with encoding | after SH4-1 | [self-host-ebpf-backend.md](../parallel-results/self-host-ebpf-backend.md) |

## Estimates

These are rough. They come from the port waves so far, where one lane ported
about 2k to 6k lines of Rust in a day, including the gap reports.

- SH0: 8 lanes, about one day with lanes in parallel.
- SH1 to SH5: about 180k lines, or 40 to 60 lanes. At the P1 to P3 pace of
  about 8 to 12 parallel lanes a wave, that is five to seven waves of ports,
  plus the compiler-gap waves that each port wave has needed so far (one per
  port wave).
- SH6 starts when SH5's driver lane lands. Parity of compile speed is the
  long pole: no port at parity got there without at least one gap wave.

## Not in this plan

- Deleting any Rust module or seed copy. Each is an owner call at parity.
- Porting the stays-Rust crates.
- The test oracles in `src/bin/*42`: they are diverse oracles, like
  `vesper-codegen-verified`, and keep their value by staying Rust.
