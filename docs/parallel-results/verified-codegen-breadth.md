# Verified direct codegen breadth (#583)

Base: `042c859` (origin/main). Lane 6 of wave nine, delivered in milestones.
This report is updated by each milestone PR.

## Milestone 1: enums and `match`

### What was built

- **Enum layout.** A non-recursive enum (generic enums after substitution)
  flattens to a tag word (the variant index, path `#tag`) followed by a
  union payload as wide as the widest variant (`#payload.N`). Each variant
  lays its fields out from word 1 in declaration order. Unused payload words
  are written as zero, so results are deterministic. A recursive enum
  (`enum L { Nil, Cons(i64, L) }`) has no finite layout without a heap and is
  rejected with `Unsupported::Enum`. Recursion is detected per
  instantiation, so finite nesting such as `Option[Option[i64]]` is laid
  out normally (`nested.ves`); polymorphic recursion hits the type-depth
  limit.
- **Constructors.** `Op::Data` of an enum variant writes the tag, the
  fields at their variant offsets and the zero padding.
- **`match`.** `Terminator::Match` lowers to a first-match chain: each arm's
  pattern becomes word tests that jump to the next arm on failure, then a
  jump to the arm block. Tag, integer literal (all four kinds, including
  `i64::MIN`), `bool` literal, tuple, struct and nested variant patterns are
  tested. A literal that does not fit the scrutinee's type never matches.
  After the last arm the chain traps with `Unreachable` (the checker
  proves arms exhaustive). The arm block's leading `Bind` of the scrutinee
  then only copies words; refutable patterns in any other binding (a `let`
  of a multi-variant enum constructor, a literal) remain
  `Unsupported::Match`.
- **Result decoding.** `decode` rebuilds enum values (reading the tag,
  the variant's fields, and skipping the payload's remaining words) so the
  differential tests compare with the evaluator's rendering.

The dispatch uses only existing slot-IR rows (`Const`, `Compare eq`,
`BranchZero`, `Jump`, `Label`, `Trap unreachable`). The instruction
selection table and its Lean counterpart are unchanged, and the
row-local simulation in `lean/VerifiedCodegen.lean` covers every emitted
row. Nothing new is proved about the lowering of patterns itself: as
before, Rust lowering is outside the Lean proofs.

### Checks

- `cargo test --locked --test verified_codegen --test verified_codegen_cli
  --test verified_codegen_lean`: 13 passed. New fixtures
  `tests/fixtures/verified_codegen/enums.ves` and `literals.ves` agree with the
  evaluator and the Cranelift JIT (with `nested.ves`) on x86-64 Linux (native) and AArch64 Linux
  (QEMU 8.2.2 user mode), with and without the IR optimizer. AArch64 macOS
  objects are built and checked for determinism but only run on Apple
  silicon (macOS CI).
- New typed rejections: recursive enum (`Enum`), string pattern (`String`),
  float pattern (`Float`).
- `cargo test --locked -p vesper-codegen-verified`: passed. The Stage 1 lock's
  recorded refusal for `parse_main` (`Effect`, effect handlers) is
  unchanged: this milestone only removes earlier refusals, so the first
  refusal on the frontend stays the handler.
- `cargo +1.100.0 clippy --locked -p vesper --all-targets -- -D warnings`
  and `cargo fmt --all -- --check`: clean. `vesper fmt --check` passes on the
  new fixtures.

### Remaining gaps in #583

Strings and byte access, Windows PE/COFF
output with unwind data, and Lean encoder proofs against an ISA model are
not in this milestone. They follow as separate PRs.

## Milestone 2: RV64 target

### What was built

- `Target::RISCV64_LINUX` (`riscv64gc-unknown-linux-gnu`, any `riscv64*`
  Linux triple) and a static ELF (`EM_RISCV`) executable container.
