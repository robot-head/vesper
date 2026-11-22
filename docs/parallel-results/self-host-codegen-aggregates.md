# SH4 lane 8: native aggregates, patterns, equality, drops and fip

Claim #1282. Prompt:
[docs/agent-prompts/self-host-sh4/08-codegen-aggregates.md](../agent-prompts/self-host-sh4/08-codegen-aggregates.md).
Rust stays the default build: nothing here installs a hook or changes
lowering.

## Part 1: the aggregate and drop decisions

The lane starts with what it can do before lane 1 (#1275) lends Cranelift
handles to Vesper: the parts of native lowering that read the IR and decide,
with no emission. Part 1 ports the decisions of src/native_aggregates.rs
and the drop plans of src/fip.rs to
`packages/vesper-codegen-aggregates` (`@vesper/codegen-aggregates`):

| Rust (src/native_aggregates.rs) | What it decides | Vesper |
|---|---|---|
| `plain_data`, `drop_free` | whether a type's storage needs no zero / a write into it drops nothing | `plain_data`, `drop_free` (layout walks over @vesper/check-layout) |
| `split_words` | the scalar words of a copy struct or tuple (≤ 16) | `split_words` |
| `split_locals` | which locals live in word variables (#1090), with the paths their uses read | `split_locals` |
| `plan` with `Graph` and `drop_unsafe_places` | where each constructed aggregate is built: the result buffer, a field of a later constructor, or a write's destination (#990, #1022) | `plan`, `consumer_home`, `param_home`, `sources`, `pass_through`, `window_ok`, `drop_unsafe_once` |
| `split_sources` | the reads that lend their storage to a split `let` | in `function_decisions` |
| `fip::needs_drop`, `may_need_drop`, `elem_needs_drop`, `type_contains_box`; `type_is_recursive` (src/native.rs) | what a value of each local's and value's type owns, and which types get a drop or equality helper function | `needs_drop`, `may_need_drop`, `elem_needs_drop`, `contains_box`, `type_is_recursive` |
| `fip::seq_drop_plan`, `borrowed_scrutinees` | the sequence, string and aggregate slots native code frees itself | `seq_drop_plan`, `borrowed_scrutinees` |
| `fip::replace_drops` | the writes that free the value they replace | `replace_drops` |
| `fip::temporary_drops` | where each owned temporary is dropped | `temporary_drops` |

The package reads a program through @vesper/place-ir (#1259) and the world
through @vesper/check-data, and keeps every Rust map as a dense table by
value id, block, local or symbol. The Rust side,
`vesper::native::aggregate_decisions` (src/native_aggregate_decisions.rs, a
read-only module inside `native` so it can call the private decision
functions), prints the same decisions as text. It is the only change to
src/: a `#[doc(hidden)] pub mod` line in src/native.rs and the new file,
which calls the decision functions as `Codegen` does for a top-level graph
and prints what they return.

The decisions also read four facts the checker's `World` has that
check-data does not carry. The test appends them after the program section
(tests/self_host_codegen_aggregates.rs `facts`): every world function with
whether its effect row is empty and whether it is `fip`, the element-view
spans, the free names of each function's closure and handler bodies, and
for each closure instruction whether it is a lent closure (§4.2) and the
names its body uses (closure and handler expressions stay Rust payloads in
place-ir).

`seq_drop_plan` and `borrowed_scrutinees` are also ported by lane 9
(@vesper/codegen-seq, #1295). The two PRs are open at the same time, so
this package keeps a private copy, which `temporary_drops` needs. Once
both have merged, a follow-up makes this package depend on lane 9's copy
(a TODO in the source names #1295).

### Differential

tests/self_host_codegen_aggregates.rs lowers every corpus world the way the
native optimize path does (FFI drops, `ir_opt`, the inliner, drop
elaboration, bounds hoisting) and compares, for every graph the path plans
(every function but `fip` ones), Rust's decisions with the package's
`aggregate_decisions` entry, line for line. The default run takes every 4th
world in an optimized build and every 24th in a debug one (Bazel's, which
passed its 900 s limit at every 4th); `CODEGEN_AGGREGATES_STEP=1` takes all
of them.

Full corpus, release build:

| | Count |
|---|--:|
| Worlds | 294 (0 not encodable) |
| Functions | 17,888 |
| Decision lines | 617,437 |
| Locals (`plain_data`, `split_words`) | 339,835 |
| Split locals | 4,157 |
| Homes in the result buffer | 5,532 |
| Homes in a constructor's field | 2,512 |
| Homes in a write's destination | 46 |
| Split sources | 6 |
| Locals with a drop fact | 15,384 |
| Values with a drop fact | 206,048 |
| Slots `seq_drop_plan` frees | 5,045 |
| Writes that free what they replace | 4,522 |
| Temporaries dropped | 16,462 |
| **Worlds that differ** | **0** |

Cost over the same 294 worlds (`decisions_cost`, `CHECK_HARNESS_SPEED=1`,
best of 5, release build): Rust's decision functions with their text 7.66 s;
the package's entry, decoding the input included, 4.57 s (0.60x of Rust);
encoding the input 2.57 s (mostly check-data's world section, which lane 2's
encode-once input removes for the swap).

The decisions run with no generic arguments (top-level graphs), as `Codegen`
plans a non-generic function. Specialized instances and nested graphs
(handled bodies) are left for the swap, where the hook receives the
instance's generic map and `nested` flag.

## Part 2: emission (drop glue first)

Lane 1's #1309 lends the native backend's builder and module to a family
hook. The emitter is `packages/vesper-codegen-aggregates/emit`
(`@vesper/codegen-aggregates-emit`, hook `drops_hook`), separate from the
pure decisions package because emission needs the `Ffi[Cranelift]`
capability.

The first ported request is `codegen_swap::drops::DROP_BOX`
(`Codegen::drop_box_value`). It covers the inline drop of a value that owns
boxes (null test, box fields, the enum tag chain), the recursive types'
helper functions (`__vesper_drop_<fnv1a of the type's text>`, local in an
object, preemptible in JIT) and the box frees (`vesper_box_free`). An
object's first box free also defines the AOT box runtime: the
`vesper_box_live` counter, the libc `free` import and the local
`vesper_box_free` body (`fip::aot_runtime::declare_or_define_free`), found
again by name through the shim's `module_get_data`. Blocks,
instructions, imports and declarations are made in the Rust emitter's
order.

The request carries only the value, so the test's glue appends the value's
type, together with the ADTs it reaches, as a check-data input
(`check_words::layout_input`). The hook checks the whole drop before it
emits anything, and declines when the drop:

- reaches a `Box[dyn T]`, whose drop belongs to the dyn runtime (lane 10);
- targets a freestanding object, where Rust reports the missing box
  runtime.

The differential is
crates/vesper-codegen-swap-test/tests/self_host_codegen_aggregates_drops.rs.
It compares CLIF per function, object bytes and JIT runs, with the
optimizer on and off. It runs over the corpus worlds whose objects free
boxes (the corpus has 1) and over the `Box` programs of tests/fip.rs and
tests/borrowed_box_match.rs, which include recursive lists and enums of
boxes.

| | Count |
|---|--:|
| Builds compared | 12 (6 programs, optimizer on and off) |
| Box drops emitted from Vesper | 140 |
| Declined | 0 |
| CLIF, object and run differences | **0** |

## Stage 1 guard

No file of the Stage 1 guard is touched: src/verified_codegen/**, src/ir.rs,
`ir_opt::optimize`, lean/**, bootstrap/** and packages/vesper-compiler/**
are unchanged. src/native_aggregates.rs and every lowering path are
unchanged; the new module only calls the decision functions and is not on
any compile path. The default compiler's objects are byte-identical.

## Compiler gaps

1. **@vesper/check-layout has no 16-byte scalars.** Its scalar test
   (check-data's `Known::is_scalar`) predates `i128`/`u128` (#1219), so it
   refuses the layout of a wide integer and of any aggregate holding one,
   where Rust's `Layout::of` gives a 16-byte scalar. The corpus has no such
   program, so the corpus differential could not show it (found in review).
   Until check-layout learns wide scalars (with lane 3, #1277, which owns
   the 128-bit aggregate work), the package reports a graph whose value or
   local types mention `i128`/`u128` (through type arguments, tuples,
   functions and ADT fields) as `wide` and leaves it to Rust; the test
   compares the program's other graphs.
   `wide_integer_graphs_are_left_to_rust` covers it with a minimal repro:

   ```vesper
   struct P { a: i128, b: u64 }
   fn wide(x: i128) -> i128 { x }
   fn holds(n: u64) -> P { P { a: 1, b: n } }
   ```

   The swap must fall back to the Rust decisions for those graphs too.
   Where the decisions need `numeric::is_integer_name` and
   `is_scalar_name`, the package already interns the two names itself.

## What is left

1. The rest of emission: addressing, allocation, loads and stores,
   `data_into`, pattern tests and bindings, equality helpers and fip box
   operations. This includes taking over lane 1's pilot. Aggregate
   instructions will recompute homes and split words from the Program, which
   lane 1 agreed to. The `ffi_lowering::drops` pass goes through SH3 lane 1's
   IR harness.
2. 128-bit elements after lane 3 (#1277) merges.
3. The `aggregate` and `drops` hook installs, the full suite and the SH4
   scoreboard row.
