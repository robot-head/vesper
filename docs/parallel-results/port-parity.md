# Port parity scoreboard

Lane 8 of the [port parity wave](../agent-prompts/port-parity/README.md)
(claim #956). Written 2026-11-18 against `main` at the merge of lanes 1-7.
Every number below is copied from a lane report, which gives the setup (all
on the same kind of host: a 4 vCPU Intel Xeon @ 2.10 GHz cloud container,
Linux x86-64, Rust 1.97 `--release`, Vesper `optimize = true`). This lane
measured nothing new.

Ratios are Vesper time / Rust time (lower is better; 1.0x is parity). "Main
before" is `main` at `cee8b1a`, after the Stage 1 compiler wave and before
any port change of this wave. The intervals are each lane's 95% interval of
the median (96% for Quarry).

## Results

| Port | Lane, PR | Benchmark | P1 | Main before | After this wave | Parity bar |
|---|---|---|---|---|---|---|
| warp-wheel | 1, #963 | `tests/abba.rs` | 8.00x | 4.98x (4.87-5.02) | **3.37x** (3.32-3.42) | not met |
| BBR | 2, #968 | `examples/ack_path_abba.rs` | 11-16x | 11.6x (10.3-11.8) | **5.3-5.5x** (two runs; 4.3-6.1) | not met |
| Prism | 3, #973 | `examples/abba.rs` (new): decode / route / encode / connection | 4.5x | 5.28x / 5.26x / 6.55x / 5.30x | **2.01x / 3.42x / 3.04x / 2.29x** | not met |
| warp-tcp | 4, #971 | `examples/abba.rs` codec / transfer | 10-14x | 7.63x / 5.36x (instructions) | codec **0.90x** instr., wall about 1.05x (0.73-1.50 across runs); transfer **2.59x** wall (2.47-2.66), 1.91x instr. | not met (transfer) |
| Aura | 5, #977 | `examples/abba.rs` one-text / reorder / encode-audit | 3.8-5.2x | 1.54x / 1.94x / 1.47x | **0.42x / 0.58x / 0.85x** (0.39-0.46 / 0.52-0.64 / 0.80-1.00) | **met** |
| Ignis | 6, #981 | `examples/abba.rs` (new): build / plan / lower at 64 / 256 passes | 5-13x | build 4.30x / 4.43x, plan 5.21x / 4.53x, lower 4.42x / 3.73x | build **3.74x / 3.93x**, plan **2.41x / 1.12x**, lower **4.19x / 3.51x** | not met |
| Quarry | 7, #976 | `examples/planner_abba.rs` | 4.3-4.9x | 2.87x wall, 2.54x instr. | **2.19x** wall (2.10-2.68; repeat 2.42x), **1.96x** instr. | not met |

Instruction ratios (callgrind) where the lane gave them: warp-wheel 5.62x to
3.88x, BBR 7.85x to 3.54x, Prism `tests/malformed.rs` 2.60x to 1.84x,
Quarry 2.54x to 1.96x.

### Default builds

- **Aura** is the one port that met the bar. Every 95% interval is at or
  below 1.00x and every port test, the leak test and the divergence checks
  pass. The owner then chose to make the port the only build (#979, in
  #977): `src/reference/` and the `vesper-port` feature are gone, and
  `bench-abba.sh` compares against the last Rust commit, `cee8b1a`. The
  report notes that the port wins mostly by doing less work than the Rust
  build (grouped edges, merge-matched keys, a lazily built snapshot), not by
  faster code generation.
- **Every other port keeps Rust as the default** and keeps its Rust code,
  under the owner's rule.
- Ignis plan at 256 passes (1.12x) and the warp-tcp codec (0.90x
  instructions) are close, but each is one workload of a port whose other
  workloads are well above the bar. Ignis's 1.12x also comes from a better
  algorithm than the Rust build's (a per-resource hazard index), so its
  report does not count it.

## What the levers bought

| Lever | Where it paid | Measured |
|---|---|---|
| Stage 1 compiler wave alone (no port change) | warp-wheel | 8.00x to 4.98x |
| Export ABI v3, state as the mirror | BBR | 11.6x to 6.9x (instructions -49%) |
| Export ABI v3, slices of mirrors | Quarry, Ignis | Quarry -3.5% instructions; Ignis within noise |
| `sum_be16`, wide reads and writes, block copies | warp-tcp codec | 7.63x to 0.90x instructions |
| `find_byte`, `extend`, slice `eq`, byte classes as masks | Prism | about 5.3x to 2.0-3.4x |
| Handler-free helpers (`div`, `pow2`, shifts by rotation) so they inline | warp-wheel, BBR | wheel 4.39x to 3.99x over three levers; BBR 6.9x to 6.5x |
| Scratch moved from handle fields into locals | Quarry, Aura | Quarry -2.5% and -3.2%; Aura -1.3% (with ABI v3) |
| Port algorithms (grouped edges, hazard index, merge-matched keys) | Aura, Ignis | Aura -43% instructions in one step; Ignis plan 256 4.53x to 1.15x |

## Compiler gaps the ports found

Each gap has a minimal repro in the report named in the last column. They
are grouped into lanes of the next compiler wave in
[`docs/agent-prompts/port-gaps/`](../agent-prompts/port-gaps/README.md).

| Gap | Ports and measured share | Reports | Next wave |
|---|---|---|---|
| Every local lives in a stack slot; every slot zeroed at entry, twice | wheel 47% (19.4% zeroing, 27.7% spills/reloads); BBR largest share of 84.8% Vesper code; warp-tcp transfer state machine; Ignis 6 KB frames | wheel 1-2, BBR 1, warp-tcp 2, Ignis 1 | already claimed: #972 (PR #983) |
| Inliner refuses slice-parameter callees | Prism 23.5%; wheel, warp-tcp, Quarry | Prism 1, wheel 6, warp-tcp 1 | already claimed: #972 (PR #983) |
| Aggregates built in zeroed temporaries then copied (struct literals, variants) | BBR `ack_progress_new` 5.96% before caching; Ignis `validate_use` about 630 instr. per call | BBR 2, Ignis 1 | struct literals: #972 (PR #983); variants in the result slot and constant folding: lane 7 |
| Sequence header: two allocations plus a thread-local `HashMap` entry per sequence | Quarry 21.1%; Prism allocator 23.8%; Aura 10.5% | Quarry 1, Prism 6, Aura 4 | lane 1 |
| Leaks: local structs with sequence fields, dropped local handles, replaced sequence fields | Quarry and Aura leak tests (worked around) | Quarry 2, Aura 4 | local struct: #972 follow-up; the rest lane 2 |
| `E0301` on disjoint fields; no `copy_within`; no move/swap through `inout` (`E0305`) | Aura 1.4% + half of 3.0%; Quarry code only | Quarry 3, Aura 2-3, Ignis 3 | lane 3 |
| No byte-class scan, ASCII case-fold compare/copy, or two-needle scan | Prism about 30% (12.1% + 14.7% + 3.2% + 6.0%); Quarry 5.2% + lexer | Prism 2-4, Quarry 6 | lane 4 |
| No `leading_zeros`/`trailing_zeros`/`count_ones`; variable shift always raises | wheel (worked around with tables and rotations) | wheel 4-5 | lane 5 |
| Inliner budget: small helpers over 48 units, short-circuit predicates, generic callees (`Ord::cmp`), handled callees, `Str` parameters | BBR (`mul_gain` 23 sites, 2.6%), Prism 5.9% before rewrite, Ignis 9% on `main`, Quarry `lex::byte`/`bound` | BBR 3, Prism 5, Ignis 2, Quarry 5, wheel 7 | budget and predicates: #972 (PR #983, budget 96 with a new cost); generic, handled and `Str` callees: lane 6 |
| Fused `try_get` compares twice; sequence header reloaded per element through `inout` | every element read; Quarry `identifier` + `lex` 4.3 M | wheel 3, Quarry 6 | lane 7 |
| `@export` directives read from the first file by name, not the root module | Aura, Quarry (silent v2) | Aura 1, Quarry 4 | lane 8 |
| `Option`/`Result` arguments convert at the boundary; `SeqRef` has no slice view | BBR 8.5% glue; Aura under 1% | BBR 4, Aura 6 | lane 8 |
| Receiver loan blocks reading the same sequence in arguments (`E0410`) | none at run time | Aura 5 | deferred: language decision (two-phase borrows) |
| Wider vectors (AVX2) | Prism, warp-tcp scans | stage1-perf-simd-bytes | deferred: Cranelift 0.125 has no 256-bit types |
