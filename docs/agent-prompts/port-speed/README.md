# Port speed wave: the compiler gaps left after the port gaps wave

> Plan written 2026-11-18 against `main` at `72209eb`, after the port gaps
> wave (lanes 1-8, #984-#991, all merged) and its re-score (lane 9, #992).
> The [manuscript](../../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

The [port gaps re-score](../../parallel-results/port-gaps-rescore.md) has the
numbers: Aura beats Rust (0.86x instructions, 0.28x-0.54x wall clock), the
warp-tcp codec is at 0.72x instructions, and the rest are 1.22x (Prism) to
2.24x (BBR) instructions, warp-wheel 1.77x wall clock. Its "Remaining
compiler gaps" table names each gap with a repro. This wave closes the ones
the compiler can close without AVX2.

## Lanes

Claims are #1021-#1028 (lane n is issue 1020+n).

| Lane | Prompt | Claim | Gap | Ports | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [Empty sequences](01-empty-seqs.md) | #1021 | a never-pushed sequence allocates a header | Quarry (allocator 43%), Prism, Ignis, Aura | now | header and growth hunks of `src/seq/native.rs`/`src/native.rs`, `src/seq/aot_runtime.rs`, `src/seq/native_runtime.rs`, `tests/native_empty_seq*.rs` |
| 2 | [Write in place](02-write-in-place.md) | #1022 | `q.f = T { .. }` and block-parameter values built in a temporary | BBR, Ignis, Quarry | now | `plan()`/home and `Write` hunks of `src/native.rs`, `tests/native_write_in_place*.rs`, `tests/native_block_param_homes*.rs` |
| 3 | [Parameters and hoisting](03-params-and-hoisting.md) | #1023 | `in` aggregate parameters spilled; one hoisted header per calling loop | warp-wheel, BBR, Prism, Aura, Ignis | now | entry-parameter hunk of `src/native.rs`, `src/bounds_hoist.rs`, `tests/native_in_params*.rs`, `tests/native_hoist_calls*.rs` |
| 4 | [Inactive payload](04-inactive-payload.md) | #1024 | inactive variant payload words zeroed | Ignis, Quarry, BBR | now (rule, readers); construction after lane 2 | variant-construction zeroing in `src/native.rs`, payload readers, enum layout in the manuscript, `tests/native_inactive_payload*.rs` |
| 5 | [Inliner cost](05-inliner-cost.md) | #1025 | `Size` refusals of 100-700-unit callees; no partial inlining | BBR, Ignis, warp-wheel, Quarry | now | `src/ir_inline.rs`, `src/ir_inline/**`, `tests/native_inline_cost*.rs` |
| 6 | [Short scans](06-short-scans.md) | #1026 | scans under 16 bytes are scalar; vector constants rematerialized; no in-place lowercase | Prism, Quarry | now | scan hunks of `src/seq/native_bytes.rs` and `src/native_vector.rs`, the new builtin in `src/seq/{check,eval}.rs`, `tests/native_short_scans*.rs`, `tests/native_ascii_in_place*.rs` |
| 7 | [Range facts](07-range-facts.md) | #1027 | `x << s` under `if s < 64` still raises | warp-wheel, BBR, Beacon | now | fact hunks of `src/check.rs` and `src/integer_widths/check.rs`, `tests/range_facts*.rs` |
| 8 | [Re-score](08-rescore.md) | #1028 | adopt lanes 1-7 in the ports and re-measure all seven | all | after lanes 1-7 | `crates/vesper-<port>/**`, `docs/parallel-results/port-parity.md`, `docs/parallel-results/port-speed-rescore.md` |

**Start now:** lanes 1, 2, 3, 5, 6 and 7, and lane 4's rule and readers.
**After lane 2:** lane 4's construction hunk.
**Recommended merge order:** 1 (largest measured share) and 7 (smallest) first, then 2, 3, 4, 5, 6 as they are green, and 8 last.

### Not in this wave

- Wider vectors (AVX2): Cranelift 0.125 has no 256-bit types.
- Drops that only matter for correctness, not time (`Box` payloads, partial
  moves, closure captures, `Wild` parts): they need a drops lane of their own.
- `Option`/`Result` struct fields as `OptionMirror` cells and `Fail` record
  decoding at the export boundary: a change to existing mirror structs, with
  BBR's glue as the only measured share.
- Ignis `lower`'s Rust-side `Vec<NativeBackendBarrier>` rebuild: a port
  change (lane 8 or an Ignis port PR), not a compiler gap.
- Inlining into `vesper build` executable objects (verifier policies, #944).
- The self-hosted Stage 1 checker (`packages/vesper-compiler`).
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
change for any new builtin or rule, `cargo +1.100.0 clippy` before pushing,
long runs in GitHub CI, merging on the four required checks. In addition:

- **Measure the repro and the ports.** Each lane prompt names the repros from
  the reports. Report them before and after, and re-measure the named ports
  with their committed ABBA benchmarks and callgrind, with the setup of
  [port-gaps-rescore.md](../../parallel-results/port-gaps-rescore.md) (local
  port rewrites allowed; lane 8 commits them). Wall clock on this kind of
  host moves by up to 30% between runs of one binary, so give instruction
  counts too.
- **Report.** `docs/parallel-results/port-speed-<lane slug>.md`.

## Collisions

| Shared file | Lanes | Rule |
|---|---|---|
| `src/native.rs` | 1 (sequence construction), 2 (homes, `Write`), 3 (entry parameters), 4 (variant zeroing) | Edit only your hunks; 4's construction after 2; merge `main` before merging. |
| `src/seq/native.rs` | 1 (header, growth) | Lane 1 only. |
| `src/seq/check.rs`, `src/seq/eval.rs` | 6 (in-place lowercase) | Lane 6 only. |
| `src/seq/native_bytes.rs`, `src/native_vector.rs` | 6 | Lane 6 only; lane 1 does not change the byte layout scans read. |
| `src/check.rs` | 7 (facts) | Lane 7 only. |
| Manuscript | 4 (enum layout), 6 (§13 builtin), 7 (totality rule, if specified) | Separate hunks; each flagged in its PR. |
| Root `Cargo.toml`/`Cargo.lock` | any lane adding a dependency | Serialize. |
