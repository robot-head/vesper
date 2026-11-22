# Self-hosting wave SH3: the IR pipeline, and the gaps SH2 found

> Plan written 2026-11-20 against `main` at `736d756` plus the open SH2 pull
> requests (claim #1187, SH2 lane 15). The
> [manuscript](../../manuscript/README.md) is the sole specification. The
> crate-by-crate inventory and later waves are in
> [self-hosting-plan.md](../../maintenance/self-hosting-plan.md). The SH2
> numbers this plan comes from are in the
> [SH2 scoreboard](../../parallel-results/self-host-sh2-scoreboard.md).
> Confirm live ownership on the issue board before acting.

SH2 ported the checker module by module. Every port that has a differential
matches Rust exactly on the corpus and its mutants, and the ports that keep
the census data design (`u32` type ids in a hash-consed arena, interned
names, flat tables with sorted rows) run at 0.28x to 1.2x of Rust's time.
Most of what is left of SH2 is hybrid swaps, which each SH2 lane finishes in
its own follow-up PRs.

SH3 ports the rest of the compiler's middle: Place IR lowering
(`ir::lower_world`), memory forwarding (`ir_opt`), the AST arithmetic
optimizer with its certificate checker (`optimize`), the inliner
(`ir_inline`) and bounds hoisting (`bounds_hoist`), about 9,800 lines of
Rust. It also fixes the gaps SH2's ports kept working around, and builds the
two shared pieces the IR ports need: a Place IR data package that crosses the
ABI both ways, and an e-graph.

**The owner asked to start SH3 on top of SH2 work that is still in open pull
requests** (2026-11-20). So several lanes stack on an open SH2 branch. The
"Stacking" rule below says how.

Every port here keeps Rust as the default build under the standing owner rule.

## Lanes

Claims: lanes 1-14 are #1240-#1253 (lane n is issue 1239+n).

### Shared pieces

| Lane | Prompt | Claim | What | Starts | Stacks on | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [Place IR data package, pass harness and IR swap point](01-place-ir-data.md) | #1240 | `@vesper/place-ir`: `ir::Program` as flat tables, words in both directions, a round-trip and pass differential harness, and one native-path swap point for the IR passes | now | #1232 (IR encoder, `@vesper/ir-check` decoder) | new `packages/vesper-place-ir/**`, new `tests/common/ir_words.rs`, `tests/self_host_place_ir*.rs`, new `src/ir_swap.rs` and its call-site hunks in `src/native.rs`, `src/native_export.rs`, `src/native_session.rs`, `src/ffi_lowering/drops.rs` (native path only), shard 7 entry, report |
| 8 | [`@vesper/egraph`](08-egraph.md) | #1247 | hash-consed e-nodes, union-find, rebuild, e-matching of the rule shapes both optimizers use, cost extraction (gap S7) | now | none | new `packages/vesper-egraph/**`, `tests/self_host_egraph*.rs`, report |

### Compiler gaps found by SH2

| Lane | Prompt | Claim | Gap | Found by | Starts | Stacks on | Exclusive boundary |
|---|---|---|---|---|---|---|---|
| 2 | [Disjoint borrows](02-disjoint-borrows.md) | #1241 | field paths through elements, two views of one sequence, copy elements lent `inout`, a field of a lent receiver, inlining non-copy views | SH2-1 G1-G4, G9; SH2-14 gap 4 | now | #1221 | element-path and disjointness hunks of `src/seq/{check,ir}.rs`, the two-phase/borrow hunks of `src/check.rs`, the non-copy-view refusal hunk of `src/ir_inline.rs`, manuscript hunks (flagged), `tests/disjoint_borrows*.rs`, report |
| 3 | [Closure parameter modes](03-closure-modes.md) | #1242 | `\|in v\| ..` closures; `with_ref`/`with_mut` on collections; `IdxMap` that fills gaps | SH2-1 G5, G6 | now | #1221 | closure-parameter hunks of the parser (with the `packages/vesper-front` and `packages/vesper-ast` mirror), checker and lowering, `packages/vesper-std/collections/**` (view callbacks), manuscript hunks (flagged), `tests/closure_modes*.rs`, report |
| 4 | [`for` over arrays and sequences; string views](04-iteration-string-views.md) | #1243 | `for s in [..]` is E0213; `as_bytes()` views cannot be bound (E1531); no `rsplit`/`strip_prefix` views of a name | SH2-9 gap 3, SH2-10 gaps 1, 3 | now | none | `for`-iteration hunks of `src/loops/**` and its lowering, string-view hunks of `src/strings/**`, the view helpers in `packages/vesper-std/bytes/**`, manuscript hunks (flagged), `tests/for_sequences*.rs`, `tests/string_views*.rs`, report |
| 5 | [Names and literals](05-names-and-literals.md) | #1244 | `effect`, `handle` and pass-mode words as names; literals past `u64::MAX`; `vesper fmt` DepthLimit on long chains | SH2-2 gap 1, SH2-3 gaps 1-2, SH2-6, SH2-8 gaps 1, 3 | now | #1239 | keyword and literal hunks of `crates/vesper-syntax/src/{lexer,parser}.rs` with their `packages/vesper-front` and `packages/vesper-ast` mirror, the chain hunk of `crates/vesper-fmt`, manuscript hunks (flagged), `tests/contextual_names*.rs`, `tests/wide_literals*.rs`, report |
| 6 | [Facts across calls](06-facts-across-calls.md) | #1245 | a callee's result range is not a fact (`128 / bits(x)` is E0401); `u128` facts stop at `i128::MAX`; widened multiplies keep the full check | SH2-9 gap 1, SH2-2 gaps 2, 4 | now | none | result-range hunks of the bounds facts in `src/check.rs` (#1088), `u128` hunks of `src/integer_widths/**` facts, the widened-multiply hunk of native lowering, `tests/call_facts*.rs`, report |
| 7 | [Compiler capacity](07-compiler-capacity.md) | #1246 | the native instance cap (1,024, 4,096 since #1231); JIT compile time of the combined checker packages; entries that take more than three words | SH2-11 gaps 1, 4; SH2-14 gap 1 | now | #1231 | `MAX_NATIVE_INSTANCES` and its users in `src/native.rs`/`src/native_export.rs`, the `WordFunction` entry hunks of `src/native.rs` and the test-entry hunks of `src/test_runner.rs`, `tests/compiler_capacity*.rs`, report |

### IR pipeline ports

| Lane | Prompt | Claim | Rust replaced (lines) | Starts | Stacks on | Exclusive boundary |
|---|---|---|--:|---|---|---|
| 9 | [`ir_opt` memory forwarding](09-ir-opt.md) | #1248 | 640 (`src/ir_opt.rs`) | after lane 1's and lane 8's first PRs | lanes 1 and 8 | new `packages/vesper-ir-opt/**`, hybrid module `ir_opt`, `tests/self_host_ir_opt*.rs`, one scoreboard row, report |
| 10 | [AST arithmetic optimizer and certificates](10-optimize.md) | #1249 | 2,600 (`src/optimize.rs`, `optimize/{certificate,witness}.rs`) | now (certificate and witness); search after lane 8's first PR | lane 8 for the search | new `packages/vesper-optimize/**`, swap point hunk of `src/optimize.rs`, hybrid module `optimize`, `tests/self_host_optimize*.rs`, one scoreboard row, report |
| 11 | [The inliner](11-inliner.md) | #1250 | 1,930 (`src/ir_inline.rs`, `ir_inline/site.rs`) | now (sizes, costs, decisions); splicing after lane 1's first PR | #1232, then lane 1 | new `packages/vesper-ir-inline/**`, hybrid module `ir_inline`, `tests/self_host_ir_inline*.rs`, one scoreboard row, report |
| 12 | [Bounds-check hoisting](12-bounds-hoist.md) | #1251 | 3,190 (`src/bounds_hoist.rs`, `bounds_hoist/{facts,native}.rs`) | now (facts and which loops hoist); rewriting after lane 1's first PR | #1232, then lane 1 | new `packages/vesper-bounds-hoist/**`, hybrid module `bounds_hoist`, `tests/self_host_bounds_hoist*.rs`, one scoreboard row, report |
| 13 | [Place IR lowering](13-place-ir-lowering.md) | #1252 | 1,450 (`ir::lower_world`, `thread_tokens`) | after lane 1's first PR | lane 1 and #1237 (expression typing) | new `packages/vesper-place-ir-lower/**`, hybrid module `ir_lower`, `tests/self_host_ir_lower*.rs`, one scoreboard row, report |

### Planning

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 14 | [SH2 and SH3 swap scoreboard and SH4 plan](14-scoreboard-sh4-plan.md) | #1253 | last: when the SH2 swaps and lanes 9-13 have each merged a first swap | `docs/maintenance/self-hosting-plan.md` (SH3 results, SH4 sections, gap table), new `docs/agent-prompts/self-host-sh4/**`, `docs/parallel-results/self-host-sh3-scoreboard.md` |

**Start now:** lanes 1-8, 10, 11 and 12 (2, 3, 5, 7, 11 and 12 on the open
branch named in "Stacks on").
**After lane 1's first PR:** lane 13, and the rewriting halves of 11 and 12.
**After lanes 1 and 8 each land a first PR:** lane 9, and lane 10's search.
**Last:** lane 14.

**Recommended merge order.**
- Each stacked lane merges only after the PR it stacks on (see "Stacking").
- Lane 1 first among the IR ports: lanes 9-13 swap through its
  `src/ir_swap.rs` and compare through its harness.
- Lane 8 before lane 9 and before lane 10's search PR.
- Lanes 2 and 3 both stack on #1221 and both touch the checker: merge `main`
  between them. Lane 2's `src/ir_inline.rs` hunk and lane 11's swap point
  both edit that file: merge `main` between them.
- Lanes 3 and 5 both edit the parser and both front-end packages: merge
  `main` between them and re-run `tests/self_host_front_v2.rs` and
  `tests/self_host_ast.rs`.
- Lanes 9-13 each add a module to `crates/vesper-hybrid` and a row to the
  SH3 scoreboard in `docs/maintenance/self-hosting-plan.md`: merge `main`
  right before merging each, and serialize merges that touch root
  `Cargo.toml` (no merge queue on this account).
- Lane 14 last.

## Rules for every lane

Every rule in [SH2's "Rules for every lane"](../self-host-sh2/README.md#rules-for-every-lane)
applies (claim first, the Stage 1 guard, the standing owner rule, the census
data design, reproduce first, the parser mirror rule, semantics first, the
corpus differential, measuring, spec, build, merge, report), with these
changes and additions.

- **Stacking (owner request, 2026-11-20).** A lane whose "Stacks on" column
  names an open pull request branches from that PR's head, not from `main`.
  Record both SHAs. Open your PR against `main` and say in its first line
  which PR it stacks on, so its diff reads as "that PR plus this lane".
  Never merge before the PR you stack on. When it merges, merge `main` into
  your branch (do not rebase a pushed branch), re-run your tests, and only
  then merge. If the PR you stack on changes under you, merge its head again.
  If it is closed without merging, stop and report.
- **Stage 1 guard, for IR lanes.** `src/ir.rs` and `ir_opt::optimize` are
  never edited, and the verified backend's pipeline (`src/verified_codegen/**`,
  its call of `ir::lower_world` and `ir_opt::optimize`) does not go through
  any swap point. Lane 1's `src/ir_swap.rs` hooks only the native path
  (`native`, `native_export`, `native_session`, `ffi_lowering::drops`). The
  default compiler installs nothing, so its output is byte-identical.
- **IR differential.** IR ports compare the whole `Program` after their pass
  (lane 1's canonical dump and decoded words, `Debug`-equal), run lane 5's IR
  check on the result, and run every corpus program that executes through
  the JIT (optimize on and off) and an executable from the hybrid build,
  comparing outputs. A pass that is a search (lanes 9 and 10) also compares
  its counts (rewrites installed, refused) with Rust's.
- **Measure the pass, not the encoding.** Report each pass alone (decode
  subtracted) and with its encoding, ABBA and callgrind, as the SH2 reports
  did. ABI v4 input and lane 1's tables are how the encoding cost goes away.
- **Constants.** Once #1239 merges, use imported `const`s and value enums
  instead of repeating constants per module.
- **Element views.** Once #1221 merges, keep states in `ChunkSeq`s of
  structs instead of the census state-pool pattern where it reads better and
  measures no slower.

## Spec calls flagged for the owner

- **Disjoint element views (lane 2).** Which pairs of element and field
  borrows are disjoint is a borrow rule change.
- **Closure parameter modes (lane 3).** `|in v|`, `|inout v|` and how a
  closure with a borrowed parameter is typed.
- **`for` over sequences and kept string views (lane 4).** What `for x in s`
  binds (a copy, or a view for non-copy elements), and how long a bound
  `Str` view lives.
- **`effect`, `handle` and pass-mode words as names, wide literals (lane 5).**
  Removing reserved words; literals up to `u128::MAX`.
- **The e-graph (lane 8).** The [self-hosting plan](../../maintenance/self-hosting-plan.md)
  (row S7) prefers an e-graph in Vesper over keeping egg and egglog behind
  an extern. Lane 8 builds it. If it cannot reproduce egg's extraction
  choices exactly, lane 8 reports how the results differ and the owner
  decides whether equal-cost, certificate-checked results are acceptable.

## Not in this wave

- The SH2 hybrid swaps and later parts: each SH2 lane finishes its own.
  `fip`'s IR drop elaboration (SH2 lane 12 part 3) moves onto lane 1's
  `@vesper/place-ir` once it lands.
- `optimize/witness_lean.rs` (Lean extraction of witnesses, SH5).
- `ffi_lowering::drops` and native lowering (SH4).
- JSON-independent `dyn_fn` span bases (SH2 lane 11 gap 5): changes Rust's
  spans, so it is an owner call first.
- Faster 128-bit division (SH2 lane 2 gap 3) and 128-bit export cells (gap 5):
  SH4, with the export ABI.
- Deleting any Rust module or seed copy: an owner call per module at parity.
