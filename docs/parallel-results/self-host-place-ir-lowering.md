# Self-hosting SH3 lane 13: Place IR lowering

Claim #1252. Prompt: `docs/agent-prompts/self-host-sh3/13-place-ir-lowering.md`.
Stacks on SH3 lane 1's first PR (#1259, head `59e4e02`, itself on #1232) and
on core typing part 2 (#1237, head `f0ded8b`), merged together on this
branch; `origin/main` was `7636681` when the lane started. Rust stays the
default lowering; nothing here changes a default build or deletes Rust
code.

The lane lands in parts. This report grows with each one.

| Part | What |
| --- | --- |
| 1 (#1265) | `lower_world` and `thread_tokens` for every expression kind except closures, handlers and payload operations; the differential |
| 2 (#1265) | closures and handlers (nested functions, captures, handler clauses) |
| 3 | sequence, tiled, vector and `Box` operations (`seq::resolve` and the other resolvers, with a structured payload kind) |
| 4 | the swap: the hybrid `ir_lower` module through lane 1's `lower` hook, the full suite on the hybrid build, the scoreboard row |

## Part 1

### What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-place-ir-lower` (`@vesper/place-ir-lower`): `src/ast.ves` (the input tables), `src/lower.ves` (the lowering, `thread_tokens` and the program rows), `src/lib.ves` (entries), README |
| Input encoder | `tests/common/ir_lower_words.rs` |
| Test | `tests/self_host_ir_lower.rs` |
| Bazel data and timeout | `BUILD.bazel` |

No Rust source changed.

**The port.** `src/lower.ves` follows `ir::Lower` statement by statement:
`emit` (span types through `numeric::default_type`, the `Read` field-type
fallback, the fallible test), `place` and `argument`, `rename_pattern`
(fresh `$slot{n}_{x}` names), literals, names and unit constructors,
tuples, struct literals, fields and projections, indexing, unary and
binary operators (with `&&`/`||` as an `if`, and borrowed string
comparisons), assignment, blocks with `let` and the `Never` cut-off, calls
(world functions and methods, effect operations through capability
labels, constructors, the wrapping, bitwise, conversion, rotation and
float intrinsics, indirect calls with the callee type's modes), `?`, `if`,
`match`, and the loop lowering of `src/loops/ir.rs` (`loop`, `for` over
ranges with the step block for `continue`, `break`, `continue`,
`return`). `thread_tokens` and `state_action` run in `flush`, which writes
each function's rows in block order.

One function is lowered at a time into working tables, because lowering
appends to blocks out of order: blocks, instructions tagged with their
block over one scratch operand pool, the `names` scopes as an undo trail
on a symbol-indexed table, and `locals` as a symbol-indexed table. Value
ids, slot names and block numbers come from the same counters in the same
order as in Rust, so the output is `Debug`-equal, not just equivalent.
Header maps that Rust keeps in `BTreeMap`s (`locals`, `bounds`) are sorted
by the names' bytes.

**The input.** check-data's names, types and world (which carry
`world.types`, `partial_ops` and `overflow_ops`), then the function table,
effect operation signatures and every body as post-order expression and
pattern rows (format in `tests/common/ir_lower_words.rs`). Two things are
read off Rust in this part and written into the input per call:

- which calls and index expressions `seq::resolve`,
  `tiled::resolve_ir_call`, `VectorOp::resolve` or the `Box` builtins route
  to a payload operation (the spans of the Rust lowering's `Seq`, `Tiled`,
  `Vector` and `Fip` instructions). Those are written as foreign
  expressions, and the port skips a function that reaches one;
- the intrinsic name class of a call of a plain name (the name tables of
  `vesper-core::numeric` and `capability_effects`).

Part 3 removes the foreign expressions.

### The differential

Corpus: lane 4's, 290 worlds on this base (Cargo, Linux). Every function
the port lowers is compared with Rust's `FunctionIr` (`{:#?}` text), and
`ir_check::validate` must report the same diagnostics on the port's
functions and on Rust's same functions.

| Worlds | Functions lowered | Functions skipped | Instructions compared | Functions differing | IR check differing |
| --: | --: | --: | --: | --: | --: |
| 290 | 10,426 | 5,002 | 178,614 | 0 | 0 |

At the end of part 1, 161 of the skipped functions reached a closure or
handler and 4,841 a payload operation (part 2's numbers are below). The
unoptimized build ran the whole part 1 differential in 403 s; with part 2
and the grown corpus (296 worlds) it takes 680 s, close to Bazel's `long`
timeout of 900 s, so the test defaults to every second world
(`IR_LOWER_STEP=1` runs them all; the tables here are from full runs).

The harness can fail: dropping `overflow_ops` from the fallible test makes
`tests/fixtures/freestanding/checked_sum.ves` and
`tests/fixtures/integer_widths/kinds.ves` (and more) differ at once. The
test also asserts a coverage floor (1,000 functions), so a port that
skipped everything cannot pass.

### Numbers

Over the functions the port lowers, every corpus world (the other
functions keep their signatures and lose their bodies, so both sides lower
the same set). Port-compiler test build, ABBA, best of six, one machine.

| Step | Time | vs `ir::lower_world` |
| --- | --: | --: |
| `ir::lower_world` (Rust) | 290 ms | 1.00x |
| Vesper entry (read the input, lower, write words) | 136 ms | 0.47x |
| Vesper read alone | 23 ms | 0.08x |
| **Vesper lowering alone** (entry minus read) | **114 ms** | **0.39x** |
| Encode (Rust, `lower_input`) | 210 ms | 0.72x |
| Decode (Rust, `decode_program`) | 123 ms | 0.42x |
| Whole swap (encode + entry + decode) | 469 ms | 1.62x |

The lowering itself runs in 0.39x of Rust's time: Rust clones a `String`
for every name, `Place` and alias map entry and allocates a `Type` tree per
instruction, where the port moves `u32` symbols and hash-consed type ids.
The word encoding costs more than the lowering, as lane 1 measured; ABI v4
input and lane 1's tables are how that goes away (part 4 measures the
hybrid build).

Callgrind: the Rust side over every 50th world (`IR_LOWER_STEP=50`,
`callgrind_one_side`) is 24.5 M Ir. The Vesper side did not finish under
callgrind within two hours here, because the test JIT-compiles the
package inside valgrind. Part 4 measures Ir on the hybrid build, where the
package is compiled ahead of time.

## Part 2

### What changed

`src/lower.ves` gains `prepare_nested` and the `Closure` and `Handle`
instructions; `src/ast.ves` reads closure rows (parameters, body, moves)
and handler rows (effect label, name and arguments, state, body, return
and operation clauses, outgoing effects, moves); the encoder writes them
and the payload pool count. No Rust source changed.

- `free_names` and `clause_free_names` of `check.rs`, over the rows, with
  bound names as a symbol-indexed count and an undo trail;
- `capture_params` (the `permissions` of the function's parameters for a
  borrowed handler body), `pattern_params`, and `add_nested` with
  `nested_name`, inherited generics, the capability snapshot (a handler's
  label added for its body only), external captures and the handler
  body's owner;
- handler clauses: the effect's generics substituted into each operation's
  parameters and result, the state parameter, and the continuation's
  function type (linear or affine by the effect);
- `Op::Closure` and `Op::Handle`: the pool entry, the alias map sorted by
  name, and the moves; a stateful handler leaves out the places its state
  expression initializes (sink reads, with their projections, and inner
  closure and handler moves).

Nested functions are appended to the function table as they are prepared
and lowered after the world's, so each one sees its parent's scope as
Rust's `pending` stack does. The pool rows are the input's, by position.
A closure or clause whose body has a foreign expression is skipped,
because its free names are not all in the input.

### The differential

| Worlds | Functions lowered | Functions skipped | Instructions compared | Functions differing | IR check differing |
| --: | --: | --: | --: | --: | --: |
| 290 | 11,180 | 4,873 | 181,508 | 0 | 0 |

After merging `main` (`a64f537`) and lane 1's latest head (`8ac0f94`) the
corpus has 292 worlds: 12,150 functions lowered, 5,466 skipped, 203,790
instructions, 0 differing, and the IR check agrees.

Rust's lowering has 600 closure and handler functions on the corpus; the
port lowers 587 of them, and the other 13 hold a payload operation. Of the
world functions, 10,439 are plain, 154 have closures or handlers (no
payload operation) and 4,869 have a payload operation, which part 3
covers.

The harness catches a wrong capture: giving a borrowed handler body's
captures `sink` instead of their permission makes 13 functions differ on
every third world (`tests/fixtures/capability_effects/more/accept/resume_public.ves`,
`tests/fixtures/verified_codegen/fail_implicit.ves` and more).

### Numbers

Same method as part 1, over the larger lowered set.

| Step | Time | vs `ir::lower_world` |
| --- | --: | --: |
| `ir::lower_world` (Rust) | 322 ms | 1.00x |
| Vesper entry | 154 ms | 0.48x |
| Vesper read alone | 25 ms | 0.08x |
| **Vesper lowering alone** | **129 ms** | **0.40x** |
| Encode | 227 ms | 0.71x |
| Decode | 140 ms | 0.43x |
| Whole swap | 521 ms | 1.62x |

## Compiler gaps

1. **Re-initializing a moved owned local inside a loop is refused.**
   Rotating two buffers (`let t = a; a = b; b = t;`) in a loop is E0303
   (`use of moved place`) and E0312 (`moved in a previous iteration`), even
   though both are assigned again before the next iteration. `swap(inout
   a, inout b)` works, and the merge sort uses it. Repro:

   ```vesper
   fn main() -> u64 {
       let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
       let mut b: ChunkSeq[u64] = ChunkSeq::new[u64]();
       for i in 0..3 {
           let t = a;
           a = b;
           b = t;
       }
       a.len()
   }
   ```

2. **`effect` cannot be a method or local name** (E0002). Known: SH3 lane 5
   (#1244) makes it contextual. The port names the accessor `effect_decl`.

3. **No imported constants on this base.** The tag, opcode and kind
   constants repeat in `ast.ves` and `lower.ves` and mirror
   `@vesper/place-ir`'s. #1239 (value enums and importable constants) has
   since merged; the port switches when this branch merges `main`.

## Stage 1 guard

`src/ir.rs`, `src/loops/ir.rs`, `src/seq/ir.rs`, `ir_opt::optimize`,
`src/verified_codegen/**`, `lean/**`, `bootstrap/**` and
`packages/vesper-compiler/**` are unchanged (this part changes no Rust
source at all), and nothing installs a `lower` hook. The bootstrap-stage1
workflow runs on this branch; its digests are in the PR.

## What is left

Parts 3 and 4 above. The skipped functions are the coverage still to come.
