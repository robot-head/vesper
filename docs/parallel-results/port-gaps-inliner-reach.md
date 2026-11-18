# Port gaps lane 6: inliner reach (#989)

Claim #989, PR #1007. Base: `07e7d4e` (main with #983). Everything is on the
native optimize path in `src/ir_inline.rs`; nothing in `ir.rs`,
`ir_opt::optimize`, `verified_codegen`, `lean`, `bootstrap` or
`packages/vesper-compiler` changed.

## What main does with #983 in

The lane named three callee kinds that stay calls. Re-measured on `07e7d4e`
with the new `Report::refused` (why each remaining direct call stayed a
call), over Ignis, BBR, warp-wheel and Quarry:

- **Bounded generics** (`sort_seq[T]` -> `less[T]` -> derived `Ord::cmp`)
  already reach the IR monomorphized (`less$mono$Edge`, ordinary functions)
  and inline. What kept Ignis's derived `cmp` out of line was its **cost after
  its own callees were inlined into it**: `impl$Ord.Edge$cmp` costs 36 before
  its five `u64` compares are inlined into it and 106 after, over the
  `small_callee` budget of 96. Most of the growth is the forwarding blocks
  each inlined `return` leaves and the `Less`/`Equal`/`Greater` constants.
  The same growth is behind almost every remaining refusal in all four
  ports (`Size`).
- **`in Str` callees** (Quarry `util::bound`) already inline since #983: a
  `Str` is a 24-byte aggregate view, bound or borrowed like any aggregate.
