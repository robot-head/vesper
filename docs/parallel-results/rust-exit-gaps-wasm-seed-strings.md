# Rust exit gap wave 1, lane 9: wasm seed strings (#1506)

Status: part 1. Slices, `Str`/`String` bytes, `ChunkSeq` and the integer
conversions lower in the Vesper wasm backend (`packages/vesper-wasm-backend`).
A compiler package does not build yet: it needs `Fail` handlers (below).

Base: `origin/main` 76a27738. Prompt:
docs/agent-prompts/rust-exit-gaps-1/09-wasm-seed-strings.md.

## Deviation from the prompt: port only

The prompt says to keep `src/wasm_backend` and its port in step. The
coordinator agreed (2026-11-22) to do this work in the port only, as #1492
did for loops. The Rust backend keeps refusing these programs with
`Unsupported(Operation)`, so the Stage 1 compiler is untouched and the seed
(SH6 lane 2, #1397) gets the breadth from the hybrid, which serves the
wasm backend from the port.

## Repro

Before this PR, every program with an `Op::Seq` (any `ChunkSeq`, slice or
string operation) or an integer conversion (`widen_u64(x)`,
`wrapping_u8(x)`, `narrow_u32(x)`) was refused by both backends:

```text
fn n(in b: Slice[u8]) -> u64 { b.len() }
fn m(in s: Str) -> u64 { s.len() }
fn main() -> u64 { let mut s = ChunkSeq::new[u8](); s.push(3); let t = "hi";
    let mut u = String::new(); u.push_str(t);
    wrapping_add(wrapping_add(n(s.as_slice()), m(t)), wrapping_add(m(u), n(t.as_bytes()))) }
```

Rust: `unsupported Operation: IR operation unknown to the Wasm backend`;
port: `Unsupported(Operation)` site 200. Now the port compiles it and it
returns 7, as the evaluator does.

## The fix

Representation (all values stay one `i64`):

- A `ChunkSeq[T]` or `String` is a pointer to a buffer: one word
  `len | cap << 32`, then the elements, one byte each for `u8` and for
  strings, eight bytes otherwise. A string literal keeps Rust's static
  layout (`[len, bytes]`), which reads as capacity 0, so the first write
  copies it and the literal never changes.
- A `Slice[T]` or `Str` view is `addr | len << 32`. Views are second class
  (only `in` arguments), so a view never outlives its buffer.
- A buffer has one owner (`ChunkSeq` moves, E0303), so writes happen in
  place. Growth (`$seq_reserve`: capacity `max(need, 2 cap, 4)`) moves the
  buffer and writes the new pointer back to the receiver's place, inside
  an `if` so a field place is rewritten only on growth.
- A `String` argument to a `Str` parameter is converted to its whole view at
  the call (`call_args`).

Operations lowered: `New`, `WithCapacity`, `Len`, `Push`, `Pop`, `Get`,
`Set`, `TryGet`, `Last`, `TrySet`, `Clear`, `Truncate`, `Reserve`, `Extend`,
`Finish`, `AsSlice`, `SubSlice`, `SliceLen`, `SliceGet`, `SliceSet`, `Swap`,
`Replace` (element views too), `BytesEq`, `Fill`, all ten `StrOp`s
(`SubStr` checks char boundaries, `PushChar` encodes UTF-8 and rejects
surrogates), and every `Wide` access (`try_read_u64_le`, `write_u32_be`, ...).
`Atomic` operations (src/atomics.rs), whose prelude bodies recurse so that
every backend intercepts them, become plain accesses of the cell (wasm runs
one thread); `compare_exchange` is refused (site 237). Before this, the
port compiled `tests/fixtures/scoped_workers/parallel_sum.ves` into a
module that overflowed its stack: the port differential found it.
Checked accesses raise `Fail[RuntimeError]::IndexOutOfBounds`, as the
evaluator does; a view interval out of range traps (the evaluator's fault).
`==` with a `Str` operand compares bytes. Integer `widen_k`, `narrow_k`
(raises `ArithOverflow`) and `wrapping_k` lower between `i64`, `u64`, `u32`
and `u8`.

Byte identity with Rust is kept: the program scan sets `seqs` only when the
pool has an `Op::Seq`, and only then do `$str_eq` and the string evacuation
copy mask the capacity bits. Programs Rust lowers have no `Op::Seq`, so their
modules are unchanged.

Both differentials now treat any `Unsupported(Operation)` refusal from Rust
that the port compiles as an extension (run against the evaluator in the
port differential), not a difference: loops, sequences and conversions.

## Numbers

- tests/wasm_seed_breadth.rs: 13 programs (seq growth, strings, sub-strings,
  UTF-8, wide reads, inout growth, struct fields, swap/replace, sequences of
  structs, literal copy-on-write, atomics) run under Wasmtime equal to the evaluator.
  Rust refuses all 13.
- tests/self_host_wasm_backend.rs: `loops_run_like_the_evaluator` and
  `hook_artifact_matches_rust` pass. Port differential and hybrid
  differential: see the PR (run after merging main).

There is no Rust baseline to compare speed against: Rust compiles none of
these programs.

## Stage 1 guard

`git diff origin/main -- src lean bootstrap packages/vesper-compiler` is
empty: this PR changes no compiler source, so `bootstrap/stage1.lock` is
unaffected.

## New host imports

None. Lane 2 (#1397) needs no runner change.

## Gaps found

1. `main` cannot have effects ("entry must handle all effects"), and
   handlers (`Op::Handle`) are not ported (part 2 of #1289). Checked
   indexing (`s[i]`, `byte_at`, `read_u64_le`) needs `Fail`, so the tests use
   the total forms, and compiler packages, which use `Fail` everywhere, do
   not build yet. Minimal repro:
   `fn main() -> u64 { handle Fail[RuntimeError] in { 1 } with { return v => v, raise(e, k) => 0 } }`
   is `NotPorted` (site 198).
2. `match` on string literal patterns over a `Str` is rejected by the
   checker (E0200), so `Str` comparison goes through `as_bytes().eq(..)`.
3. Not lowered yet: `CopyFrom`, `CopyWithin`, `SumBe16`, `BytesCmp`,
   `FindByte`, `FindEither`, `FindNotIn`, `EqIgnoreAsciiCase`,
   `ExtendAsciiLowercase`, `MakeAsciiLowercase`, `Vector`, `Resize`,
   `FillRange`, `ExtendFromWithin`, `ExtendReversed`. They raise
   `Unsupported(Operation)` site 233.
4. An element view passed as an `inout` call argument (`f(inout s[i])`)
   writes back through the receiver's place, not the element.
5. Float to integer conversions are refused (site 194).

## What is left

- Part 2: lower `Fail` handlers (`Op::Handle`), then build one compiler
  package (the hybrid's `packages/vesper-place-ir` reader is the first
  candidate: slices, strings and `ChunkSeq` throughout).
- The operations in gap 3, as the package build finds them.
- Input on `main` (`cabi_realloc`) is lane 10 (#1508).
