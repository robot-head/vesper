# @vesper/ir-opt

The compiler's `ir_opt` pass (`src/ir_opt.rs`) in Vesper (self-hosting wave
SH3, lane 9, #1248), on [@vesper/place-ir](../vesper-place-ir)'s tables and
[@vesper/egraph](../vesper-egraph)'s e-graph. Rust stays the default:
`ir_opt::optimize` is Stage 1 guarded (the verified backend runs it) and is
unchanged. The hybrid build's `opt` hook (src/ir_swap.rs) is the next part.

| File | What | Rust it follows |
|---|---|---|
| `src/forward.ves` | memory forwarding: per block, places, `disjoint` pairs, state terms, twelve rounds, the first earlier scalar of the read's type in its class; operand and terminator rewrite | `optimize`'s block loop, `RULES`, `resolve`, `operands` |
| `src/reuse.ves` | `@fip` constructor reuse: topological order, joins, reuse slots, `BoxReuse` and call slots | `fip_reuse_pass`, `FIP_RULES`, `flows_to_return` |
| `src/view.ves` | values, locals, places as text, the place graph's overlap | `PlaceGraph::overlaps`, `scalar` |
| `src/lib.ves` | the entry `ir_opt_entry` | |

`FIP_RULES` unions an allocation with every slot of the same `(key, rank)`
tag. Each object has one tag and ranks are unique per key in a block, so the
classes are the tag classes; reuse.ves compares the tags instead of running a
relation join, which @vesper/egraph does not have.

The world questions of the reuse pass (`fip::active`, `box_inner`,
`pattern_box_slots`, `slot_key`, `caller_slot_functions`) come in as facts
after the program; `ir::thread_tokens` and `ir_check::validate`, the end of
`optimize`, stay with the caller. The differential is
`tests/self_host_ir_opt.rs`; the report is
[docs/parallel-results/self-host-ir-opt.md](../../docs/parallel-results/self-host-ir-opt.md).