- **Handled callees** were refused (Quarry `util::div_rem`; BBR's `div` and
  `pow2` before #950 rewrote them without handlers). **Unbounded generic**
  callees (`fn id[T]`) were refused; none of the four ports has one on a
  hot path.

## What changed

1. **Cost.** `cost(world, f)` no longer counts nullary constructors of a
   scalar layout, name bindings of scalar values (register moves), or blocks
   that hold only free instructions and end in a jump. Edge cmp fits again,
   and Ignis's `Edge` and `SlotItem` cmp now inline into their sorts.
2. **Handled callees.** A handled body and its clauses are lowered from
   shared nested graphs (`ir::nested_name`) that reach the enclosing
   function's locals only through the handle's `aliases`. The copy re-points
   `aliases`, `moves` and `state_value` at the renamed locals, so the
   handler's scope stays intact in the caller. Three limits:
   - A callee with a `return` inside a handled body stays a call, because
     that `return` leaves the callee.
   - A handled callee borrows only whole caller locals.
   - The call graph has edges from a function to its handler graphs, so a
     function that calls itself from a handled body is recursive. A handled
     callee is charged for its handler graphs, which native lowering emits
     at every copy. Both points came from the Codex review.
   - A caller takes at most `Budget::caller_handles` (8) inlined handles,
     counted transitively. Native lowering copies the enclosing scope's
     bindings at every handle, so its cost grows with the number of handles
     in one function. Without the cap, `tests/integer_widths.rs` took 307 s
     instead of 35 s (and timed out on BuildBuddy): it calls a doubly
     handled helper 49 times from one `main`. With the cap it takes 45 s.
3. **Unbounded generic callees** inline as a copy typed with the call's type
   arguments (`World::calls` at the call's span). A copy stays a call when
   the native backend would look up a type in its body by source span with
   the enclosing function's type arguments: nested generic calls, explicit
   call types, handlers, and operators, indexing or projections on its values.
4. **`Report::refused`**: callee -> reason -> count.

Executable objects keep inlining as they have since #959. The decision depends
only on the world, so verifier-observed builds stay byte-identical.

## Stage 1 guard

Stage 1 builds with `optimize` off, so the inliner never runs there. The
`bootstrap-stage1` workflow ran on the branch: "Stage 1 reproducibility and
cross-backend ratchet" and "Stage 0 two-seed build and Path B" passed. That
job's fresh `--reproduce --outcome` lock matches `bootstrap/stage1.lock`.

## Repros

`vesper build --lib`, `objdump -d` of the function, main -> branch:

| Repro | main | branch |
| --- | --- | --- |
| handled `mix` (`div`, `pow2`, `div_rem`) | 35 insns, 3 calls | 87 insns, 0 calls |
| `count_less[Edge]` (derived cmp, 3 `less` sites) | 181 insns, 4 calls | 755 insns, 0 calls |
| `run` with generic `pick`/`some` | 58 insns, 2 calls | 55 insns, 0 calls |
| `in Str` `bound` (Quarry shape) | inlined | inlined |

Inliner sites over each port's whole program (refusal reasons from
`Report::refused`):

| Port | sites main -> branch | callees still refused main -> branch |
| --- | --- | --- |
| Ignis | 552 -> 561 | 29 -> 23 (TexEntry cmp still over budget: 235) |
| BBR | 281 -> 285 | 11 -> 10 |
| warp-wheel | 256 -> 256 | 8 -> 7 |
| Quarry | 707 -> 733 | 50 -> 42 (`div_rem` now inlines) |

## Ports

Setup: cloud x86-64, 4 vCPU Xeon, Rust 1.97 release. Each port was built
twice with `VESPER_PORT_COMPILER` set: once to main's `vesper` (`07e7d4e`)
and once to this branch's. No port source changed. The
Codex-review fix left every port binary byte-identical (md5).

Instructions (callgrind, deterministic):

| Port | workload | Rust | main | branch | ratio main -> branch |
| --- | --- | ---: | ---: | ---: | --- |
| BBR | `ack_path_abba measure 20000` | 21.97 M | 51.73 M | 49.73 M | 2.35x -> 2.26x |
| Quarry | `planner_abba window 20` | 19.47 M | 32.94 M | 32.90 M | 1.69x -> 1.69x |
| Ignis | `abba measure 20` (all six workloads) | 116.17 M | 175.34 M | 174.09 M | 1.51x -> 1.50x |

Wall clock: two ABBA runs per arm, interleaved main, branch, main, branch
in one session. The figures are medians, with the 95% interval of the
median in parentheses.

| Port | main run 1 | branch run 1 | main run 2 | branch run 2 |
| --- | --- | --- | --- | --- |
| Ignis plan 64 | 1.83x (1.77-1.95) | 1.69x (1.34-2.13) | 1.70x (1.25-2.30) | 1.72x (1.20-2.01) |
| Ignis plan 256 | 0.87x (0.77-1.11) | 0.77x (0.68-0.98) | 0.74x (0.69-1.02) | 0.74x (0.67-0.90) |
| BBR (16 x 200k) | (2.25-3.56) | (2.73-3.13) | (2.66-3.15) | (2.39-4.07) |
| Quarry (driver) | ~2.0x | ~1.98x | ~2.15x | ~1.88x |
| warp-wheel (30 epochs) | 1.87x (1.78-1.91) | 1.89x (1.87-1.91) | 1.88x (1.85-1.91) | 1.90x (1.86-1.95) |

All wall-clock differences between the main and branch arms are inside the
noise of this machine. The deterministic change is BBR's 3.9% fewer
instructions. The ports' hot helpers were already inlined by #983, or the
ports had already rewritten them by hand (BBR's handler-free
`div`/`pow2`, Quarry's inline `bound`). This lane mainly removes the need for
those rewrites. Rust stays the default build of every port.

## What is left

- **Size.** Most remaining refusals in every port are `Size` on callees of
  100-700 after their own inlining (Ignis `TexEntry` cmp 235, BBR
  `mul_div_sat` and `start_probe_bw_down`, warp-wheel `WheelCore::unlink`).
  A budget that charges a callee for what it costs after the caller's
  simplification, or partial inlining of the hot path, would reach them.
- **Code size.** Inlining a derived `cmp` at each site multiplies its body:
  `count_less[Edge]` grew from 181 to 755 instructions.
- **Generic copies** that apply operators or nested generic calls stay calls
  until the native backend types those from the IR instead of by span.
- **Handles in one function** cost native lowering more than linearly,
  because each handle copies the enclosing bindings, which grow with every
  inlined copy. The cap keeps that cost bounded. A native fix would let the
  cap go.
- **Recursive** functions (Quarry's parser and evaluator) stay calls.
