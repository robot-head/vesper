# Port speed lane 6: short byte scans, loop-carried scan constants, in-place ASCII lowercase

Claim #1026. Base `72209eb` (merged up to `3c075a4`, which adds only port
sources and docs). Prompt:
[06-short-scans.md](../agent-prompts/port-speed/06-short-scans.md).

**Result.** Byte scans of fewer than 16 bytes now run one vector step, so
`find_not_in`, `find_byte`, `find_either`, `eq` and `eq_ignore_ascii_case`
cost the same at every length from 1 to 15 bytes: a 4-byte `find_not_in`
drops from 151 to 98 instructions per call and a 14-byte case-folded
compare (`Content-Length`) from 377 to 115. The scan loops build their vector
constants once per call instead of once per step, so each further 16-byte
step of `find_not_in` costs 25 instructions instead of 39. The new
`s.make_ascii_lowercase(lo, hi)` (and `try_make_ascii_lowercase`) lowercases
part of a sequence in place. Prism goes from **1.22x to 1.15x** Rust's
instructions with no port change. Quarry stays at 1.58x.

## What changed

All of it is on the native optimize path (`operands_only`) for x86-64 and
AArch64, in `src/seq/native_bytes.rs`. Unoptimized builds, and so the Stage 1
corpus, emit the same code as before.

