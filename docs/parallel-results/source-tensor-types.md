# Source tensor types (wave eight, lane 10)

Design §28.2: symbolic shape-typed tensors, quantized wire-struct blocks and a
linear `KvPageLease`. Claim issue #459. Base `1094e64` (plan PR #477), merged
with `main` at `83872e5` (PR #379).

## What was built

`src/tensor_types.rs` and `src/tensor_types/` (module docs hold the full rules):

| File | Contents |
| --- | --- |
| `dim.rs` | Dimensions in normal form: a constant times a product of variables. `M * K == K * M`, overflow (> 2^40), zero and factor limits are errors. |
| `types.rs` | The tensor view of source types, generic kinds (dimension, dtype, type) inferred from use, and type erasure. |
| `wire.rs` | `wire struct` layouts checked by the Vesper Bridge layout checker (`vesper_ffi::Registry`), and their binding to the Q4_0 (18-byte) and Q8_0 (34-byte) host formats. |
| `infer.rs` | Shape inference over function bodies (let, blocks, if/match joins, records, `Option`/`Result`, `?`, closures), unification at calls, inferred dimension preconditions, and every static rule below. |
| `elaborate.rs` | Lowering to core syntax: the built-in prelude, erased tensor types, a hidden leading `u64` parameter `$dim_N` per dimension parameter, dimensions passed at each call, and a `Dims` descriptor for `reshape`/`fill`. |
| `eval.rs` | The evaluator: the elaborated program runs on the existing one-shot effect machine (`Runtime::begin_external`); every `Tensor` and `KvCachePool` operation suspends it and the host services it with `vesper-tensor` kernels. |

Surface (all additive syntax in `crates/vesper-syntax`, own commit):

- `Tensor[DType, [d, ...]]`. A dimension is a decimal constant, a dimension
  parameter (`N: dim`, or any generic used in a dimension position) or a
  product (`QHeads * HeadDim`). Rank is at most 4.
- dtypes: `F32`, `Bf16`, a quantized block wire struct, or a generic used only
  as a dtype.
- `wire struct Q4Block32Wire { scale_fp16: u16le, packed_nibbles: ByteArray[16] }`.
- `KvPageLease[KvHeads, HeadDim]` and `ActiveKvSeq[KvHeads, HeadDim]`, both linear.
- Operations of the built-in linear effect `Tensor`: `matmul`, `matvec`, `add`,
  `mul` (broadcasting), `swiglu`, `reshape[[..]]`, `transpose`, `softmax`,
  `silu`, `to_bf16`, `to_f32`, `argmax`, `fill[[..]]`, `rms_norm`, `rope`,
  `qmatvec`, `block_scale`.
- Operations of the built-in linear effect `KvCachePool[KvHeads, HeadDim]`:
  `begin_sequence`, `append_kv_step`, `attend` (grouped-query), `tokens`,
  `release_sequence`, `lease_page`, `write_page`, `page_tokens`, `release_page`.

### Static rules and their codes

| Code | Rule |
| --- | --- |
| E0761 | Malformed tensor type, unknown dimension, non-dtype element type, generic used at two kinds |
| E0762 | Zero or overflowing dimension, rank above 4 |
| E0763 | Shape mismatch: matmul/matvec inner axis, broadcasting, reshape element count, ranks, call arguments, results, let annotations, branch joins, K/V step shape |
| E0764 | A dimension or shape cannot be inferred |
| E0765 | RoPE even head width, GQA `QHeads % KvHeads == 0`, quantized rows of whole 32-element blocks |
| E0766 | Element type per operation (F32 arithmetic, Bf16 K/V steps, conversions) |
| E0767 | Wire struct layout (implicit padding, field types, copy and non-generic), and a dtype that is not a Q4_0/Q8_0 block layout |
| E0768 | A tensor, lease or sequence is built, destructured or projected; a non-linear type holds a lease |
| E0769 | A source handler for `Tensor` or `KvCachePool` |
| E0770 | Unsupported uses: tensor operators, indexing, unknown operations, tensors in impl/trait/effect items |
| E0771 | A lease or sequence used with another pool instance than the row's `KvCachePool[..]` |

Inside a function its dimension parameters are rigid. At a call they are
unified with the arguments and the expected result; `rotate[D](x: [1, D * 2])`
applied to `[1, B * 2]` solves `D = B`. A requirement that depends on a rigid
dimension (for example RoPE on `[H, D]`) becomes an inferred precondition of
the function (`FnSig::obligations`, a fixed point across recursive calls) and is
discharged at each call; the evaluator checks the entry's preconditions with
the concrete dimensions.

Leases: the core checker's existing linear rules already require every
`KvPageLease`/`ActiveKvSeq` to be consumed exactly once on every path (E0302,
E0303, E0309 fixtures). This lane makes the pool operations the only
entrances and exits: no literal, pattern or field access on those types or on
`Tensor`, no source handler for the effects. The consuming exits are
`release_page(sink page)` and `release_sequence(sink seq)`. The host evaluator
also counts pool pages still allocated at exit (`Stats::allocated_pages_at_exit`).

## Evaluator vs vesper-tensor

`tests/tensor_types.rs` runs each accepted fixture and calls the same
`vesper-tensor` APIs directly on the same inputs. Results are compared bit for
bit:

- `projection.ves`: matmul plus broadcast bias and reshapes, against a naive
  ascending-`k` matmul; two batch sizes from one program.
- `layer.ves`: `rms_norm`, `rope` (base 10000), SwiGLU MLP via `Matrix::matvec`
  and `ops::swiglu`, `softmax`, `argmax`; broadcasting `add`/`mul`, `silu`,
  Bf16 round trip, `transpose`, `fill`.
- `attention.ves`: two decode steps over a paged Bf16 `KvPool` with
  `paged_gqa_attention` (two-pass), 4 query heads over 2 KV heads.
- `quantized.ves`: Q4_0 and Q8_0 `qmatvec` against `Matrix::quantized(..).matvec`,
  and a block scale read through the wire layout (`vesper_ffi::view_wire`).
- `pages.ves`: a page lease filled to its 16-token limit, then released; the
  out-of-pages path.
- `generic.ves`: dimension inference through records (`Linear[I, O]`),
  `Option` branches, product dimensions and RoPE.

`tests/tensor_types_reject.rs` checks 38 rejected fixtures, one or more per
rule, each with the expected code, message and source span.

## Checks

Run on the cloud host (x86_64 Linux, Rust 1.97 and 1.100):

- `cargo test --test tensor_types --test tensor_types_reject`: 9 + 2 passed.
- `cargo fmt --all -- --check`: clean. `vesper fmt --check tests/fixtures/tensor_types`: all 44 fixtures canonical.
- See the PR for the workspace test and clippy results.

## Decisions

- **Erasure plus dimension passing.** The core checker, Place IR, runtime and
  backends see only core syntax. Tensor types are erased to opaque handles and
  dimension parameters become hidden `u64` parameters, so symbolic dimensions
  (§28.1 `Seq`) have run-time values without any change to frozen schemas.
- **Tensor operations are host effects.** The core language has no arrays, so
  operations are operations of built-in linear effects serviced by the host,
  like the design's `linear effect KvCachePool`. Functions that use them write
  `eff [Tensor]` (and `KvCachePool[..]`).
- **`wire` is not recorded in the AST** (the item schema is frozen): a wire
  struct is a copy struct whose fields use wire-only types, and every struct
  used as a dtype must have a checked block layout.
- **Shape literals and products** are encoded as the unspellable type names
  `[]` and `*`; decimal type arguments use PR #379's encoding.
- **KvPageLease on the host** is a one-page `vesper_tensor::kv::KvPool`
  sequence capped at 16 tokens, so pages and sequences share one pool through
  APIs already on `main`.
- Dimension products are computed at run time with `wrapping_mul`; the host
  re-checks every buffer against its dimensions, so a wrapped value is a typed
  error, never a wrong read.

## Remaining gaps

- **check.rs hook not yet wired.** The contract puts this lane's `src/check.rs`
  hook after PRs #379 and #375. #379 has merged, #375 is open. Until the hook
  lands, `vesper::tensor_types::check` / `analyze` is the entry point (used by
  the tests); `check::check` and the CLI do not yet route tensor modules to it.
  The hook is one call at the top of `check::check`.
- No CLI command runs the evaluator; `vesper run` stops at the unhandled
  `Tensor` effect.
- Tensors in `impl`, trait and user effect items, closures returning tensors
  through generic positions, and user effects with dimension parameters are
  rejected (E0770 or E0764).
- No native or Wasm lowering of tensor operations; no egglog fusion (lane 11)
  and no Lean proofs of the shape rules or KV conservation in this lane.
- `StaticTensor`/`TensorView[r, ..]` region-backed views, named axes
  (`[Batch: dim, ...]`) and `LinearSeq[KvPageLease]` inside `ActiveKvSeq` are
  not implemented; `ActiveKvSeq` owns its pages through the pool.
- Rust ownership in the host evaluator is not evidence that the source typing
  rules are implemented; the rules are enforced by the checker and tested by
  the fixtures. Nothing here is a theorem about the host kernels.
