# Self-hosting wave SH4: the backends

> Plan written 2026-11-21 against `main` at `b24be92` plus the open SH3 pull
> requests (part of #1253, SH3 lane 14). The
> [manuscript](../../manuscript/README.md) is the sole specification. The
> crate-by-crate inventory and later waves are in
> [self-hosting-plan.md](../../maintenance/self-hosting-plan.md). Confirm live
> ownership on the issue board before acting.

SH3 is porting the compiler's middle (Place IR data, lowering, `ir_opt`, the
AST optimizer, the inliner, bounds hoisting). SH4 ports what comes after it:
native lowering through the Cranelift shim, the export ABI, FFI lowering,
freestanding images and Windows objects, the wasm and eBPF backends, and the
checks that run on emitted code. That is about 49,000 lines of Rust:

| Family | Rust (non-test lines) | Lane |
|---|--:|---|
| `Codegen` core: IR graph walk, expressions, operators, control flow (src/native.rs) | ~3,500 | 6 |
| Instances, signatures, native ABI, calls, entries (src/native.rs) | ~2,500 | 7 |
| Aggregates, arrays, patterns, equality, drop glue, fip; the `ffi_lowering::drops` pass | ~3,900 | 8 |
| Sequences, bytes, strings and the AOT sequence runtime | ~4,700 | 9 |
| Closures, handlers, effects, `dyn`, sessions | ~4,800 | 10 |
| Vectors, tiles, floats, 128-bit helpers, regions, fip runtime, atomics | ~5,800 | 11 |
| Export ABI v1-v4, bindings, glue | ~4,400 | 12 |
| FFI lowering (`.vsys`) | ~3,100 | 13 |
| Freestanding images, Windows objects | ~4,100 | 14 |
| Wasm backend | ~7,000 | 15 |
| eBPF backend, stack bound | ~2,800 | 16 |
| Backend verifiers, coverage, batch builds, resource traces | ~4,300 | 17 |