1. **Short scans.** When `len < 16`, a scan does one 16-byte load from the
   data pointer, computes its match lanes as the loop body does, and masks
   the bitmask to `(1 << len) - 1`. A two-sided `eq` loads both sides and
   checks that the masked equal lanes are all of them.
   - **Why the read cannot fault.** The load runs only when `len > 0`
     (the data pointer of an empty view may be null) and when
     `p & 4095 <= 4080` for every pointer it reads, so the 16 bytes stay in
     the 4 KiB block of the first scanned byte. That byte belongs to the
     sequence, so its page is mapped and readable. Memory protection is per
     page, and every page size of x86-64 and AArch64 is a multiple of 4 KiB,
     so the load touches no other page. A view that starts within the last 15 bytes of a
     4 KiB boundary (about 15 starts in 4,096) takes the scalar loop. The
     bytes past `len` may belong to another allocation or another thread, but
     no result depends on them: they are masked off, and nothing is written.
   - The sequence layout is unchanged (lane 1's area). Freestanding targets
     (`OperatingSystem::None_`) keep the scalar loop, since an image without
     an MMU may map devices next to RAM.
2. **Loop-carried constants.** Cranelift's egraph rematerializes `iconst`
   and the ALU ops on it next to each use, and the vectors built from them
   followed: inside `find_not_in`'s loop the table load and lane insert, the
   bit row, the `sevens`, and the `high` splat (`xor`/`neg`/`vmovd`/
   `vpbroadcastb`) ran on every step, and `end = len & -16` was recomputed in
   the header. The scan loops of `find_*`, `eq`, `eq_ignore_ascii_case` and
   the lowercase copy now pass `end` and their vectors as loop header
   parameters. The back edge passes `bor(v, v)`: Cranelift removes a block
   parameter whose every argument is one value before the egraph runs, and
   the egraph then folds `bor(v, v)` back to `v`, so the values stay in
   registers and nothing is added to the loop. The constants are built only
   on the path that runs the loop, so short scans do not pay for them.
3. **In-place ASCII lowercase.** `s.make_ascii_lowercase(lo, hi)` on a
   `ChunkSeq[u8]` or `Slice[u8]` maps `A`-`Z` to `a`-`z` in `s[lo..hi]`. It
   checks `lo <= hi <= s.len()` once and raises `IndexOutOfBounds` before
   writing otherwise; `try_make_ascii_lowercase` yields `false` instead.
   Native code reuses the lowercase copy with the destination equal to the
   source (lowercasing is idempotent, so the overlapping last step is
   exact). The copy itself (also used by `extend_ascii_lowercase`) now does
   runs of 4 to 15 bytes as two overlapping 8- or 4-byte SWAR words, read
   before either is written, which never touch a byte outside the range. The
   evaluator (`src/seq/eval.rs`) is the reference. Manuscript §13 gains one
   sentence after the byte-class builtins (flagged in the PR).

The new `SeqOp::MakeAsciiLowercase` adds one line to the "returns above"
arm of `src/seq/native.rs` (lane 1's file) and to the refusal arm of
`src/seq/lean.rs`; both match on every operation.

## Repros (instructions per call)

Callgrind `Ir` of an executable from `vesper build` that calls the scan
2,000 times on a view at a varying start of a 128-byte sequence, minus the
same program with no calls, divided by 2,000. Each number includes the
call's view, `Option` and accumulator code (about 70 instructions), the same
in both columns. Inputs are all in the class, or hold no needle, so every
scan runs to the end. `eq_fold` compares an uppercase view with a lowercase
one, equal under folding.

| Scan | Length | Before | After |
|---|---:|---:|---:|
| `find_not_in` (tchar) | 1 | 91 | 98 |
| | 4 | 151 | **98** |
| | 8 | 231 | **98** |
| | 11 | 291 | **98** |
| | 15 | 371 | **98** |
| | 16 | 103 | 105 |
| | 17-31 | 142 | **130** |
| | 32 | 135 | **125** |
| `find_byte` | 1 | 81 | 78 |
| | 4 | 111 | **78** |
| | 15 | 221 | **78** |
| | 16 | 84 | 85 |
| | 17-31 | 104 | 99 |
| `find_either` | 1 | 87 | 80 |
| | 4 | 132 | **80** |
| | 15 | 297 | **80** |
| | 16 | 88 | 87 |
| | 17-31 | 111 | 103 |
| `eq_ignore_ascii_case` | 1 | 103 | 113 |
| | 4 | 167 | **115** |
| | 14 | 377 | **115** |
| | 16 | 114 | 115 |
| | 17-31 | 154 | **145** |
| | 32 | 144 | 137 |

Lengths 2-15 are flat in the after column (every length was measured; the
table leaves some out). The second vector step of `find_not_in` (16 to 17
bytes) costs 25 instructions after, 39 before. A 1-byte case-folded compare
is 10 instructions slower and a 16-byte scan 1-2 slower (the extra length
test). Everything else is faster.

The constants: in the x86-64 code of the 32-byte `find_not_in`, the loop is
now `vmovdqu`, the class lookup (`vpsrlw`, `vpand`, `vpaddusb`, `vpshufb`
twice, the second through `vpand`/`vpaddusb`), `vpand`, `vpcmpeqb`,
`vpcmpgtb`, `vpand`, `vpandn`, `vpmovmskb`, `test`, `jne`, `add`, `jmp`, plus
the header's compare. The table, bit row, `sevens`, zeros and `high` splat
are built once before it. The `vpaddusb` with a constant-pool operand is
Cranelift's own `swizzle` lowering (indices of 16 or more must read 0).

In-place lowercase, the same harness (a lowercase of `s[off..off + n]` per
call), against the per-byte loop Quarry's catalog path uses:

| Length | Byte loop | `make_ascii_lowercase` |
|---:|---:|---:|
| 1 | 62 | 69 |
| 4 | 130 | **71** |
| 8 | 218 | **68** |
| 11 | 284 | **78** |
| 16 | 394 | **77** |
| 32 | 713 | **90** |

## Ports

Setup as in [port-gaps-rescore.md](port-gaps-rescore.md): this cloud x86-64
container, release builds, callgrind `Ir` of the whole process, Prism
`abba measure 20` and Quarry `planner_abba window 20`. "Before" is the port
sources at `3c075a4` built with the release compiler at `72209eb` (plus this
lane's new builtin, which no port uses); "after" is the same sources built
with this branch's compiler, as `VESPER_PORT_COMPILER`.

| Port | Rust Ir | Port Ir, before | Port Ir, after | Ratio before | Ratio after |
|---|---:|---:|---:|---:|---:|
| Prism | 53.31 M | 65.01 M | 61.27 M | 1.22x | **1.15x** |
| Quarry | 19.47 M | 30.80 M | 30.71 M | 1.58x | 1.58x |

Prism gains 5.8% with no source change: its header names, tokens and short
values are now one vector step. Quarry gains 0.3%. Its SQL keywords and
identifiers are short, but the lexer and identifier scans are a small part
of `window 20`.

Quarry local rewrite (not committed; lane 8 decides): the catalog path's
`identifier` checks the word with `find_not_in` on the pool view, and copies
a name with an uppercase byte with `try_copy_within` and lowercases it with
`try_make_ascii_lowercase`, instead of two per-byte loops. All Quarry port
tests pass with it, and `window 20` measures 30.72 M, the same as without
it: the workload loads its catalog once. The patch is in the project files
(`port-speed/quarry-identifier-in-place-rewrite.patch`).

Wall clock was not measured; at this size the ABBA intervals of earlier
lanes overlap, and the instruction counts are the firmer result.

## Tests

- `tests/native_short_scans.rs`: each short scan against a Rust model at
  every start of a buffer longer than a page (4,128 starts, so some views start
  in the last 15 bytes of a 4 KiB block and take the scalar path) and every length
  0..=17, with needles planted every 23-37 bytes so the bytes past a view
  would change the result if they were not masked. `eq` and its folded form
  compare against a second buffer at the same and at a shifted start. The
  evaluator runs a 40-start sweep; both JIT configurations and an AOT
  executable run the whole one. A CLIF check: optimized hosted objects for
  x86-64 and AArch64 have the 4 KiB test, unoptimized ones do not. Removing
  the mask makes the `find_*` tests fail.
- `tests/native_ascii_in_place.rs`: every interval of a 63-byte buffer at 20
  starts and lengths 0..=40, lowercased in place by the checked form, the
  total form and through a `Slice` that starts at byte 1, with the whole
  buffer hashed after each; all 256 byte values; out-of-range intervals
  (total form `false` with nothing written, checked form raising); `E1525`,
  `E0207` and `E0200`.
- The earlier byte tests (`native_byte_classes`, `native_simd_bytes`,
  `native_bytes_builtins`, `native_bytes`) pass, and so do the Prism and
  Quarry port tests built with this compiler.
- `cargo +1.100.0 clippy -p vesper --all-targets -- -D warnings` is clean.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changes. The new
code runs only when `operands_only` holds, and the Stage 1 corpus is built
unoptimized. The `bootstrap-stage1` workflow run on this branch is linked in
the PR.

## What is left

- `sum_be16` still rebuilds its masks in its inner loop, and has no short
  step (its inputs are packet headers of 20 bytes or more).
- The swizzle lowering's `vpaddusb` with a constant-pool operand stays in
  the `find_not_in` loop. Masking the row index to 0-15 and testing the high
  bit apart would avoid it, at the cost of a different `high` handling.
- A view that starts in the last 15 bytes of a 4 KiB block takes the scalar loop. A load
  of the 16 bytes that end at `len` would also be safe there.
- `find_not_in` is still scalar on the SSE2-only x86-64 target.
- A 1-byte case-folded compare is 10 instructions slower than the scalar
  loop was; a `len >= 2` test would fix it at the cost of one compare.
