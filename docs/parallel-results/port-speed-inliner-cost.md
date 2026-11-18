# Port speed lane 5: inliner cost after simplification, and partial inlining (#1025)

Claim #1025. Base `72209eb` (origin/main). Prompt:
[05-inliner-cost.md](../agent-prompts/port-speed/05-inliner-cost.md).
Everything is in `src/ir_inline.rs` and the new `src/ir_inline/site.rs`, on
the native optimize path. Nothing in `ir.rs`, `ir_opt::optimize`,
`verified_codegen`, `lean`, `bootstrap` or `packages/vesper-compiler` changed.

**Result.** Each call site now charges the callee what it costs at that site,
and a callee that is still over budget can be inlined partially: its cheap
part is copied and the whole callee is called on its expensive side. The
named Ignis `TexEntry` cmp is now inlined this way. BBR's instructions drop
1.5% (2.25x to 2.22x of Rust) and its object shrinks 4.6%. Ignis drops 0.7%
(1.43x to 1.42x) and its object grows 2.2%. Quarry and warp-wheel do not
change. Wall-clock differences are inside this machine's noise. No port
reaches parity, so Rust stays the default build of every port but Aura.

## What changed

1. **Cost at the site** (`site::fold`). A small sparse conditional constant
   propagation over the callee, seeded with the site's known by-value
   arguments: integer and `bool` constants, and nullary constructors of
   enums that own nothing (`Ordering`, `BbrState`). It folds compares, `!`,
   single-assignment locals and block parameters that every live
   predecessor passes the same value, then decides `Branch` terminators and
   `match`es whose taken arm binds no names. The callee is charged
   [`cost`] over the blocks still reachable, with the budget unchanged. The
   copy keeps that folding: decided terminators become jumps, and
   unreachable blocks are copied empty (`Unreachable`). A parameter the
   callee writes, re-binds or lends `inout` is never folded, and neither is
   a callee with a handler or closure.
2. **Partial inlining** (`site::split`). When the site cost is still over
   [`Budget::small_callee`], the inliner looks for a `Branch` or `match` in
   a *pure head*: blocks that only read, compare, project and bind values
   that own nothing, with no loop, call, write or fallible operation on any
   path from the entry. Cutting one successor edge leaves the *hot* blocks
   (still reachable) and the *cold* blocks (reachable only through the cut).
   The split is taken when the hot part plus the call is within budget and
   the cold part is at least 16 and at least as large as the hot part.
   Among the candidates it prefers a cold side with a loop or call and a hot
   side without, then the largest hot side. The copy keeps the hot blocks;
   the cut edge goes to one new block that repeats the original call
   (same arguments, same places) and jumps to the continuation. The head
   has no effects, so running it twice on the cold path is unobservable.
   Every argument bound by value must own nothing (or be a slice view),
   because the call passes it again. Handled callees are not split.
3. **Code growth.** [`Budget::callee_growth`] (4,000 units) bounds what the
   copies of one callee add to the program, which caps a derived `cmp` or
   `eq` inlined into every sort and lookup. On the four ports it never
   binds; the program and caller budgets bind first.
4. **Reporting.** `Report::partial` (partial sites per callee),
   `Report::folded` (sites whose copy lost a branch or block) and
   `Report::growth` (units added per callee). With `VESPER_INLINE_LOG=<path>`
   the compiler appends one line per site whose callee is over budget
   before folding (`full`, `partial` or `size`, with the costs). The tables
   below come from it.

The decision still depends only on the world and the IR, so
verifier-observed and instrument-hook builds stay byte-identical to plain
builds.

## The `Size` table

Every site, in the four ports, whose callee costs more than the budget (96)
before site folding: the callee's cost, its cost at the site, and what the
inliner does now (for a partial inline, the hot and cold cost). Sites that
stay calls for another reason are not listed. "Before" every one of them was
a `Size` refusal.

### BBR

