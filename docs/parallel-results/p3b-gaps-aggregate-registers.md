# P3b gaps lane 5: copy-struct locals in registers

Claim #1090, plan PR #1098, base `9305b24`. Native optimize path only.

## What changed

- **Split locals** (`src/native_aggregates.rs`, `split_locals`, `Split`).
  A local whose type is a struct or tuple of scalar words (integers,
  `bool`, `f32`, `f64`; nested structs and tuples; at most 16 words, no
  unit fields) is one Cranelift variable per word when no `inout`
  argument, sequence, `Box` or tiled place, closure or handler names it,
  and some use names one of its fields. Field reads are the variable,
  field writes (plain and checked compound) define it, a `let` loads only
  the words some use reads (non-trapping loads), and a read of an
  aggregate field or of the whole local stores the words into storage.
  Splits are keyed by the local's own variable, so a specialized callee's
  local of the same generated name never aliases the caller's.
- **Cost count.** A candidate is split only when it is not dearer than
  memory: in memory each field use is a load or store and a `Some(h)` of a
  sequence element copies the element; split, each `let` loads the live
  words and each aggregate read that is not built in place stores its
  words. Uses in loops count 8x. This keeps headers that are read once
  and passed whole to calls (silicon `m_aplic_hdr` callers) in memory.
- **Results in place** (`plan`). A single-use aggregate read of a split
  local gets the home of its consumer: the result buffer (`return`,
  `Option::Some(s)` built in the caller's result) or a field of a later
  constructor (not inside a `Write` destination, whose window rule does
  not cover these stores). A read bound to another split local copies
  variables; a read bound to a split local lends its source instead of
  copying it first. Split locals never get pass-through or `Write` homes.
- **Element field reads** (`src/seq/native.rs`, `seq_fused_bind`).
  `Some(h) => h.f` on a fused `try_get` loads the split `h`'s live words
  straight from the element: one load per field read, no element copy.

## Tests

`tests/native_sroa.rs`, each program in the evaluator, the JIT with the
optimizer on and off, x86-64 and AArch64 objects, and an executable:

- `normalize_agrees`: the eBPF `scalar_normalize` shape (loop, early
  `None`, nested field writes, `Option[Scalar]` result, an `in` use of a
  nested field, a copy mutated apart from its source).
- `normalize_keeps_fields_in_registers`: the optimized CLIF of `normalize`
  has at most 14 loads and 14 stores (14 and 12 on this branch).
- `word_kinds_agree`: `u8`, `bool`, `i32`, `f64`, `u16`, `f32` words,
  nested structs, an `inout` use beside split locals.
- `element_field_reads_agree`: element field reads in a loop, and a
  binding that stays a copy when the element is replaced after it.

All `tests/native_*` suites (except the CLI ones, which CI runs), `backends`
and `checked_arithmetic` pass locally (Linux x86-64, release).

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changes. The
"Stage 1 reproducibility and cross-backend ratchet" and "Stage 0 two-seed
build and Path B" jobs of the bootstrap-stage1 workflow pass on this branch.

## Measurements

Cloud VM, 4 vCPUs, Linux. Callgrind `Ir`; wall clock on these hosts moves
by up to 30%, so the instruction counts are the comparison. Main = `9305b24`.

`scalar_normalize` (eBPF package, x86-64 `vesper build --lib`, static):

| | Instructions | Loads | Stores |
|---|--:|--:|--:|
| main | 282 | 58 | 42 |
| this branch | 259 | 41 | 22 |

The loop now keeps all six `Scalar` words in registers and the entry copy
of `input` is gone. What memory traffic is left is the inlined
`tnum_intersect`/`tnum_range` results (`Option[Tnum]` temporaries, not
locals) and the result stores.

Ports (port build only; the Rust build is unchanged):

| Workload | main Ir | branch Ir | Change |
|---|--:|--:|--:|
| eBPF scalar domain, 4096 steps (`callgrind_one_workload`) | 9,533,776 | 8,739,334 | -8.3% |
| eBPF decode + encode | 5,398,006 | 5,398,006 | 0 |
| eBPF concrete ALU | 764,612 | 764,612 | 0 |
| silicon `aia-intc`, per run (20 runs minus 0) | 3,977,941 | 3,978,803 | +0.02% |
| silicon `aplic-direct`, per run | 1,255,631 | 1,255,640 | 0 |
| BBR `ack_path_abba measure 20000` (whole process) | 46,615,259 | 46,664,897 | +0.1% |

eBPF ABBA `abba_scalar` (30 epochs): Vesper/Rust median 2.01 (1.89-2.12)
on main, 1.76 (1.58-1.89) on this branch. Against the Rust Ir in
port-p3-ebpf.md (4,555,569) the scalar workload goes from 2.09x to 1.92x.

## What is left

- **Silicon header reads.** `m_aplic_hdr` returns the whole `AplicHdr` and
  its callers then read fields or pass the header to calls, so the cost
  count keeps those headers in memory and `aia-intc` does not move. The
  fix is to let `Some(h) => h` lend the element to a split caller local
  through the join (no copy in between), or to port the callers to read
  `a.hdr` fields directly.
- **Aggregate temporaries that are not locals.** Inlined `Option[Tnum]`
  results are still built in a slot and re-read; scalar replacement of
  IR values (not just locals) would remove the rest of `scalar_normalize`'s
  memory traffic.
- **Enums and `Option` locals** are not split (a payload enum needs a tag
  variable and per-variant words).
- **Locals lent `inout`** keep their slot; restoring words after pure calls
  (as `register_locals` does for scalars) would cover them.
- BBR moves by +0.1%: its hot locals are already pointers that a split
  would only copy, and the cost count keeps nearly all of them in memory.
