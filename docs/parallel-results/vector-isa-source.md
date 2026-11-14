# Wave seven, lane 28: source vectors and `eff [Isa[P]]` (§14)

Claim: #351. Branch `claude/project-thread-yq3tj6`, based on `main` at
`283c811` (a descendant of the required baseline `c025b9a`). Root package and
one additive syntax hunk; no new dependencies, no root manifest change.

## What was built

Fixed-width source vectors with a checked ISA capability, evaluator semantics
for every operation, and Cranelift SIMD lowering differential-tested against
the evaluator. The JSON string encoded-size computation of vesper-serde's host
encoder is ported to source as a 128-bit and a 256-bit kernel behind a checked
dispatch.

| Path | Content |
| --- | --- |
| `src/vector.rs` (new) | Types (`Elem`, `Shape`, `Kind`), profiles and their lattice, the intrinsic table, the IR operation `VectorOp`, the `Isa` discipline pass `check_world`, ISA-flag dispatch (`isa_enabled`, `host_enabled`), native byte layout and decoding |
| `src/vector/check.rs` (new) | Typing of the intrinsics; a child module of `check` |
| `src/vector/eval.rs` (new) | Reference semantics of every operation |
| `src/native_vector.rs` (new) | Cranelift SIMD lowering; a child module of `native` |
| `src/ir.rs` | Additive `Op::Vector { op, args }` and its lowering from a reserved path call |
| `src/runtime.rs`, `src/ir_check.rs`, `src/ir_opt.rs` | One `Op::Vector` arm each (evaluate; release operand loans; rename operands) |
| `src/native.rs` | `mod native_vector;` and two dispatch arms (Place IR and the expression path used by specialized closures) |
| `src/check.rs` | Hooks: `mod vector_check`, vectors are `copy` in `World::mode`, vector types in `valid_type`, the intrinsic call in `Checker::call`, `vector::check_world` after `capability_effects::check_world` |
| `src/capability_effects.rs` | `Isa` registered as an erased capability effect (`is_capability_effect`), with its own row-entry diagnostic |
| `src/layout.rs` | Native layout and decoding of vectors (packed lane bytes as words) |
| `crates/vesper-syntax/src/parser.rs` | Decimal lane counts in type arguments (`Vec[u8, 16]`), encoded as an all-digit type name no identifier can spell |
| `tests/vector.rs`, `tests/vector_kernel.rs` (new) | 18 tests |
| `tests/fixtures/vector/json_escape.ves` (new) | The ported kernel, canonically formatted |

### Types

`Vec[T, N]` holds `N` lanes of `T`; `Mask[T, N]` holds one boolean per lane of
the same shape. `T` is one of the language's scalar types `u8`, `u32`, `u64`,
`i64`, `f64` (the language has no `i8`, `u16`, `i16`, `i32` or `f32`), and the
vector is 128, 256 or 512 bits wide: 15 shapes. Both are `copy`, have no
source fields and need no profile to be passed, returned, bound or stored.
Other spellings are E1401. A two-parameter `Vec` or `Mask` declaration is
rejected (E1401) so the names stay unambiguous.

### Operations

Reserved path intrinsics, all total and by value:

