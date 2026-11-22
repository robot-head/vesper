# Rust-exit gap wave 1, lane 2: cold inlined callees off the hot path

Claim [#1499](https://github.com/robot-head/vesper/issues/1499). Prompt:
`docs/agent-prompts/rust-exit-gaps-1/02-cold-inline.md`. Base `main` at
`66255e17` (2026-11-22). Gap 8 of
`docs/parallel-results/rust-exit-kinetic-resonance.md`, handed over by the
code-size lane (#1426).

## Repro

```vesper
fn slow(x: u64) -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(x);
    s.len()
}

pub fn hot(x: u64) -> u64 {
    if x > 1000000 {
        return slow(x);
    }
    wrapping_add(x, 1)
}

pub fn plain(x: u64) -> u64 {
    wrapping_add(x, 1)
}
```

`vesper build --lib --lib-name repro` on `main`: `vesper_repro_hot` is 74
instructions, and its common path (`x <= 1000000`) runs 30 of them: a
32-byte frame, four callee-saved saves and restores, and two drop-flag
tests on a constant `false`, against 6 for `vesper_repro_plain`.

## Cause

The inliner's choice, not native slot placement. `slow` costs well under
`Budget::small_callee`, so it is spliced into `hot`'s rare branch. The
caller then owns `slow`'s sequence: its slot, the callee-saved registers
the push's grow path needs, and the drop flags at the join. Cranelift has
no shrink-wrapping, so a register any block uses is saved in the
prologue, and every call of `hot` pays for it. Moving the branch into a
helper did not help, because the helper was inlined back.

## Fix

`src/ir_inline.rs` (`inline_into`), mirrored in
`packages/vesper-ir-inline/src/lib.ves`: a call stays a call, with the new
refusal `Refusal::Site("sequence owner on a cold path")`, when

- the callee owns a sequence (`owns_sequence`): a local, a by-value
  parameter included, whose type holds one and is not a slice view
  (borrowed `in`/`inout` sequence parameters become the caller's places
  and own nothing), and
- the call's block is cold (`cold_block`): it is on no cycle, and some path
  from the entry leaves the caller (a `return` or an `Op::Exit` of the
  caller) without passing it.

A call that every invocation runs, or that sits in a loop, still inlines:
the frame is paid once per call of the caller either way. The check comes
right after `site_bindings`, so earlier site refusals keep their reason.
The port adds reason code 35 (`R_SITE_COLD`), and
`tests/common/inline_words.rs` names it.

After the fix `vesper_repro_hot` is 12 instructions: the common path is
8 (`push rbp; mov rbp,rsp; cmp; ja; lea; mov rsp,rbp; pop rbp; ret`), 2 more
than `plain`'s frame-pointer pair, and the rare branch is a call.

Kinetic's `fastmath_sin`/`fastmath_cos` exports held the sequence
themselves (the correctly rounded branch's empty `ChunkSeq` cache), so the
inliner rule alone could not reach them. Their rare branch now calls
`cold_sin`/`cold_cos` (`crates/vesper-kinetic/vesper/src/lib.ves`), which
the new rule keeps as calls. On `main` the same rewrite changes nothing,
because the helpers are inlined back.

## Numbers

Callgrind, port arm only (`abba one port <workload> 20` for Kinetic), x86_64
cloud VM. "Base" is `main`'s compiler, "new" this branch's.

| Port / workload | Base Ir | New Ir | Change |
|---|--:|--:|--:|
| Kinetic `fastmath` | 18.50M | 17.33M | -6.3% |
| Kinetic `crmath` | 749.1M | 741.0M | -1.1% |
| Kinetic `gjk`, `colorer`, `lbvh`, `sap`, `morton` | | | +0.00% (4.6k start-up) |
| Resonance `graph` (200 blocks) | 103.99M | 104.16M | +0.17% |
| Resonance `kernel` | 103.79M | 103.79M | 0 |

Kinetic `fastmath` export wrappers: `fastmath_sin`/`cos` 49 to 22
instructions per call, `sinf`/`cosf` 59 to 29; their frame and saves are
gone (rare branch: one call).

Objects (`vesper build --lib`, `.text` per function): Warp TCP, Warp BBR and
Quarry are byte-identical to `main` (none of their cold error paths inlines
a sequence-owning callee; a first version that counted slice views as owned
sequences kept `sum_range` out of `fill_checksum_n` and cost Warp TCP's
`codec` 3%, which the slice exclusion fixed). Resonance changes five
functions: `g_render`, `p_compile`, `verify_root` and `c_verify` shrink by
3.5-12 KB, and the smaller `g_render` now fits the budget of
`render_graph`, which inlines it.

ABBA (`abba compare 30 3`, 30 epochs, median port/rust and range):
Kinetic `fastmath` 1.45x (1.34-1.57) on `main`, 1.41x (1.13-1.75) here. The
VM's timings are noisy (range over 1.1-1.8x on one build); Ir is the
reliable number.

What keeps `fastmath` above the bar is not gap 8: the port's `f_sin`,
`f_cos`, `f_log`, `f_atan` and `f_exp` bodies run 1.4-1.6x the Rust build's
instructions (25.7M vs 16.3M for `sin` over 200 runs), and the Rust build
inlines the five kernels into the benchmark loop, which an export call
cannot (lane 5, #1502). Those bodies are scalar f64 code with no sequence,
so they belong to the code-size lane (#1426): header reloads and spills.

## Checks

- `tests/native_cold_inline.rs` (new; its program is inline, outside the sampled corpus):
  the evaluator, the JIT and x86-64/AArch64 objects agree with `optimize`
  on and off; `hot` keeps its call with the new refusal; `every`
  (unconditional) and `looped` (rare branch inside a loop) inline `slow`;
  `borrowed` inlines `total`, which only borrows its sequence.
- `tests/native_inline.rs`, `native_inline_cost.rs`,
  `native_inline_inout_slice.rs`, `native_inline_reach.rs` pass unchanged.
- Inliner differential (`tests/self_host_ir_inline.rs`, default corpus
  step): all 5 tests pass, 0 of 42 programs differ in decisions, inlined
  programs and tight budgets; the new fixture alone
  (`SELF_HOST_IR_INLINE_ONLY=cold_inline SELF_HOST_IR_INLINE_STEP=1`) also
  differs in 0 programs. The port's refusal table grows to 36 reasons
  (`REASONS`); a first run without that overflowed code 35 into the next
  function's row.
- `cargo test --release -p vesper-kinetic` with the port built by this
  compiler: every test passes (31 test binaries).
- Stage 1 guard: no file under the guard changes. The inliner runs only on
  the optimized native path, never in `ir_opt::optimize` or the verified
  backend, and the Stage 1 lock corpus is built with `optimize` off (module
  docs of `src/ir_inline.rs`), so `bootstrap/stage1.lock` cannot move; the
  `bootstrap-stage1` workflow is dispatched on the branch.

## New gaps

- None in the inliner. The constant-`false` drop-flag tests that
  `fastmath_sin` showed on `main` (`xor r9d,r9d; test r9b,r9b; jne`) are a
  native lowering pattern of any caller that owns a sequence on one branch,
  and survive the CLIF clean-up on `main` today. Repro: `fastmath_sin` as on
  `main` (`crates/vesper-kinetic/vesper/src/lib.ves` before this PR); handed
  to the code-size lane (#1426).

## What is left

- The cold-path rule covers callees. A sequence the caller itself declares
  on a rare branch still costs the whole caller (the Kinetic shape before
  the rewrite); outlining such a region automatically would need a new
  function in the program, which the native export path does not support
  yet. The rewrite above is the pattern for ports until then.
- `fastmath` reaching the 1.5x bar's upper end depends on #1426 and #1502.