| Callee (caller) | cost | at site | now |
|---|---:|---:|---|
| `inflight_with_gain` (`check_drain_done`, `is_time_to_cruise`) | 174 | 63 | **inlined** |
| `bdp_multiple` (`probe_rtt_cwnd_bound`, `update_min_rtt`) | 170 | 138 | partial, hot 8 / cold 132 |
| `bdp_multiple` (`inflight_with_gain`, `update_max_inflight`) | 170 | 170 | partial, hot 8 / cold 146 |
| `check_full_bw_reached` (2 sites) | 102 | 102 | partial, hot 13 / cold 90 |
| `inflight_with_headroom` (2 sites) | 103 | 103 | partial, hot 8 / cold 88 |
| `start_probe_bw_down` (5 sites) | 107 | 107 | call |
| `handle_inflight_too_high` (2) | 103 | 103 | call |
| `probe_rtt_cwnd_bound` (3) | 127 | 127 | call |
| `handle_lost_packet`, `update_congestion_signals`, `finish_ack`, `is_time_to_cruise` | 253-266 | same | call |
| `adapt_long_term_model` (2), `set_cwnd`, `update_model_and_state` | 368-1064 | same | call |

`mul_div_sat`, named by the prompt, already inlines on `72209eb`: its cost
after #1006 is under budget at every site.

### Ignis

| Callee (caller) | cost | at site | now |
|---|---:|---:|---|
| `queue_can_transition` (`handoff`, 2 sites) | 126 | 73 | **inlined** |
| `pairs_by` (`topological_order`, 1 of 2 sites) | 177 | 72 | **inlined** |
| `impl$Ord.TexEntry$cmp` (`less`, `sort_dedup`) | 235 | 235 | partial, hot 87 / cold 150 |
| `lower_barrier` (`lower_plan`) | 287 | 287 | partial, hot 93 / cold 118 |
| `push_pending` (`process`) | 198 | 198 | partial, hot 85 / cold 115 |
| `vk_access_bits_lossy` (4 sites) | 105 | 105 | partial, hot 6 / cold 101 |
| `mtl4_stage_bits` (2) | 98 | 98 | partial, hot 7 / cold 93 |
| `validate_use`, `translate` | 529-533 | same | partial, hot 8-14 |
| `push_pending` (`handoff`, `plan_final_layouts`) | 198 | 115 | call |
| `pairs_by` (other site) | 177 | 149 | call |
| `process` (2) | 1104 | 1006-1009 | call |
| `queue_can_transition` (3 other sites), `vk_stage_bits_lossy` (2), `vk_access_bits_for_sync` (2), `split_segments` (2), `needed_waits` (2), `sort_seq` and `sort_dedup` instances, `boxes`, `check_pairs`, `plan_buffer`, `handoff`, `plan_texture`, `topological_order`, `plan_final_layouts`, `hazard_edges`, `assemble` | 112-1619 | same | call |

### warp-wheel

| Callee (caller) | cost | at site | now |
|---|---:|---:|---|
| `LeaseCore::rearm` (`lease_core_rearm`) | 242 | 242 | partial, hot 8 / cold 236 |
| `WheelCore::unlink` (8 sites) | 113 | 113 | call |
| `LeaseCore::remove_at` (3) | 109 | 109 | call |
| `next_work_tick` (2), `place` (2), `wheel_core_new` (2), `open_tick` | 185-454 | same | call |

### Quarry

| Callee (caller) | cost | at site | now |
|---|---:|---:|---|
| `values::compare_vals` (`stats::analyze`) | 102 | 102 | partial, hot 8 / cold 96 |
| `parse::ident` (7), `values::charge_row` (4), `util::identifier` (3), `stats::analyze` (3), `stats::operand` (2), `Str::next_char` (2) | 103-289 | same | call |
| `lex`, `parse_ddl`, `prepare_q`, `execute_run`, `set_rows_run` and the other entry points | 245-1127 | same | call |

**Reading the table.** Constant arguments are rare in these ports: 6 sites
fit after folding. Most of the remaining `Size` callees are straight-line
procedures over `inout` state (`start_probe_bw_down`, `WheelCore::unlink`,
`remove_at`) with no branch a site decides and no cheap side to split off,
so they stay calls under an unchanged budget. Partial inlining reaches the
callees with an early exit: a derived `cmp` (the copy compares the first
fields and calls the whole comparison only when they are all equal), and
guards such as `if b.full_bw_reached { return; }` or
`if bw == infinity() { return ... }`.

## Repros

`vesper build --lib`, `objdump -d` of the exported function, `72209eb` ->
this branch.

| Repro | before | after |
|---|---|---|
| `step` calling `mul_div_sat` twice, with a 4x unrolled wide division (tests/native_inline_cost.rs) | 23 insns, 2 calls on every path | 153 insns; the common case (`a, b < 2^32`) makes no call, the wide case 1 call per site |
| `count_less` over a 12-field derived `cmp` (single call site) | 927 insns, inlined | 927 insns, inlined (unchanged: the single-site budget already covered it) |
| `pick(0, x)` with two heavy arms (folding; IR checked by the test, not objdump) | call | inlined; the copy holds no loop, only the `x + 1` arm |

