# Port gaps wave: the compiler gaps the ports measured

> Plan written 2026-11-18 against `main` after the port parity wave (lanes
> 1-7, #949-#955, all merged). The [manuscript](../../manuscript/README.md) is
> the sole specification. Confirm live ownership on the issue board before
> acting.

The [port parity wave](../port-parity/README.md) put the Stage 1 speedups
into the seven ports. The [scoreboard](../../parallel-results/port-parity.md)
has the numbers: Aura beat Rust (0.42x-0.85x) and is now Vesper only by the
owner's choice; warp-tcp's codec is at 0.90x Rust's instructions; the rest are
1.96x-5.5x. Each lane named what only the compiler can close, with a repro.
This wave closes those gaps.

## Already claimed elsewhere

#972 (PR #983, open) owns the largest gap: scalar locals in registers, no
double zeroing of stack slots, struct literals without per-field temporaries,
narrow-integer masks, slice-parameter inlining and a new inliner cost with a
budget of 96. Its follow-up covers the leak of a `let`-bound local struct that
holds a `ChunkSeq`. No lane here duplicates that work. Lanes 2, 5, 6 and 7
wait for #983 where they touch its files.

## Lanes

Claims are #984-#992 (lane n is issue 983+n).

| Lane | Prompt | Claim | Gap | Ports | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [Sequence runtime](01-seq-runtime.md) | #984 | two allocations and a thread-local map entry per sequence | Quarry 21%, Prism, Aura, Ignis | now | `src/seq/native_runtime.rs`, `src/seq/aot_runtime.rs`, `reserve` hunks, `tests/native_seq_runtime*.rs` |
| 2 | [Drops](02-drops.md) | #985 | dropped handles, replaced fields, nested sequences, temporaries leak | Aura, Quarry | now (fip.rs); native hunks after #983 | drop elaboration in `src/fip.rs`, drop hunks of `src/native.rs` and `src/seq/native.rs`, `tests/native_drop*.rs` |
| 3 | [In-place borrows](03-in-place-borrows.md) | #986 | E0301 on disjoint fields; no `copy_within`; no `swap`/`replace` through `inout` | Aura, Quarry | now | E0301 builtin-receiver checks in `src/seq/check.rs`/`src/strings/check.rs`, the E0305 hunk of `src/check.rs`, the new builtins, `tests/borrow_paths*.rs`, `tests/native_copy_within*.rs` |
| 4 | [Byte classes](04-byte-classes.md) | #987 | no class scan, ASCII case fold, or two-needle find | Prism about 30%, Quarry | now | new builtins beside `find_byte` in `src/seq/{check,eval,native_bytes}.rs`, byte hunks of `src/native_vector.rs`, `tests/native_byte_classes*.rs` |
| 5 | [Bit builtins](05-bit-builtins.md) | #988 | no `leading_zeros`/`trailing_zeros`/`count_ones`; variable shifts raise | warp-wheel, BBR, Beacon | now (spec, typing); native after #983 | new integer builtin hunks in `src/integer_widths/check.rs`, `src/check.rs`, `src/runtime.rs`, `src/native.rs`, `tests/bit_builtins*.rs` |
| 6 | [Inliner reach](06-inliner-reach.md) | #989 | generic, handled and `Str`-parameter callees stay calls | Ignis, BBR, Quarry, warp-wheel | after #983 | `src/ir_inline.rs`, `src/ir_inline/**`, `tests/native_inline*.rs` |
| 7 | [Codegen redundancy](07-codegen-redundancy.md) | #990 | variants copied twice, constant aggregates unfolded, fused read compares twice, headers reloaded in loops | all | after #983 | aggregate/variant hunks of `src/native.rs`, fused-read hunks of `src/seq/native.rs`, `src/bounds_hoist.rs`, `tests/native_aggregates*.rs`, `tests/native_hoist*.rs` |
| 8 | [Export boundary](08-export-boundary.md) | #991 | `@export` read from the wrong file (bug); `Option`/`Result` convert; no `SeqRef` slice | BBR 8.5%, Aura, Quarry, Prism | now | directive lookup hunks, `src/native_export/**`, `crates/vesper-port-abi/**`, `tests/native_export*.rs` |
| 9 | [Re-score](09-rescore.md) | #992 | adopt the new builtins in the ports and re-measure all seven | all | after #983 and lanes 1-8 | `crates/vesper-<port>/**`, `docs/parallel-results/port-parity.md` |

**Start now:** lanes 1, 3, 4 and 8, lane 2's drop elaboration and lane 5's spec and typing.
**After #983:** lanes 6 and 7, and the native hunks of lanes 2 and 5.
**Recommended merge order:** 8 (the directive bug) first, then 1, 2, 3, 4, 5, 6, 7 as they are green, and 9 last.

### Not in this wave

- Anything #972 owns (above).
- Two-phase borrows (Aura gap 5, E0410 when a receiver loan overlaps a read
  of the same sequence in an argument). It has no run-time cost and is a
  language decision for the owner.
- Wider vectors (AVX2): Cranelift 0.125 has no 256-bit types.
- Inlining into `vesper build` executable objects (verifier policies, #944).
- The self-hosted Stage 1 checker (`packages/vesper-compiler`). Lane 3 makes
  the seed checker accept more programs; the self-hosted checker may stay
  stricter and is out of scope under the Stage 1 guard.
- Re-recording `bootstrap/stage1.lock`.
- Changing any port's default build or deleting Rust code.

## Rules for every lane

The [Stage 1 performance wave's rules](../stage1-perf/README.md#rules-for-every-lane)
apply unchanged: claim first, the Stage 1 guard (no edits to
`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**`; new passes on the native
optimize path only unless they are correctness fixes; verified digests equal
to the lock), semantics first (interpreter, JIT and AOT agree, with
differential tests), leak tests for runtime helpers, the smallest manuscript
change for any new builtin, `cargo +1.100.0 clippy` before pushing, long runs
in GitHub CI, merging on the four required checks. In addition:

- **Measure the repro and the ports.** Each lane prompt names the repros from
  the port reports. Report them before and after, and re-measure the named
  ports with their committed ABBA benchmarks (local port rewrites allowed for
  new builtins; lane 9 commits them).
- **Report.** `docs/parallel-results/port-gaps-<lane slug>.md`.
- **Coordinate with #972.** If PR #983 or its follow-up already closes part
  of a lane's gap, say so in the report and skip it.

## Collisions

| Shared file | Lanes | Rule |
|---|---|---|
| `src/native.rs` | #983, then 2 (drop hooks), 5 (bit builtins), 7 (aggregates) | Wait for #983. Edit only your hunks; merge `main` before merging. |
| `src/seq/native.rs` | #983, then 2 (drops), 7 (fused reads) | Wait for #983. 2 before 7. |
| `src/seq/check.rs`, `src/seq/eval.rs`, `src/seq/native_bytes.rs` | 1 (`reserve`), 3 (`copy_within`), 4 (scans) | Separate hunks; merge `main` before merging. |
| `src/check.rs` | 3 (E0305), 5 (integer builtins) | Separate hunks. |
| `src/ir_inline.rs` | #983, then 6 | 6 after #983. |
| Manuscript §13 and integer kinds | 1, 3, 4, 5, 8 | Separate hunks; each flagged in its PR. |
| Root `Cargo.toml`/`Cargo.lock` | any lane adding a dependency | Serialize. |
