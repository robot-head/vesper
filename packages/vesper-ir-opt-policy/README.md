# @vesper/ir-opt-policy

The native optimizer policy (`src/ir_opt_policy.rs`) in Vesper (Rust-exit gap
wave 1, lane 12, #1510), around [@vesper/ir-opt](../vesper-ir-opt)'s pass. The
policy is the default compiler's `opt` path (`src/ir_swap.rs`). It gives
`ir_opt::optimize`'s program and count, and skips the blocks that memory
forwarding cannot change.

| File | What | Rust it follows |
|---|---|---|
| `src/screen.ves` | the block screen: `block_may_improve` (a sink read of a local with an earlier instruction on its place), then `block_can_improve` (the walk of each scalar sink read's state back across disjoint writes) | `block_may_improve`, `block_can_improve`, `place_of` |
| `src/lib.ves` | `policy_words`: @vesper/ir-opt's `ir_opt_words` with forwarding only on the kept blocks (`forward_program_kept`), and the harness entries | `optimize_with_stats` |

The Rust policy drives `ir_opt::optimize` through per-block surrogates,
threads and an opt-in cache, because the Rust pass takes whole functions.
@vesper/ir-opt already works per block, so a mask over the block rows replaces
the surrogates. The threads and the cache have no counterpart here: the
hybrid calls the pass once per compilation, and the cache is off by default.

The hybrid build's `opt` hook (`crates/vesper-hybrid/modules/ir_opt`) calls
`policy_words`. The differential is `tests/self_host_ir_opt_policy.rs`; the
report is
[docs/parallel-results/rust-exit-gaps-leftovers.md](../../docs/parallel-results/rust-exit-gaps-leftovers.md).
