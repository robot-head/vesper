# P3b gaps wave: the compiler gaps the P3a and P3b ports found

> Plan written 2026-11-18 against `main` at `101a3cf`, while the P3b port
> lanes (#1063-#1071) are running. The [manuscript](../../manuscript/README.md)
> is the sole specification. Confirm live ownership on the issue board before
> acting.

The P3a ports (Pact, Loom audit, Bench, eBPF, silicon, Aegis) and the P3b
ports and compiler lanes (Warp, Prism H3, Chorus, Flux, Kinetic, Resonance,
dyn closures, loop-owned boxes, Bridge handles) each wrote a "Compiler gaps"
section with repros in `docs/parallel-results/port-p3-*.md` and
`port-p3b-*.md`. Several of those reports are still on open port-lane PR
branches (#1073-#1083); read them there. This wave closes the gaps, ordered
by measured port impact:

1. **Speed.** `f32` vectors (Resonance 5x scalar), bounds checks the hoisting
   pass misses (the Bench `add_rank` loop is 97-99% of the port), `inout`
   slice helpers that stay calls (Warp 13% of the frame encoder), copy-struct
   locals in stack slots (eBPF `scalar_normalize` 64%), and the export
   boundary (silicon 23% `memcpy`; `f32` as bits everywhere).
2. **Missing language.** Fixed-size arrays and constant tables (Bench, Warp,
   Prism H3, Aegis), `continue` and literal typing (every port),
   qualified paths and submodule items, and two-phase `inout` arguments
   (Chorus hoists about 140 arguments, Pact about 50).
3. **Correctness.** E0410 on bit builtins assigned to a `let mut`, a native
   hang for a resuming handler around a loop, Box drops on `Fail` exits, the
   `sink`-parameter leak in specialized callees, affine drop imports.
4. **Library.** A sort in `@vesper-std` and bulk sequence operations.

## Lanes

Claims are #1086-#1097 (lane n is issue 1085+n).

| Lane | Prompt | Claim | Gap | Ports | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [f32 vector lanes](01-f32-vectors.md) | #1086 | no `f32` vector lanes: float kernels are scalar (16 `mulss` for a 16-lane multiply) | Resonance (ABBA kernel 5.3x, graph 5.0x), Kinetic, Tensor | now | `Elem::F32` and the f32 hunks of `src/vector.rs`, `src/vector/**` and `src/native_vector.rs`; the slice load/store intrinsics added to the same files; `tests/vector_f32*.rs`, `tests/vector_slice_io*.rs`; the §14.3 hunk of `docs/manuscript/content.typ` that lists lane types |
| 2 | [Export ABI: f32, lent handles and handle collections](02-export-abi-widening.md) | #1087 | `f32` crosses as `u32` bits; `in` handles are copied on every call; `ChunkSeq[Handle]` and `Box[dyn P]` fields are refused | silicon (23% memcpy in boundary calls), Resonance, Kinetic, Tensor, eBPF | now | `src/native_export.rs`, `src/native_export/**` (binding generators, glue, directives); the generated-binding runtime support crate(s) the bindings call, if any; `tests/native_export_f32*.rs`, `tests/native_export_lent_handles*.rs`, `tests/native_export_handle_seqs*.rs`; the export ABI hunk of the manuscript (§13) |
| 3 | [Bounds checks and slice headers](03-bounds-facts.md) | #1088 | `dist[s + rank]` and constant-range accesses are checked every iteration; every `inout` slice header is reloaded after any store | Bench (the `add_rank` loop is 97-99% of the port), Resonance, atomics workers, Prism H3, Kinetic | now | `src/bounds_hoist.rs`, `src/bounds_hoist/**`; the slice-header load hunk of `src/native.rs` (where a slice parameter's {ptr, offset, len} is read) and its alias facts; `tests/native_bounds_facts*.rs`, `tests/native_slice_headers*.rs` |
| 4 | [Inline helpers that take an inout slice](04-inline-inout-slices.md) | #1089 | helpers that take an `inout` slice stay calls (`w_vi` 9% and `vi_put` 4% of Warp's `fr_encode`) | Warp, warp-tcp, Resonance, Prism H3, Kinetic | now | `src/ir_inline.rs`, `src/ir_inline/**`; `tests/native_inline_inout_slice*.rs` |
| 5 | [Copy-struct locals in registers](05-aggregate-registers.md) | #1090 | copy-struct locals live in stack slots and reload each field per use; `Option[Struct]` results go through a temporary; `s.try_get(d).f` copies the whole element | eBPF (`scalar_normalize` 64% of scalar workload), silicon, BBR, Quarry, Kinetic | now | the aggregate-local home and result hunks of `src/native.rs` (not the slice-header hunk, which lane 3 owns); `src/native_aggregates.rs`; the element field-read hunk of `src/seq/native.rs`; `tests/native_sroa*.rs`, `tests/native_result_in_place*.rs`, `tests/native_seq_field_reads*.rs` |
| 6 | [Fixed-size arrays and constant tables](06-fixed-arrays.md) | #1091 | no fixed-size arrays or static data: scratch tables are heap `ChunkSeq`s read through `try_get`, constant tables are built at run time | Bench, Warp (ACK ranges), Prism H3 (Huffman/static tables), Aegis (`[u8; N]` fields), Chorus (module constants) | now (part 1); the export-mirror part after lane 2; merge after lanes 7 and 8 if both touch the parser | new `src/arrays.rs`, `src/arrays/**`; the array type, literal and `const` item hunks of `crates/vesper-syntax/src/parser.rs` (and the lexer only if needed); the array and `const` hunks of `src/check.rs`, `src/layout.rs`, the evaluator, and native lowering in a new `src/native_arrays.rs`; the `[u8; N]` mirror-field hunk of `src/native_export/**` (part 2, after lane 2 merges); `tests/arrays*.rs`, `tests/const_items*.rs`; the manuscript hunks for array types and `const` items |
| 7 | [continue and literal typing](07-loops-and-literals.md) | #1092 | no `continue`; `0..3` and `if c { 1 } else { 2 }` are `i64` against `u64` uses; `?` only on `Result` | every P3 port (Kinetic, Prism H3, Chorus, Pact, Bench, eBPF, Loom audit, Aegis) | now | the loop and `continue` hunks of `crates/vesper-syntax/src/parser.rs`, their HIR lowering and `vesper fmt`/highlighting support; the integer-literal inference and `?` hunks of `src/check.rs` and `src/integer_widths/check.rs`; the `wrapping_neg` builtin wherever the other wrapping builtins live; `tests/continue*.rs`, `tests/literal_typing*.rs`, `tests/try_option*.rs`; the manuscript hunks for these forms (§2.2 grammar) |
| 8 | [Qualified paths and submodule items](08-modules-and-paths.md) | #1093 | submodule functions need unique names, `.vsys` imports and exports only work in the root module, an imported function named `u64` shadows the type | Prism H3, Pact, Kinetic, Flux, Aegis, Chorus | now | package module resolution (`src/packages.rs`, `src/package_graph.rs` and the name-resolution hunks of `src/check.rs`); the path-expression, `use` and pattern hunks of `crates/vesper-syntax/src/parser.rs`; the module hunks of `src/ffi_lowering/**` (where `.vsys` imports are bound) and of `src/native_export/directives.rs` (which module's `pub fn`s are exported); `tests/package_paths*.rs`, `tests/package_submodule_exports*.rs`; the module hunks of the manuscript |
| 9 | [Two-phase inout arguments and bound views](09-argument-borrows.md) | #1094 | `f(inout c, g(in c))` is E0301, so ports hoist reads into locals (Chorus about 140 places, Pact about 50, eBPF throughout) | Chorus, Pact, eBPF verifier, Loom audit, Aegis, Warp | now | the argument-loan reservation hunk of `src/check.rs` (next to `reserved_receivers`); the matching Place IR check hunk of `src/ir_check.rs`; the E1531 hunk of `src/strings/check.rs` and the E1521 hunk of `src/seq/check.rs`; `tests/two_phase_arguments*.rs`, `tests/bound_views*.rs`; the two-phase loan paragraph of the manuscript (§5, "Two-phase receiver loans") |
| 10 | [Drops on every exit and handler fixes](10-exits-and-handlers.md) | #1095 | five correctness bugs the P3b lanes found; ports work around each | Kinetic, Prism H3 (E0410), Chorus, Warp, Kinetic (boxes), Flux/Chorus (handlers), Warp (affine handles) | parts 1-2 now; part 3 after #1076 merges; part 4 after #1077 merges | the bit-builtin rewrite hunk of `src/integer_widths.rs` (`lower_bit_builtin_calls`); the loop/resume hunks of `src/native_handlers.rs`, `src/native_handlers/**`; the `Fail`-exit and specialized-callee `sink` hunks of `src/fip.rs` (after #1076); the affine drop-import hunks of `src/ffi_lowering/**`, `src/fip.rs` and the evaluator (after #1077); `tests/bit_builtin_mut*.rs`, `tests/native_handler_loops*.rs`, `tests/native_fail_exit_drops*.rs`, `tests/native_specialized_sink*.rs`, `tests/ffi_affine_drop*.rs`; the §16.3 affine-handle bullet of the manuscript (part 4) |
| 11 | [std sort and bulk sequence operations](11-std-sort-and-bulk.md) | #1096 | no sort in std; no bulk fill or resize; `extend` cannot read its own sequence; byte-at-a-time reversed pushes; `find_not_in` slow without SSSE3 | Kinetic (sorts), eBPF (interpreter setup), Aegis (2x on SNP), Warp, Chorus, Prism H3 | now | `packages/vesper-std/**` (new `sort` module and helpers); the new `ChunkSeq` builtins in `src/seq/check.rs`, `src/seq/eval.rs` and their native lowering in `src/seq/native*.rs` (not the element field-read hunk, which lane 5 owns); the `find_not_in` fallback hunk of `src/seq/native_bytes.rs`; `tests/std_sort*.rs`, `tests/seq_bulk*.rs`, `tests/native_find_not_in*.rs`; the §13 builtin list and the std catalog hunks of the manuscript |
| 12 | [Adopt the wave in the P3 ports and re-score](12-rescore.md) | #1097 | the scoreboard has no P3 rows measured against one compiler | Resonance, Kinetic, Warp, Prism H3, Chorus, Flux, eBPF, silicon, Pact, Bench, Loom audit, Aegis | after lanes 1-11 merge and after the port speed re-score (#1028) | `crates/vesper-<port>/**` of the P3 ports whose port-lane PRs have merged (coordinate with any open port-lane PR instead of editing its files); `docs/parallel-results/port-parity.md` (P3 section) and `docs/parallel-results/p3b-gaps-rescore.md` |

**Start now:** lanes 1-9 and 11, and parts 1-2 of lane 10.
**After #1076 (P3b lane 2):** lane 10 part 3; lane 3's hoisting in functions with boxes.
**After #1077 (P3b lane 3):** lane 10 part 4.
**After lane 2:** lane 6 part 2 (array fields in export mirrors).
**After lanes 1-11 and #1028:** lane 12.

**Recommended merge order:** lane 10's parts as they are green (correctness
first); then 3, 4 and 1 (largest measured shares); 7 before 8 before 6 (all
three edit the parser; merge `main` between them); 2, 5, 9 and 11 as green;
12 last.

### Spec calls flagged for the owner

- **`while` (decided).** The owner chose to keep `while` out of the core
  and wait for library `sugar` (§7.6), so no lane adds it.
- **Two-phase `inout` arguments (lane 9).** Extends the manuscript's receiver
  rule ("Only receivers are reserved") to every `inout` argument.
- **Arrays and `const` items (lane 6)**, **`f32` lanes (lane 1)**, **export
  ABI widening (lane 2)**, **affine drop imports (lane 10)** each add a small
  manuscript hunk, flagged in its PR.

### Not in this wave

- A core `while`: the owner chose to wait for `sugar` (§7.6).
- 128-bit integers (Bench emulates `u128`/`i128` in `wide.ves`): one port.
- The realtime verifier's policy gaps (Resonance: `reject_unbounded_loops:
  false` ignored, `--verify-realtime` only for `emit --entry`, undeclared
  `vesper_seq_fault`/`Memmove` callees, 90 s verifier time).
- `Option[E]` results of `handle` with no native layout, and `Fail[RuntimeError]`
  from division and indexing passing through `Fail[VErr]` rows (eBPF).
- Word enums lent in place at the export boundary (Warp `FrameW`, about 6%),
  and large payload enums passed by value (eBPF `Insn`).
- Run-time evidence for compiled `dyn` slots, closures capturing generics,
  associated types and `Rc`/`Arc` of objects (dyn closures report).
- Pointer results and narrow integers in `.vsys` imports (still #611).
- Wider vectors (AVX2): Cranelift 0.125 has no 256-bit types.
- The self-hosted Stage 1 checker (`packages/vesper-compiler`): it stays
  stricter for every new form, as it already is for receiver loans.
- Changing any port's default build or deleting Rust code (the owner decides
  per port; see #1085 for the current deletions).

## Rules for every lane

The [Stage 1 performance wave's rules](../stage1-perf/README.md#rules-for-every-lane)
apply unchanged: claim first, the Stage 1 guard (no edits to
`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**`; new passes on the native
optimize path only unless they are correctness fixes or new language forms;
verified digests equal to the lock), semantics first (interpreter, JIT and AOT
agree, with differential tests), leak tests for runtime helpers, the smallest
manuscript change for any new builtin, form or rule, `cargo +1.100.0 clippy`
before pushing, long runs in GitHub CI, merging on the four required checks.
In addition:

- **Language lanes keep old programs' meaning.** Lanes 6-9 add forms and
  relax refusals; every program that checks on `main` must check and run the
  same afterwards (run the existing corpus differentially).
- **Measure the repro and the ports.** Each prompt names the repros from the
  reports. Report them before and after, and re-measure the named ports with
  their committed ABBA benchmarks and callgrind, with the setup of
  [port-gaps-rescore.md](../../parallel-results/port-gaps-rescore.md) (local
  port rewrites allowed; lane 12 or the port's own lane commits them). Wall
  clock on this kind of host moves by up to 30% between runs of one binary,
  so give instruction counts too.
- **Do not edit a P3b port lane's crate** while its PR is open
  (#1066-#1071, #1073, #1083); hand rewrites to that lane on its issue.
- **Report.** `docs/parallel-results/p3b-gaps-<lane slug>.md`.

## Collisions

| Shared file | Lanes | Rule |
|---|---|---|
| `crates/vesper-syntax/src/parser.rs` | 6 (array types/literals, `const`), 7 (`continue`), 8 (paths, `use .. as`, patterns, contextual keywords) | Separate hunks; merge 7, then 8, then 6, each after merging `main`. |
| `src/check.rs` | 6 (array/`const` typing), 7 (literal inference, `?` on `Option`), 8 (name resolution), 9 (argument reservation) | Edit only your hunks; merge `main` before merging. |
| `src/native.rs` | 3 (slice-header loads), 5 (aggregate homes, results) | Separate hunks; 6's array lowering goes in a new `src/native_arrays.rs`. |
| `src/seq/check.rs`, `src/seq/eval.rs`, `src/seq/native*.rs` | 5 (element field reads), 9 (E1521 receivers), 11 (new builtins, `find_not_in` fallback) | Separate hunks. |
| `src/native_export/**` | 2 (all), 6 part 2 (array mirror fields), 8 (`directives.rs` submodule exports) | 6 part 2 after 2 merges; 8 edits only which module's items are exported. |
| `src/fip.rs` | 10 parts 3-4; P3b lane 2 (#1076) | Lane 10 parts 3-4 only after #1076 merges. |
| `src/ffi_lowering/**` | 8 (`.vsys` in submodules), 10 part 4 (affine drops); P3b lane 3 (#1077) | Both after #1077 merges for any hunk #1077 touches. |
| `src/vector.rs`, `src/native_vector.rs` | 1; stale wave-nine claim #587 | Lane 1 comments on #587 and stops if #587 has an open PR. |
| Manuscript | 1, 2, 6, 7, 8, 9, 10, 11 | Separate hunks; each flagged in its PR. |
| Root `Cargo.toml`/`Cargo.lock` | any lane adding a dependency | Serialize. |
