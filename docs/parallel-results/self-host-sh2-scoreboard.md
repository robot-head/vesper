# SH2 scoreboard and the SH3 plan

> Self-hosting wave SH2, lane 15 (#1187). Written 2026-11-20 against `main`
> at `736d756` plus the open SH2 pull requests. The
> [SH3 plan](../agent-prompts/self-host-sh3/README.md) comes from this report.

## What changed in this lane's scope

The lane's prompt says to start when SH2 lanes 5-14 have each merged a first
swap, build one hybrid compiler with every swap, and re-measure. **The owner
asked to start SH3 now, on top of the SH2 work still in open pull requests**
(2026-11-20, 22:27 UTC). On that day one swap was in review (#1232, layout
and the IR check) and none had merged besides SH1's parse swap. So:

- **This scoreboard is assembled from the lanes' own reports**, on `main`
  and on their open branches. Nothing below was re-measured in one hybrid
  build, and the numbers come from different harnesses (see each row).
- **The single-build re-measurement moves to SH3 lane 14** (#1253), which
  runs once the SH2 swaps and the SH3 swaps have merged, and also plans SH4.
- **No local SH3 sample was translated.** The SH2 reports already list the
  gaps SH3's code will meet (they are the gap lanes below), and SH3 lanes 11
  and 12 start with the read-only halves of the inliner and bounds hoisting,
  which serve the same purpose: each lane commits its repros before its fix.
- This PR changes only documentation, so the Stage 1 guard holds trivially:
  no file under `src/`, `lean/`, `bootstrap/` or `packages/` is touched.

## Scoreboard (function and speed per module)

"Differences" is the lane's differential against the Rust module over the
corpus, the checker tests and mutants (every diagnostic's code, span and
text). Speed is the module's ABBA wall clock against Rust's, release build,
Linux x86-64, unless the row says otherwise. Wall clock on these hosts moves
by up to 30% between runs; the instruction counts are steadier.

| Lane | Module | PR | State | Differences | Speed vs Rust | Hybrid swap |
|---|---|---|---|---|---|---|
| SH2-4 | `@vesper/check-data` (types, names, `World`, diagnostics) and harness | #1217 | merged | 0 on 281 worlds | not timed (correctness tasks) | n/a (shared package) |
| SH2-5 | layout and the IR check | #1232 | open | 0: every corpus type; IR check 1,710 cases, 322,908 diagnostics | per call with the boundary: layout 5.43x (world rebuilt each call), IR check 0.53x; hybrid corpus `check` 1.08x wall / 1.070x Ir, `emit object` 1.14x / 1.312x ([report](self-host-layout-ir-check-swap.md)) | **swapped** (non-default) |
| SH2-6 | package assembler (names, packages) | #1227 | open | 0 of 80 | whole 0.72-0.87x; assembler alone 0.37-0.44x; **1.54x Ir** (two parses per module, gap N1) | follow-up (`AssembleTable`) |
| SH2-7 | core typing: relations, exhaustiveness (part 1) | #1222 | merged | 0 on 282 worlds | relations 1.18x, patterns 1.21x (with decode) | needs the checker spine (part 3+) |
| SH2-7 | core typing: expression typing (part 2) | #1237 | open | 0 on its cases | no fair Rust subset; timed at the swap | same |
| SH2-8 | capability effects (part 1) | #1238 | open | 0 on 680 worlds | **0.28x** checking; 0.49x with encoding | follow-up |
| SH2-9 | region, sequence, tiled, vector type rules (part 1) | #1226 | open | 0 on 282 worlds | types 1.12x, world 1.17x (rules alone) | with the expression rules |
| SH2-10 | loops, `?` on `Option`, atomics, stack bound | #1223 | open | 0 (894 control, 532 atomics worlds, 4,000 call graphs) | atomics 2.31x, stack bound 2.54x (mostly decoding) | follow-up (`src/stack_bound.rs`) |
| SH2-11 | `dyn_fn`, `dyn_trait` | #1231 | open | 0 on every case | 0.70x on the dyn suites, 0.28x on the repository's sources | follow-up |
| SH2-12 | fip checks (part 1) | #1228 | open | 0 on 1,260 worlds | 0.92x with decode, 0.56x checks alone; 1.42x Ir with decode, ~1.0x checks alone | part 2 |
| SH2-13 | Pact declaration pass (part 1) | #1225 | open | 0 on 29 worlds | not comparable yet (Rust side includes `World::collect`) | part 2 |
| SH2-14 | tensor types checker (part 1) | #1230 | open | 0 (57 fixtures, 1,880 mutants, 852 sources) | 0.28x fixtures, 0.49x corpus | part 2 (with elaboration) |

Gap lanes: SH2-1 element views and owning collections (#1221, open; the
state pool 1.62x of Rust's `Vec<State>`, 1.45x Ir, against 1.58x for the
flat pool it replaces), SH2-2 `i128`/`u128` (#1219, merged), SH2-3
contextual `eff` (#1218, merged). Outside the wave, value enums and
importable constants (#1239, open) fix the gap every SH2 report hit.

**Reading the scoreboard.** Function: every port that has a differential
matches Rust exactly. Speed: the ports that keep the census data design run
at 0.28x to 1.2x of Rust; the slow rows are decode-bound (loops and
atomics, the regions world task) or parse twice (names), and both causes are
known (ABI v4 input; element views). No module is at parity in both function
and speed as measured in the hybrid build yet, because only one swap exists,
so **Rust stays the default for every module** under the standing rule.

## Compiler gaps

Collected from the SH2 reports, with the lane that fixes each. Minimal
repros are in the reports named; each SH3 gap lane commits them as failing
tests first.

| Gap | Seen in | Repro | Fix |
|---|---|---|---|
| A `const` cannot be imported from another module (E0111) | every SH2 port | `pub const A: u8 = 1;` in `a.ves`, `use self::a::{A};` | #1239 (open, outside the wave) |
| Field paths through an element; two views of one sequence; copy elements lent `inout`; a raising element view with a live linear value | SH2-1 G1-G4 | `xs[0].n = 1`; `swap(inout xs[0], inout xs[1])` | SH3-2 (#1241) |
| A method on a struct cannot also lend one of its fields | SH2-14 gap 4 | `put_name(in st, inout st.err, x)` is E0301 | SH3-2 (#1241) |
| Functions holding non-copy element views are not inlined | SH2-1 G9 | a matched `try_get` of a `Box` element | SH3-2 (#1241) |
| Closures have no parameter modes; `IdxMap` of non-copy values cannot fill gaps | SH2-1 G5, G6 | `with_ref(k, \|in v\| ..)` | SH3-3 (#1242) |
| `for` iterates only integer ranges (E0213) | SH2-10 gap 1 | `for s in ["a", "b"] { .. }` | SH3-4 (#1243) |
| A `Str` view cannot be bound (E1531); no view of part of a name | SH2-10 gap 3, SH2-9 gap 3 | `let v = s.as_bytes(); v.len()` | SH3-4 (#1243) |
| `effect`, `handle` and pass-mode words cannot be names (E0002) | SH2-3 gaps 1-2, SH2-6, SH2-8 gap 1 | `fn effect() {}`, `struct S { sink: u64 }` | SH3-5 (#1244) |
| Integer literals stop at `u64::MAX` | SH2-2 gap 1 | `fn f() -> u128 { 18446744073709551616 }` | SH3-5 (#1244) |
| `vesper fmt` stops at DepthLimit on a long `\|\|` chain | SH2-8 gap 3 | 24 `f(x)` terms joined by `\|\|` | SH3-5 (#1244) |
| A callee's result range is not a fact (E0401 on a divisor) | SH2-9 gap 1 | `128 / bits(x)` | SH3-6 (#1245) |
| `u128` facts stop at `i128::MAX`; widened multiplies keep the full check | SH2-2 gaps 2, 4 | `let a: u128 = 18446744073709551615; a * a` | SH3-6 (#1245) |
| Native monomorphization cap (1,024, then 4,096) | SH2-11 gap 1, SH2-14 gap 1 | 1,025 instances reachable from `main` | SH3-7 (#1246) |
| Word entries take at most three words | SH2-11 gap 4 | a four-parameter entry | SH3-7 (#1246) |
| The AST and IR cross the ABI only as hand-encoded words; encoding is 27-42% of a port's time | SH2-8 gap 4, SH2-12 gap 4, SH2-10 | `encode_task` | ABI v4 `CheckInput` (SH2-4 follow-up); SH3-1 (#1240) for the IR |
| `dyn_fn` span base depends on `serde_json` text length | SH2-11 gap 5 | the caller passes the base | owner call (changes Rust's spans); not in SH3 |
| 128-bit division is a bit loop; no 128-bit export cells | SH2-2 gaps 3, 5 | `u128 / u128` | SH4 (export ABI, native lowering) |

## The SH3 plan

[docs/agent-prompts/self-host-sh3/README.md](../agent-prompts/self-host-sh3/README.md),
claims #1240-#1253 (lane n is issue 1239+n): two shared pieces (the Place IR
data package with its harness and swap point; an e-graph), six gap lanes from
the table above, five IR pipeline ports (`ir_opt`, the AST optimizer and its
certificates, the inliner, bounds hoisting, Place IR lowering, about 9,800
lines of Rust), and the re-measurement and SH4 plan last. Lanes 2, 3, 5, 7,
11 and 12 stack on open SH2 branches (#1221, #1239, #1231, #1232).

## What is left

- The single-build re-measurement of every SH2 swap, with callgrind per
  module and for the whole checker: SH3 lane 14.
- Each SH2 lane's hybrid swap and later parts, which stay with that lane.
