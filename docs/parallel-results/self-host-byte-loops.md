# SH1 lane 3: byte loops, slice headers and range facts in loops

Claim #1146. Prompt:
[03-byte-loops.md](../agent-prompts/self-host-sh1/03-byte-loops.md). PR
#1167. The base is `e29e63d` (origin/main with the SH1 plan #1158).

## Reproduced first

Each repro was re-run on `e29e63d` before any change (the repros are
the functions in `tests/byte_loop_facts.rs`):

| Shape | On main |
|---|---|
| `for i in 0..e` after `if e > s.len() { return }` | versioned behind a guard (bounds facts #1118); the check runs in the slow copy |
| `for i in 0..min(e, s.len())` | versioned behind a guard |
| `loop { if i >= s.len() { break; } .. s.try_get(i) .. }` | checked on every byte |
| `loop { if i >= end { break; } .. }`, `end` from a table (span `offset`) | checked on every byte |
| `if i + 1 >= n { return } byte(i + 1)` (front end lexer) | checked |
| the same after `if at > n { Fail::raise(..) }` (span `position`) | checked: `Fail::raise` lowers as a call and a jump, so the join had two predecessors |
| `out.try_set(i, ..)` through an `inout`/`in` slice parameter, straight line | **passes**: `stable_views` (#1118) already reads the header once at entry. Dropped from scope. |
| `self.items.try_get(i)` with `in self` | header read at each access |
| `if n > 0 { loop { .. x % n .. } }` | `E0401`: every call in the body (even `wrapping_add`) forgot all facts |

## What changed

### 1. Comparison facts (`src/bounds_hoist/facts.rs`)

`drop_compared_checks` runs first in `bounds_hoist::hoist_function`, on
the native optimize path only. It makes an element access unchecked
(`SeqOp::Hoisted`, no guard, no loop copy) when the comparisons that
dominate it prove `index < len`:

- **Facts.** A block that branches on an unsigned `u64` comparison gives
  each successor whose only predecessor it is a fact (`a < b` or `a <= b`).
  The fact holds in every block that successor dominates. Facts are
  computed on the graph without the out-edges of blocks that call a
  function returning `Never` (`Fail::raise`), so the code after
  `if at > n { Fail::raise(..) }` knows `at <= n`.
- **Proof.** `index < bound` and `bound <= len`. The bound can be the
  length itself, a value another fact bounds (depth 4), or a block
  parameter whose every incoming value is at most the length (an inlined
  `min`). Checked `i + k < n` also proves `i < n`; wrapping
  `wrapping_add(i, 1) < n` proves only the access at `i + 1`, because `i`
  may be `u64::MAX`.
- **Values.** Place IR keeps locals in slots, so a value is a term:
  - a constant;
  - a parameter the function never changes, or a field path of one;
  - a single-binding local, read as the value it was bound to;
  - a read of a mutable local at a point;
  - `x + k` / `wrapping_add(x, k)`;
  - a length.

  Two reads of a mutable local are equal only when one point dominates
  the other and no write, rebinding or non-`in` loan of the local runs on
  any path between them. Lengths are compared the same way: no
  instruction that could resize, move or lend the place runs between the
  length read and the access, and no box drop of it either. So a local
  sequence filled before the loop still counts.
- **Places.** A local bound once to `t.as_bytes()` or `s.as_slice()` of a
  stable place has that place's length. An `in` parameter, an `in` or
  `inout` slice parameter, and a slice local bound once are stable.

### 2. Loops bounded by an invariant compared end

`version_compared_loops` handles a non-counted loop whose accesses a
comparison inside the loop puts below a bound the function cannot relate
to the length. The span `offset` scan is an example: `end` comes from the
line table. If the bound does not change in the loop (a local the loop
never writes, a constant, or a value defined before the loop), the loop
is versioned behind the existing `BoundsGuard` with `lo = 0` and `hi =
end`. The guard means `end <= s.len()`, or the range is empty. The fast
copy reads unchecked; when the guard fails, the original loop runs.

The pass skips counted `for` loops: they belong to the counted-loop pass
and its cost model (#1118's triangle-loop rule). It also skips the checked
copy of a loop that pass already versioned. Both versioning passes share
`copy_loop` and a `Fresh` allocator of value ids and tokens, split out of
`clone_loop`.

### 3. Headers

- **`Fail::raise` in loops.** `header_hoists` refused every loop that
  contains a `::` call. A call returning `Never` never comes back to the
  loop, so it no longer refuses those loops or counts as a call for the
  one-receiver cap. The span `offset` loop now reads `bytes`' header once.
- **Fields of `in` parameters.** `stable_views` also reads a sequence
  field of an `in` parameter (`self.items` of `in self`) once at entry,
  when every read of it is an access receiver. Nothing can write, move or
  resize a place under an `in` borrow.

### 4. Range facts in loop bodies (`src/loops/check.rs`)

`havoc` forgot the facts of every binding when the body contained any
call. A call can change a local only through an argument, a receiver or
itself (a function object): closures capture by value and handler clauses
capture snapshots. It now forgets facts only for the locals the body
names in those positions, and still for every `inout` parameter, to stay
conservative. `if n > 0 { for .. { x % n } }` and `if n != 0 { loop { x / n
} }` then need no `Fail`. A body that writes `n` still does
(`E0401`, tested).

**Manuscript (flagged in the PR).** The integer-kinds paragraph now says:
"so a range from before a loop holds in its body when the body does not
write the local (a call writes only the places it is passed or called
on)".

### Review fixes

- **Same point.** `unchanged_by` sliced `[p + 1..q]` and panicked when
  both reads were the same instruction (`let j = i` compared and used as
  the index). It broke every port build in CI. Identical points are an
  empty interval now (`snapshots_of_changing_locals`, `snapshot_index`).
- **Snapshot bounds.** A bound read from a local before the loop and
  before a later write (`let end = limit; limit = 0; loop { if i >= end
  ..`) was guarded by the local's value in the preheader, which is wrong.
  A bound read outside the loop now counts only when nothing writes the
  local between that read and the preheader (`snapshot_end`, which the
  old rule versioned behind a guard on `0`).

### Other test edits

In `tests/native_bytes.rs`, three tests counted versioned loops. Loops
over `0..s.len()` are now proved without a guard, so they check unchecked
accesses per function instead, and the resizing test asserts that the
pushing loop has no guard and keeps its check.


In `tests/native_bounds_facts.rs`, `short_inner_loops_keep_their_checks`
now takes the triangle loop's bound as a parameter. With `n = s.len()`
both loops are now proved without any guard, which is better, but the
test was meant to exercise the cost model for an inner loop.

## Stage 1 guard evidence

- **Forbidden paths.** Nothing under `src/verified_codegen/**`,
  `src/ir.rs`, `ir_opt::optimize`, `lean/**`, `bootstrap/**` or
  `packages/vesper-compiler/**` changed.
- **Checker change.** The Stage 1 corpus has no loops, so the change to
  `src/loops/check.rs` cannot reach it.
- **Native passes.** They run only with the optimizer on, and Stage 1
  builds with it off.
- **Bootstrap workflow.** `bootstrap-stage1` passed on the branch:
  [run 44648784392](https://github.com/robot-head/vesper/actions/runs/44648784392)
  (Stage 0 two-seed build and Path B, Stage 1 reproducibility and
  cross-backend ratchet).

## Measurements

Callgrind Ir on the cloud container (Linux x86-64, 4 vCPU). "main" is a
release `vesper` at `e29e63d`, "branch" is this PR. Ports are built
through `VESPER_PORT_COMPILER`.

### Span lookup (`crates/vesper-span/scripts/bench.sh` method, 5 batches)

| | main | branch | Rust |
|---|--:|--:|--:|
| whole lookup workload | 51.30M | 46.34M (−9.7%) | 34.76M |
| `vesper_span_offset` | 31.94M | 26.39M (−17%) | 24.60M (`LineIndex::offset`) |
| `vesper_span_position` | 31.16M | 30.68M (−1.5%) | 19.43M (`LineIndex::position`) |

- **`offset`** is now 1.07x of Rust. Its scan is the guarded `while` loop
  (§2), with `bytes`' header read once (§3).
- **`position`'s loop** has no check left: its bound `at` is compared with
  `text.len()` before a `Fail::raise`. Per byte it still runs about 19
  instructions, because `b >= 128 && b < 192` is materialized as a `bool`
  (`setb`, `movzbq`, `test`) and the `if` result goes through a block
  parameter. See gap 1.

### Minimal kernels (Ir per call, 64 bytes, `vesper build` executables)

| Kernel | main | branch |
|---|--:|--:|
| `sum_checked` (`for i in 0..e` after `if e > len`) | 541 | 533 (guard and loop copy gone) |
| `sum_min` (`for i in 0..min(e, len)`) | 855 | 663 |
| `sum_while` (`while i < len`) | 723 | 531 |
| `sum_until` (`while i < end`, guarded) | 748 | 602 |
| `pair` (straight-line after `i + 1 >= n`) | 34 | 31 |

### Front end v2 lexer (`tests/self_host_front_v2_bench.rs once_for_callgrind`)

| | main | branch | Rust |
|---|--:|--:|--:|
| `run_vesper_lex` (tokens into tables) | 608.0M | 585.3M (−3.7%) | 83.5M (`run_rust_lex`) |
| `run_vesper_lex_raw` (scan only) | 403.3M | 392.3M (−2.7%) | |
| `run_vesper` (lex and parse) | 1,430.2M | 1,398.2M (−2.2%) | |

The lexer reads bytes through helpers (`block_comment`, `string`) that are
not inlined, so most of its checks stay (gap 2).

### Compile time

The same run JIT-compiles the front end package first, which shows what
the new passes cost the compiler. The first version rebuilt the facts for
every loop it tried and counted single bindings once per name; the bounds
pass went from 1.04G to 7.15G Ir. Facts are now built once per versioned
loop (and not at all without a candidate loop), and single bindings come
from one walk:

| | main | branch |
|---|--:|--:|
| `bounds_hoist::run` | 1.04G | 2.42G |
| whole front end compile and run | 72.06G | 73.38G (+1.8%) |

### Std data (`@vesper-std/arena`, `@vesper-std/intern`, drivers of `tests/std_arena.rs` and `tests/std_intern.rs`)

| Workload | main | branch |
|---|--:|--:|
| arena tree, 100k nodes, build + recursive eval | 41.54M | 41.54M |
| same arena, forward-pass eval | 43.11M | 44.10M (+2.3%) |
| interning, 1M names of 100k | 1,846.7M | 1,822.4M (−1.3%) |

- **Recursive eval.** One access per call, through `in ar`. Its header
  word is one load either way, and the cost is the call and gap 4 of the
  std-data report (the `Option[Node]` copy).
- **Forward pass.** `ar.at(k)` under `k < ar.len()` is now unchecked; the
  driver is inlined into `main`, whose arena was grown earlier, which the
  positional length rule covers. The loop body is about 130 heavily
  spilled instructions in one large `main`, and the change in register
  allocation costs about 10 instructions per iteration, more than the
  compare saves. This is noise from register allocation, not a slower
  shape: the eval loop itself has no check left.

## Compiler gaps

1. **`&&` and `if` values in byte loops are materialized.** The span
   `position` loop, which no longer has any checks, spends about 19
   instructions a byte on:

   ```vesper
   fn is_continuation(b: u8) -> bool { b >= 128 && b < 192 }
   for i in start..at {
       let b = byte(in bytes, i);
       if !is_continuation(b) {
           character = if b >= 240 { wrapping_add(character, 2) } else { wrapping_add(character, 1) };
       }
   }
   ```

   The `&&` becomes a `bool` in a register and is tested again; the `if`
   value passes through a block parameter. Branching on the comparisons
   directly (or `(b & 0xC0) != 0x80` in the port) would bring it near
   Rust's 12.
2. **Facts are intraprocedural.** The lexer helpers (`block_comment`,
   `string`) read `b[i]` where the caller knows `i < n`. When a helper is
   not inlined, its own checks stay.
3. **No facts through `Option` results or `match` on enums.** For
   example, `match find_either(..) { Some(k) => i + k, None => n }`
   bounds `i + k` by the slice length, but nothing records it.
4. **Register pressure in large inlined `main`s.** The arena forward pass
   regressed 2.3% from allocation noise alone (see above).

## What is left

- Facts across calls (gap 2) and from `find_*` results (gap 3).
- Gap 1 is general Cranelift lowering work; it is not about bounds.