The SH3 plan deferred one more piece of SH3 lane 14 (#1253): its single-build
re-measure of the SH2 and SH3 swaps and its SH4 sample translation. This plan
is the SH4-plan part of #1253. The re-measure stays with #1253 and runs when
SH3's swaps have merged. Lane 1's pilot below takes the place of the sample.

**The owner asked to start SH4 on top of SH3 work that is still in open pull
requests** (2026-11-21, "do the next round of self hosting stacked on the
last"). Most lanes stack on #1259 (SH3 lane 1 part 1: `@vesper/place-ir` and
`src/ir_swap.rs`), which itself stacks on #1232. The SH3
[stacking rule](../self-host-sh3/README.md#rules-for-every-lane) applies.

Every port here keeps Rust as the default build under the standing owner rule.

## How SH4 swaps

Native lowering is one large `Codegen` that emits one function at a time into
a Cranelift module. Porting it whole before anything can be compared would be
a 38k-line lane. Instead:

- **Lane 1 lends Cranelift to Vesper.** The shim gets borrowed builder and
  module handles, so the Rust `Codegen` can hand its live `FunctionBuilder`,
  variables and values to a Vesper hook for one family's emission and take
  them back. `src/codegen_swap.rs` holds one optional hook per family, called
  at the Rust family's dispatch site, on the native path only.
- **Each port lane swaps one family.** It adds a Vesper package and a hybrid
  install for its hook, and never edits the Rust lowering.
- **The differential is the emitted code.** Per-function CLIF text must equal
  Rust's, object bytes must be identical, and JIT run results must match with
  optimize on and off, over the whole corpus. The census already showed this
  is reachable: its lowering family produced byte-identical objects at 1.06x
  of Rust's Ir with Cranelift's compile included.
- **Lane 2 makes the inputs cheap.** Hooks get the `World` and the `Program`
  from one encode-once, per-compilation input, not a fresh encoding per call.

The whole-backend lanes (15 wasm, 16 eBPF, 17 verifiers) do not need lent
builders: they read Place IR through #1259 and compare output bytes.

## Lanes

Claims: lanes 1-18 are #1275-#1292 (lane n is issue 1274+n).

### Shared pieces

| Lane | Prompt | Claim | What | Starts | Stacks on | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [Codegen swap point, lent Cranelift handles, lowering differential](01-codegen-swap.md) | #1275 | lent builder and module handles, `JITModule`, builder extras, `src/codegen_swap.rs`, `@vesper/codegen-base`, CLIF and object differential, census family as pilot swap | now | #1259 | `crates/vesper-cranelift-sys/**`, `packages/vesper-cranelift/**`, new `packages/vesper-codegen-base/**`, new `src/codegen_swap.rs` and its dispatch-line hunks in the native family files, `tests/common/clif_diff.rs`, `tests/self_host_codegen_swap*.rs`, hybrid module `codegen`, report |
| 2 | [Swap inputs without re-encoding](02-swap-inputs.md) | #1276 | one shared, encode-once `World` and `Program` per compilation, lazy sections or lent accessors | now | #1259 | new `src/swap_input.rs` and its `src/ir_swap.rs` hunk, the encode-once hunks of #1232's and #1259's encoders, hybrid input plumbing, `tests/self_host_swap_inputs*.rs`, report |

### Compiler gaps

| Lane | Prompt | Claim | Gap | Found by | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 3 | [128-bit aggregates, division, export cells](03-wide-aggregates.md) | #1277 | native reads `[i128; N]` elements wrong; slow 128-bit division; no 128-bit export cells | #1270; SH2 lane 2 gaps 3, 5 | now, from main | `src/native_arrays.rs`, 128-bit hunks of `src/native_aggregates.rs`, `src/integer_widths/native.rs`, `src/native_export.rs`, `src/native_export/binding_v4.rs`, `tests/wide_aggregates*.rs`, report |
| 4 | [Callback parameters in registers](04-callback-registers.md) | #1278 | lent callbacks keep parameters in stack slots (+1.3% to +4.1% Ir) | #1271 (SH3 lane 3) | now, from main | `register_locals`/`in_param_registers` hunks of `src/native.rs`, `tests/callback_registers*.rs`, report |
| 5 | [Optimizer time on large programs](05-optimizer-budget.md) | #1279 | `ir_opt::optimize` is 72% of the combined checker build | SH3 lane 7 gap 1 | now, from main (owner chose option A, 2026-11-21) | new `src/ir_opt_policy.rs` and its default-`opt` hunk in `src/ir_swap.rs`, `tests/optimizer_budget*.rs`, report |

### Native lowering ports

Each starts now, stacked on #1259, with the parts that read the IR and decide
(no emission). Emission and the swap come after lane 1's first PR.

| Lane | Prompt | Claim | Rust replaced | Hook | Also waits on | Exclusive boundary |
|---|---|---|---|---|---|---|
| 6 | [Scalars, operators, control flow](06-codegen-scalar.md) | #1280 | `Codegen` core in `src/native.rs` | `scalar` | | new `packages/vesper-codegen-scalar/**`, hybrid module `codegen_scalar`, `tests/self_host_codegen_scalar*.rs`, one scoreboard row, report |
| 7 | [Instances, native ABI, calls](07-codegen-calls.md) | #1281 | instances, signatures, ABI, calls, entries in `src/native.rs` | `call` | lane 4 (register locals) | new `packages/vesper-codegen-calls/**`, hybrid module `codegen_calls`, `tests/self_host_codegen_calls*.rs`, row, report |
| 8 | [Aggregates, patterns, equality, drops, fip](08-codegen-aggregates.md) | #1282 | `src/native_aggregates.rs`, `src/native_arrays.rs`, drop and equality helpers, `src/ffi_lowering/drops.rs` | `aggregate`, `drops` | lane 3 (128-bit elements) | new `packages/vesper-codegen-aggregates/**` (takes lane 1's pilot), hybrid modules `codegen_aggregates` and `drops`, `tests/self_host_codegen_aggregates*.rs`, row, report |
| 9 | [Sequences, bytes, strings, AOT sequence runtime](09-codegen-seq.md) | #1283 | `src/seq/native*.rs`, `src/seq/aot_runtime.rs`, `src/strings/native.rs` | `seq` | | new `packages/vesper-codegen-seq/**`, hybrid module `codegen_seq`, `tests/self_host_codegen_seq*.rs`, row, report |
| 10 | [Closures, handlers, effects, dyn, sessions](10-codegen-control.md) | #1284 | closure and effect code of `src/native.rs`, `src/native_handlers/**`, `src/dyn_trait/native.rs`, `src/native_session.rs` | `control` | lane 4 (callback parameters) | new `packages/vesper-codegen-control/**`, hybrid module `codegen_control`, `tests/self_host_codegen_control*.rs`, row, report |
| 11 | [Vectors, tiles, floats, wide integers, regions, atomics](11-codegen-numeric.md) | #1285 | `src/vector.rs`, `src/native_vector.rs`, `src/tiled/{native,native_runtime,aot_runtime}.rs`, `src/float_math/native.rs`, `src/integer_widths/native.rs`, `src/regions/{native,aot_runtime}.rs`, `src/fip/aot_runtime.rs` | `numeric` | lane 3 (128-bit helpers) | new `packages/vesper-codegen-numeric/**`, hybrid module `codegen_numeric`, `tests/self_host_codegen_numeric*.rs`, row, report |
| 12 | [Export ABI](12-codegen-export.md) | #1286 | `src/native_export.rs`, `src/native_export/**` | `export` | lane 3 (128-bit cells) | new `packages/vesper-codegen-export/**`, hybrid module `codegen_export`, `tests/self_host_codegen_export*.rs`, row, report |
| 13 | [FFI lowering](13-ffi-lowering.md) | #1287 | `src/ffi_lowering.rs`, `src/ffi_lowering/{types,native}.rs` | `ffi` | | new `packages/vesper-ffi-lowering/**`, hybrid module `ffi_lowering`, `tests/self_host_ffi_lowering*.rs`, row, report |
| 14 | [Freestanding images, Windows objects](14-freestanding.md) | #1288 | `src/native_freestanding.rs`, `src/freestanding/**`, `src/native_windows.rs` | `freestanding` | | new `packages/vesper-codegen-freestanding/**`, hybrid module `codegen_freestanding`, `tests/self_host_freestanding*.rs`, row, report |

### Whole backends and checks on emitted code

| Lane | Prompt | Claim | Rust replaced | Starts | Exclusive boundary |
|---|---|---|---|---|---|
| 15 | [Wasm backend](15-wasm-backend.md) | #1289 | `src/wasm_backend.rs`, `src/wasm_backend/**` | now on #1259; swap after lane 1 | new `packages/vesper-wasm-backend/**`, hybrid module `wasm_backend`, `tests/self_host_wasm_backend*.rs`, row, report |
| 16 | [eBPF backend, stack bound](16-ebpf-backend.md) | #1290 | `src/ebpf_backend.rs`, `src/ebpf_backend/**`, `src/stack_bound.rs` | now on #1259; swap after lane 1 | new `packages/vesper-ebpf-backend/**`, hybrid module `ebpf_backend`, `tests/self_host_ebpf_backend*.rs`, row, report |
| 17 | [Backend verifiers, coverage, batch, resource traces](17-backend-checks.md) | #1291 | `src/native_verifiers.rs`, `src/native_coverage.rs`, `src/native_batch.rs`, `src/runtime/resource_trace*.rs` | now on #1259; swap after lane 1 | new `packages/vesper-backend-checks/**`, hybrid module `backend_checks`, `tests/self_host_backend_checks*.rs`, row, report |

### Planning

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 18 | [SH4 scoreboard and SH5 plan](18-scoreboard-sh5-plan.md) | #1292 | last: when lanes 6-17 have each merged a first swap | `docs/maintenance/self-hosting-plan.md` (SH4 results, SH5 sections, gap table), new `docs/agent-prompts/self-host-sh5/**`, `docs/parallel-results/self-host-sh4-scoreboard.md` |

**Start now:** lanes 1-17 (1, 2 and 6-17 stacked on #1259; 3, 4 and 5 from
`main`).
**After lane 1's first PR:** emission and the swap in lanes 6-14, and the swaps
of lanes 15-17.
**Last:** lane 18.

**Recommended merge order.**
- Each stacked lane merges only after #1259 (and so #1232) has merged.
- Lane 1 first among the ports: every port swaps through `src/codegen_swap.rs`
  and compares through its harness. Lane 2 next, so the ports measure with
  cheap inputs.
- Lanes 3 and 4 merge before the ports that adopt them (3 before 8, 11 and 12;
  4 before 7 and 10). Lane 4 and lane 1 both edit `src/native.rs`: merge
  `main` between them.
- Lanes 6-17 each add a module to `crates/vesper-hybrid` and a row to the SH4
  scoreboard in `docs/maintenance/self-hosting-plan.md`: merge `main` right
  before merging each, and serialize merges that touch root `Cargo.toml`.
- Lane 18 last.

## Rules for every lane

Every rule in [SH2's "Rules for every lane"](../self-host-sh2/README.md#rules-for-every-lane)
and every [SH3 addition](../self-host-sh3/README.md#rules-for-every-lane)
(stacking, the Stage 1 guard for IR lanes, measuring the pass and not the
encoding, constants, element views) applies, with these additions.

- **Stage 1 guard, for backend lanes.** No swap point is on the verified
  backend's path (`src/verified_codegen/**`, `crates/vesper-codegen-verified`),
  and the default compiler installs no hook, so its objects are
  byte-identical to `main`'s.
- **Port lanes never edit the Rust lowering.** Lanes 6-17 read the Rust files
  they port and add only Vesper packages, tests, a hybrid install and a
  report. Only lane 1 adds hook call sites; only lanes 3 and 4 change Rust
  lowering behaviour. A port that finds a Rust bug reports it with a repro
  and matches Rust until the fix merges.
- **Codegen differential.** Per-function CLIF text equal to Rust's
  (lane 1's `tests/common/clif_diff.rs`), object bytes identical, and JIT
  run results equal with optimize on and off, over the whole corpus and the
  family's own tests. A lane names every difference it cannot close.
- **Emit through the shim.** Lowering emits through `@vesper/cranelift`
  (owner decision 2026-11-19). A builder call the shim lacks goes into lane 1
  if lane 1 is open, or a small shim PR that names lane 1's claim.
- **Measure emission and the whole compile.** Report the family's emission
  alone and the whole compile with the family swapped, ABBA and callgrind,
  against the Rust compiler. The census measured emission at 2.35x Ir and
  emission plus Cranelift at 1.06x; Cranelift's compile is the same code on
  both sides, so the whole-compile number is the parity bar.

## Calls flagged for the owner

- **Optimizer time on large programs (lane 5).** `ir_opt::optimize` is 72% of
  the combined SH2 checker build (170.8 s), and it is Stage 1 guarded. Options:
  (A) a native-path policy around the unchanged pass (per-function runs, a
  content-hash cache, skipping saturation where it finds nothing), which
  leaves `bootstrap/stage1.lock` alone; (B) budgets inside `ir_opt::optimize`,
  which edits guarded code and needs a Stage 1 re-lock; (C) no Rust change,
  relying on SH3 lane 9's Vesper `ir_opt` on `@vesper/egraph`. **The owner
  chose A (2026-11-21).**
- **WIT component wrapping (lane 15).** If wit-component's part the backend
  uses is large, lane 15 keeps it behind a `.vsys` extern and reports the
  size; whether to port it later is an owner call.

## Not in this wave

- SH3 lane 14's single-build re-measure of SH2 and SH3 (#1253, after SH3's
  swaps merge).
- The evaluator's FFI host (`src/ffi_lowering/eval_host.rs`), the driver, the
  test runner, the LSP and Lean extraction: SH5.
- A Vesper-native backend (SH7, owner decision 2026-11-19).
- Deleting any Rust module or seed copy: an owner call per module at parity.