| Group | Operations |
| --- | --- |
| Construction | `Vec::splat[T, N](x)`, `Vec::from[T, N](x0, ..)` (the shape may also come from the expected type) |
| Lane arithmetic | `add`, `sub`, `mul` (not `u8`), `div` (`f64` only), `min`, `max`, `and`, `or`, `xor`, `not`, `shl`, `shr` (integers), `saturating_add`, `saturating_sub` (`u8`) |
| Comparison | `eq`, `ne`, `lt`, `le`, `gt`, `ge` produce a `Mask`; `Vec::select(mask, a, b)` |
| Masks | `Mask::and`, `or`, `xor`, `not`, `count`, `bitmask`, `any`, `all`; `Mask::active[T, N](offset, len)` (the design's `P::active_mask`) |
| Lanes | `Vec::extract(v, i)`, `Vec::replace(v, i, x)`, `Vec::shuffle(v, i0, ..)` with literal, range-checked lane indices |
| Reductions | `Vec::reduce_add`, `reduce_xor`, `reduce_or`, `reduce_and`, `reduce_min`, `reduce_max` |
| Strings | `Vec::load_masked[u8, N](s, offset)` reads bytes of `s`, zero past its end; `Mask::in_bounds[u8, N](s, offset)` marks the lanes it read |
| Dispatch | `Isa::supported[P]() -> bool` |

Semantics (`src/vector/eval.rs`): integer lanes wrap at their width; shifts
take the amount modulo the lane width; signed comparisons and `shr` for `i64`,
unsigned for the others; float lanes are IEEE binary64, comparisons are
ordered except `ne`, and `min`/`max` return NaN when either lane is NaN and
order -0.0 below +0.0 (Cranelift's `fmin`/`fmax`); reductions fold in lane
order; a string load past the end or at an overflowing offset reads zero
lanes and never faults. Typing errors are E1402 (unknown operation, wrong lane
type, non-literal or out-of-range lane index, `bitmask` over 64 lanes), E0207
(arity, stray type arguments), E0200 and E0209 (operand types and literal
ranges), E0308 (non-value passing).

### `Isa[P]`

`Isa` is a reserved, stateless capability effect with one profile argument:
`Baseline128`, `Avx2`, `Avx512`, `Sve2`, `Rvv1` (anything else is E1404). Like
the lane-15 capability effects it is erased: no handler can discharge it
(E0701), no declaration may redefine it (E0100), and it is never requested
from the caller's row. Its own rules, checked by `vector::check_world` on the
surface HIR after checking (E1403):

- A vector operation needs a profile in scope whose fixed width holds the
  shape: 128 bits any profile, 256 bits `Avx2` or `Avx512`, 512 bits
  `Avx512`. The scalable profiles grant 128-bit fixed vectors only.
- A call of a function, or of a function value whose type carries `Isa[Q]`,
  needs a profile `P` in scope with `Q ⊑ P` in `Baseline128 ⊑ Avx2 ⊑ Avx512`,
  `Baseline128 ⊑ Sve2`, `Baseline128 ⊑ Rvv1`.
- In scope are the profiles of the function's row, plus `P` inside the
  then-branch of the checked dispatch `if Isa::supported[P]() { .. } else
  { .. }` (only that exact condition grants it). A closure body holds only
  the profiles of its own declared row, because it can run elsewhere.

`Isa::supported[P]()` is true when the code generator of this execution
targets `P`. The native backend folds it to a constant from the Cranelift ISA
flags it compiles with (the host's for the JIT, the named target's for object
emission); the evaluator asks the same host ISA builder
(`cranelift_native`), so evaluator and JIT select the same branch on one
host. `Baseline128` holds on every x86_64 and AArch64 target (SSE2 and NEON
are architectural and Cranelift lowers every operation with them alone),
`Avx2` needs `has_avx2` and `has_fma`, `Avx512` needs `has_avx512f` and
`has_avx512vl`; `Sve2` and `Rvv1` are never supported because Cranelift 0.125
cannot generate SVE2 or RVV code. Native entry points still may not carry
effects, so `main` reaches vector code only through the dispatch.

### Lowering

`Op::Vector` is an additive Place-IR operation with evaluator semantics for
every intrinsic; its operands are the call's non-literal arguments, read as
`in` arguments (copy lanes, a shared string borrow). It is opaque to the
effect optimizer (a barrier, so read forwarding never crosses it). Lean CFG
extraction and the other IR consumers reject it through their existing
wildcard arms ("unsupported operation"), as the wave-seven contract requires.

Natively a vector or mask is an aggregate: a pointer to its packed lane bytes
in a stack slot, so parameters, results, block parameters and copies reuse the
existing aggregate ABI. Each operation loads one, two or four 128-bit
Cranelift vectors (`i8x16`, `i32x4`, `i64x2`, `f64x2`), computes, and stores
a fresh result. Comparisons produce all-ones lanes; `bitmask`, `count`,
`any` and `all` use `vhigh_bits`; `shuffle` of one chunk is a single
`shuffle`; `Mask::active` compares a splatted bound with a lane-index
constant; `load_masked` does a direct vector load when the whole chunk lies
inside the string and a clamped byte-by-byte load with zeroed lanes
otherwise, so it never reads outside the inline string. Cranelift 0.125 has
no 256- or 512-bit vector types, so wider shapes are chunked 128-bit code
under every profile: the AVX2 and AVX-512 profiles are typing and dispatch
distinctions here, not wider machine instructions.

## Ported kernel

`tests/fixtures/vector/json_escape.ves` computes vesper-serde's
`encoded_size(&Value::String(s))`: 2 plus 1 per byte, 2 for `"`, `\`, `\n`,
`\r`, `\t`, `\b`, `\f`, and 6 for any other control byte. Each 16- or 32-byte
chunk is a masked load, eleven lane comparisons, mask algebra and three
counts; the recursion ends when `Mask::in_bounds` has no lane left (the
language has no loop). `json_string_size` dispatches to the 256-bit kernel
under `Isa::supported[Avx2]`, else to the 128-bit one. Tests compare the
evaluator and the JIT with the host encoder itself on 23 inputs (empty,
every control byte, quotes, backslashes, multibyte UTF-8, and lengths around
every 16/32-byte boundary up to 129), on the dispatched path and directly on
the 128-bit path.

## Checks

- `cargo test --locked --test vector --test vector_kernel`: 18 passed (x86_64
  host with AVX2 and AVX-512, so all three profiles ran their vector branch;
  128/256/512-bit differential tests take 6, 9 and 13 s in a debug build).
- `cargo +1.100.0 clippy --locked -p vesper -p vesper-syntax --all-targets -- -D warnings`: clean.
- `cargo fmt --all -- --check`: clean; `vesper fmt` canonicalized the fixture.
- Existing objects unchanged: `vesper emit --emit object` and `--emit clif`
  for all 8 `examples/*.ves` on `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu` are byte-identical between `main` (`283c811`)
  and this branch (32 of 32 artifacts). `programs_without_vectors_emit_no_vector_code`
  also checks that their IR and CLIF contain no vector operation or type.
- The full `cargo test --workspace --locked` run did not finish locally (the
  session's disk allowance ran out while linking test binaries); the four
  required CI jobs run the whole workspace, and the macOS job runs the
  differential tests natively on AArch64.

## Decisions

- **Spelling.** The design writes methods (`chunk.simd_eq(needle)`) and
  `VVec::splat[P, u8]`. Method calls on vectors would need a new receiver
  path through the checker and the trait elaboration; reserved path calls
  (`Vec::eq(a, b)`) reuse the existing call syntax and keep the core
  `ExprKind`/`Type` schema frozen. The only syntax change is the lane count.
- **Isa as an erased capability.** Registering `Isa` through
  `capability_effects::is_capability_effect` gives it the erasure every
  backend already applies to capability effects (no evidence, compiled once
  like a pure function), while its rules stay in `src/vector.rs`. An ordinary
  declared effect would have required evidence passing and handlers.
- **Dispatch by code generator, not CPUID.** One Cranelift module compiles
  with one flag set, so per-function multiversioning would need several ISAs
  in one object. The probe folds at compile time instead, and the evaluator
  reads the same flags. Running a generic-target object on an AVX2 machine
  therefore takes the Baseline128 path.
- **Totality.** No vector operation can fail, so none needs `Fail` evidence or
  native abort paths; the string load is masked instead of bounds-checked.
- **Masks are their own type** (`Mask[T, N]`, lane-width all-ones natively)
  rather than integer vectors, so mask algebra cannot be confused with lane
  arithmetic.

## Remaining gaps

- No length-agnostic `VVec[P, T]`/`VMask[P, T]` and no SVE2/RVV code: Cranelift
  0.125 has neither. `Sve2` and `Rvv1` are accepted profiles whose probe is
  always false.
- No profile polymorphism (`fn k[P: IsaProfile](..) eff [Isa[P]]`): a profile
  argument must be a concrete profile name.
- No 256/512-bit machine instructions (Cranelift has no such types); no
  `mul_add`/FMA, widening multiply-accumulate, conversions between lane
  types, aligned slices or `ByteSlice` (the language has no slices: strings
  are the only byte input).
- No egglog vector saturation (§14.4 stage 4) and no Lean `bv_decide`
  equivalence proofs (stage 5); vector programs are rejected by Lean CFG
  extraction.
- Vector operations inside `ConstTime`, `Realtime` or `DetFloat` bodies are
  rejected by the capability-effect call discipline as undetermined calls.
- No runtime CPUID: dispatch follows the code generator's flags (see
  Decisions).
- These are source typing rules and reference semantics checked by tests;
  nothing here is a proof about the emitted machine code.