- `src/verified_codegen/riscv64.rs`: one fixed RV64I + M sequence per
  abstract instruction, using the same frame layout as AArch64 (`s0` frame
  pointer, incoming area at `16 + 8n(s0)`). No compressed instructions are
  emitted, so the output runs on RV64GC. RISC-V has no flags: unsigned
  carry and borrow are `bltu` tests, signed add/sub overflow uses the
  `(b < 0) != (sum < a)` identity, multiplication compares `mulh`/`mulhu`
  with the sign extension of the low word, and `i64::MIN / -1` is trapped
  before `div`. RISC-V's own `rem` already gives `MIN rem -1 = 0` and the
  dividend's sign. Conditional branches reach only ±4 KiB, so each branch to
  a label is an inverted branch over a `jal` (±1 MiB, a new `Fix::Jal`
  fixup with a typed `Limit` error when out of range). Frame offsets of
  2048 bytes or more go through a computed address.
- 64-bit immediates are a fixed eleven-instruction sequence (`addi` of the
  top nine bits, then five `slli 11; ori` chunks); a unit test interprets
  the sequence for boundary values.
- `vesper build --codegen verified --target riscv64gc-unknown-linux-gnu`
  writes the RV64 executable; `tests/verified_codegen_cli.rs` checks the
  ELF header and runs it under QEMU when present.
- Encoder unit tests compare fifteen instruction forms with the reference
  encodings from the RISC-V unprivileged specification.

### Checks

- Every differential test in `tests/verified_codegen.rs` (all fixtures,
  more than ninety checked-arithmetic boundary cases, the ten-million-
  iteration tail loop) now also runs the RV64 executable under QEMU 8.2.2
  `qemu-riscv64` user mode and agrees with the evaluator and Cranelift.
  BuildBuddy CI already installs `qemu-user`; elsewhere a missing QEMU is
  reported as unavailable, never as a pass.
- The Lean model covers abstract instruction selection only; the RV64 byte
  encodings are checked by the unit tests and by execution, not proved.

## Milestone 3: `f64`

### What was built

- `f64` is a one-word kind holding the IEEE-754 binary64 bit pattern.
  Literals are their `to_bits()`. New slot-IR rows `FArith` (`+ - * /`,
  also compound assignment), `FNeg` and `FCompare` (`== != < <= > >=`)
  select to new abstract instructions `fop`, `fneg` and `fcmp`. `%` on `f64`
  and `f32` are typed `Unsupported::Float` errors.
- Encodings: x86-64 SSE2 (`movq`, `addsd`/`subsd`/`mulsd`/`divsd`,
  `ucomisd` with a parity check so `==` is false and `!=` true on NaN,
  operand swaps for `<`/`<=`, and `btc rax, 63` for negation); AArch64
  (`fmov`, `fadd`/`fsub`/`fmul`/`fdiv`, `fcmp` with `cset` on EQ, NE, MI,
  LS, GT, GE, which are all false for an unordered pair except NE, and
  `fneg`); RV64 D extension (`fmv.d.x`/`fmv.x.d`, `fadd.d` etc. with the
  static round-to-nearest-even mode, `feq.d`/`flt.d`/`fle.d`, `fsgnjn.d`).
  No fused multiply-add or reassociation is ever emitted.
- Lean: `lean/VerifiedCodegen.lean` gains the three rows and three
  instructions, with `sim_farith`, `sim_fneg` and `sim_fcompare` in
  `select_simulates`. Both levels are defined with Lean's `Float` on the
  same bit patterns, so the theorem shows the table routes the right
  operands to the right binary64 operation and writes the right result
  word. It does not show that `addsd` (or any target instruction)
  implements IEEE-754, and NaN payloads, which differ between targets, are
  not meaningful in the model. `lean/CodegenValidation.lean` drops integer
  facts for `f64` results (`fcompare` results are `bool`), and every
  Codegen Lean file still compiles with Lean 4.35.0 and no `sorry`.

### Checks

- `tests/fixtures/verified_codegen/floats.ves` (dot product, a sum loop of
  `0.1`, `-0.0`, `inf - inf`, a subnormal quotient, `0.1 + 0.2` via `+=`,
  all six comparisons with NaN and with `-0.0 == 0.0`, `nan != nan`) agrees
  with the evaluator and Cranelift on x86-64, and on AArch64 and RV64 under
  QEMU, optimizer on and off. NaN results are compared as NaN, not by
  payload.
- `VESPER_LEAN=<lean 4.35.0> cargo test --locked --test verified_codegen_lean
  --test refinement_places_lean`: pass, including the line-for-line table
  comparison (93 rows).