## Ports

Setup as in [port-gaps-rescore.md](port-gaps-rescore.md): cloud x86-64,
4 vCPU Xeon, release `vesper` built at `72209eb` ("before") and at this
branch ("after"), passed to each port build as `VESPER_PORT_COMPILER`. No
port source changed.

Instructions (callgrind `Ir`, whole process):

| Port | workload | Rust | before | after | ratio before -> after |
|---|---|---:|---:|---:|---|
| BBR | `ack_path_abba measure 20000` | 21.97 M | 49.45 M | 48.69 M | 2.25x -> **2.22x** |
| Quarry | `planner_abba window 20` | 19.47 M | 31.09 M | 31.10 M | 1.60x -> 1.60x |
| Ignis | `abba measure 20` | 116.17 M | 165.65 M | 164.56 M | 1.43x -> **1.42x** |

Object `.text` size of each port's Vesper object:

| Port | before | after | change |
|---|---:|---:|---:|
| BBR | 79,566 | 75,926 | -4.6% |
| Quarry | 337,626 | 337,602 | 0.0% |
| Ignis | 363,362 | 371,218 | +2.2% |
| warp-wheel (file size) | 69,080 | 69,192 | +0.2% |

Wall clock: each port's committed ABBA driver, two runs per arm, in the
order before, after, before, after. Medians of port/Rust, with the 95%
interval of the median.

| Port, workload | before 1 | after 1 | before 2 | after 2 |
|---|---|---|---|---|
| BBR (16 x 200k) | (2.70-3.40) | (2.55-2.99) | (2.41-3.24) | (2.46-3.09) |
| Quarry (driver, median regression) | 1.95x | 1.70x | 1.87x | 2.02x |
| warp-wheel (30 epochs) | 1.77x (1.76-1.79) | 1.77x (1.75-1.79) | 1.77x (1.75-1.78) | 1.76x (1.73-1.77) |
| Ignis build 64 / 256 | 2.32x / 2.37x | 2.50x / 2.25x | 2.45x / 2.44x | 2.53x / 2.40x |
| Ignis plan 64 / 256 | 1.37x / 0.65x | 1.44x / 0.69x | 1.34x / 0.64x | 1.42x / 0.66x |
| Ignis lower 64 / 256 | 3.24x / 2.69x | 3.29x / 2.50x | 3.21x / 2.64x | 3.25x / 2.53x |

All wall-clock differences are inside this machine's run-to-run spread
(up to 30% for one binary). The deterministic results are the instruction
counts above.

Every port's test suite passes with the branch compiler
(`cargo test --release -p <port> --features vesper-port` for BBR, Ignis,
warp-wheel, Quarry, Prism and warp-tcp, and `-p vesper-aura`), including the
leak tests.

## Tests

`tests/native_inline_cost.rs`: constant arguments deciding `if` chains,
known tags deciding a `match`, a written parameter that must not fold,
partial inlining of a `mul_div_sat` shape, a 12-field derived `cmp`, and the
`callee_growth` cap. Each program runs through the evaluator (optimize on
and off), the JIT (optimize on and off) and x86-64 and AArch64 object
emission, and must agree. `tests/native_inline.rs` and
`tests/native_inline_reach.rs` still pass unchanged.

## Stage 1 guard

Stage 1 builds with `optimize` off, so the inliner never runs there, and no
Stage 1 input changed. The `bootstrap-stage1` workflow run on the branch is
linked from the PR.

## What is left

- **Straight-line procedures just over budget** (`start_probe_bw_down` 107,
  `WheelCore::unlink` 113, `remove_at` 109, `Str::next_char` 109). Neither
  folding nor splitting reaches them. They need a larger budget for
  callees with few sites, or a cost that counts what Cranelift folds
  (repeated field reads of one `inout` place), which this lane did not try
  because the budget was to stay unchanged.
- **Guard splits** (hot 6-14) copy an early-exit test and still call the
  callee in the common case. They cost little code and save a call only
  when the guard exits. With no profile the inliner cannot tell which side
  is hot.
- **Callees with handlers** are never split, and their handler graphs are
  charged in full at every site.
