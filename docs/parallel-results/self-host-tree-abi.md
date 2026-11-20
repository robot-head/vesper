# Self-hosting SH1 lane 6: compiler trees across the export ABI

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1149. Base: `main` at `e29e63d`. Lane prompt:
[`docs/agent-prompts/self-host-sh1/06-tree-abi.md`](../agent-prompts/self-host-sh1/06-tree-abi.md).
Owner decision (2026-11-19): a new `// @export abi 4` with all three forms
(lent Vesper trees, owned trees decoded once, lent Rust handles), so v3
libraries and their committed bindings stay byte-identical.

Rust stays the default build everywhere, and no Rust code was deleted.

## Summary

- **Borrowed matching** (spec call, manuscript §4.3). A `match` on a place
  of an `in`/`inout` binding of non-copy type borrows the place instead of
  moving it. Pattern bindings, Box contents included, are `in` borrows, so a
  `Box` tree can be walked any number of times. The matched place is frozen
  inside the arms (`E0301`), moving a binding is `E0305` and writing one is
  `E0307`. Works in the evaluator, the JIT (optimizer on and off),
  executables and library objects.
- **Export ABI v4** (spec call, manuscript §16.3 and §16.6). Everything v3
  has, plus:
  - owned trees: a handle type whose fields are tree forms (scalars,
    `String`, `Box[T]`, `Option[Box[T]]`, `ChunkSeq` of trees, other tree
    types) gets a Rust `<Type>Tree`. `<Type>::encode(tree)` builds the
    Vesper value in the library's own allocations once, and
    `<Type>::decode()` / `<Type>Ref::decode()` copy one back;
  - sequence views: a lent view reads a `ChunkSeq` of handles in place as
    `<Elem>Seq<'a>` (`len`, `get`, `iter`), so Rust walks a whole Vesper
    tree with no copy;
  - lent Rust handles: an `in` parameter of a `.vsys` extern type
    (`affine extern type World;`) is the address of a Rust value
    (`World::lend(&world)`); Vesper reads it only through the library's
    accessor imports, which the linking crate implements. Every other
    position is refused;
  - `box_new` in the runtime symbols, which encoders call.

## What changed

| Area | Files |
| --- | --- |
| Checker: borrowed scrutinee, `match_borrows` freeze, `in` pattern binds | `src/check.rs` |
| IR checker: borrowed scrutinee reads take no loan and do not consume | `src/ir_check.rs` |
| Drops: `fip::borrowed_scrutinees`, no box/string drops for borrowed binds | `src/fip.rs` |
| Native: `borrowed_bind` suppresses the unpack frees | `src/native.rs`, `src/native_session.rs` |
| ABI v4 planner, directive, glue, bindings | `src/native_export.rs`, `src/native_export/{directives,glue,binding,binding_v2,binding_v4}.rs` |
| Spec | `docs/manuscript/content.typ` (§4.3, §16.3, §16.6) |
| Guide | `docs/maintenance/porting-a-crate.md` ("Export ABI v4") |
| Tests | `tests/borrowed_box_match.rs`, `tests/tree_abi.rs` |

The IR is unchanged (`src/ir.rs` is Stage 1 guarded): the scrutinee is
still a `Sink` read, and every later pass asks
`fip::borrowed_scrutinees(f)` which scrutinees are borrowed.

## Repros first

`6e1b489` committed the repros before the fix: matching `in e: Expr` and
binding its boxed children was `E0305` on main (moving out of a borrow), and
the tree walks of `tests/std_arena.rs` had to consume their tree
(`eval_box(sink e: Expr)`). No repro passed on main.

## Tests

- `tests/borrowed_box_match.rs` (8 tests): the `std_arena` tree walk over
  `Box` trees taken `in` and walked twice, 100,000 nodes natively (JIT both
  optimizer settings, plus the executable) and 300 in the evaluator, against
  a Rust model; payload shapes; box alloc == free and no stray seq frees;
  borrowed `String` bindings are never freed; the four negative codes.
- `tests/tree_abi.rs` (4 tests + 1 ignored measurement): the interface
  names ABI v4 and its forms; v3 libraries are unchanged (no `Tree`, `Seq`
  views, `box_new` or encoders in their bindings); extern handles cross only
  as `in` parameters; and an end-to-end program, linked with `rustc` against
  the library object, that encodes a lean-shaped `Term`/`Decl`/`Module`
  tree, walks it through borrows in Vesper, reads it through lent views and
  `DeclSeq`, decodes it, re-encodes it, calls a Vesper function on a lent
  Rust `World` handle through the accessor imports, and ends with zero live
  library allocations.
- Pre-existing suites rerun green: fip, native_drop_glue, std_arena,
  dyn_box, native_fail_exit_drops, native_specialized_sink, continue,
  loop_boxes, ffi_handles and the native_export suites (v2, v3, cli, f32,
  handle seqs, lent handles, arrays, package submodules).

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
`bootstrap-stage1` workflow was dispatched on the branch to show the
reproduce digests equal `bootstrap/stage1.lock`; the PR links the run.

## Numbers

Callgrind instructions on a 2,001-node lean-shaped term
(`cargo test --test tree_abi -- --ignored measure`), per node:

| Step | Ir/node |
| --- | --- |
| Flat encoder (the shape of the lean-syntax port's 611-line encoder) | 45 |
| v4 `encode` (Rust tree to Vesper value) | 394 |
| Drop of the encoded value | 181 |
| v4 `decode` (back to a Rust tree) | 516 |
| Borrowed walk in Vesper (before: impossible without consuming) | 133 |
| Vesper render to `String` | 250 |
| Rust reference printer | 57 |

So the owned tree path works but costs about 9x the flat encoder per node
today. The cost is allocation: one `calloc` per box and a `seq_new`/free
pair per string (gap 1). The lent view path costs nothing to cross: Rust
reads the Vesper tree in place.

## Compiler gaps

1. **Per-node allocation in encode.** Each `Box` is its own `calloc`, and
   each `String` a `seq_new` + copy. A bump region per encoded tree (or
   `vesper-alloc` regions in the runtime) would bring encode near the flat
   encoder. Repro: `measure` in `tests/tree_abi.rs`, the `encode` row.
2. **Nested `Box` patterns are not covered.** `match e { Expr::Neg(Box(a))
   => ..., _ => ... }` is reported non-exhaustive (`E0212`) though the
   wildcard covers it. The tests use nested matches instead.
3. **`inout` bindings through a `Box`.** Matching an `inout` place binds
   `in` borrows for now; writing through a binding needs a Place IR
   projection into box contents, which is in `src/ir.rs` (Stage 1 guarded).
   Repro: `inout e: Expr`, `match e { Expr::Add(l, r) => { l = ... } }` is
   `E0307`.
4. **Deep recursion frames.** A Vesper render of a 10,000-deep right-leaning
   tree overflows the 8 MB main stack; measurements use n = 1,000.
5. **No view of a `ChunkSeq[String]` element.** `<Elem>Seq<'a>` covers
   handle elements; a sequence of strings still decodes.

## What is left

- Swap the lean-syntax port's flat encoder for the owned tree path in a
  follow-up (the lean-shaped pilot above runs without one), once gap 1
  narrows the cost; nothing was deleted.
- `Layout::of(world, ty)` in `crates/vesper-hybrid` over a lent `World`:
  the design is the lent-handle form above and the `World` pilot in
  `tests/tree_abi.rs`; the hybrid crate was not touched.
- `inout` borrowed bindings (gap 3), in a Stage 2 Place IR change.
