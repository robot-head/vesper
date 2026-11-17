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

## Milestone 4: Windows x64 COFF

### What was built

- `Target::X86_64_WINDOWS` (`x86_64-pc-windows-msvc`) emits an x86-64 COFF
  object (written directly, deterministically) with `.text`, `.xdata` and
  `.pdata`, exporting the C `main`. `vesper build --codegen verified
  --target x86_64-pc-windows-msvc` links it with the same MSVC linker
  discovery and static runtime libraries as the Cranelift Windows path
  (`native::windows::link_executable`, read-only reuse).
- Every compiled function and `main` get one `RUNTIME_FUNCTION` and a
  version-1 `UNWIND_INFO` (`UWOP_PUSH_NONVOL rbp`, then
  `UWOP_ALLOC_SMALL`/`ALLOC_LARGE` for the fixed frame), with
  `IMAGE_REL_AMD64_ADDR32NB` relocations. Function bodies never move `rsp`,
  so the fixed allocation is the whole unwind state. On Windows the
  epilogue is the canonical `add rsp, frame; pop rbp; ret`, and frames of a
  page or more touch each page below `rsp` in order (as `__chkstk` does)
  before allocating.
- `main` calls the entry, writes the result words with `GetStdHandle` and
  `WriteFile` (raw bytes, no CRT text mode) for the word protocol, and
  returns the low byte of the first word as the exit status. Abort exits
  call `ExitProcess(101|102|103)`. The x86-64 integer, float and control
  encodings are otherwise shared with Linux: they use only `rax`, `rcx`,
  `rdx`, `xmm0` and `xmm1` (volatile in the Windows ABI) and `rbp`, which
  the prologue saves.

### Checks

- The differential tests include the Windows target. Locally the objects
  were linked with MinGW-w64 GCC 13 and run under Wine 9.0: every fixture,
  every checked-arithmetic boundary and the ten-million-iteration loop
  agree with the evaluator and Cranelift. On the Windows CI job the same
  tests link with `link.exe` and run natively; elsewhere the target is
  reported as unavailable.
- `containers_are_well_formed` checks the object with
  `native::windows::inspect`: AMD64, `.text`/`.xdata`/`.pdata`, one
  `RUNTIME_FUNCTION` and `UNWIND_INFO` per function plus `main`, and three
  `ADDR32NB` relocations per row.
- `builds_and_runs_on_the_host` now builds and runs the native `.exe` on
  Windows (exit status `6765 % 256`, and 101 for an unhandled overflow).

### Limits

The abort stubs, and the brief `WriteFile` call window in `main`, have no
exact unwind description: a trap is a jump into the stub from the
function's own frame, so a stack walk started there would be imprecise.
Nothing in these programs raises SEH exceptions, and no unwinding is
exercised by the tests.

## Milestone 5: RV64 encodings proved against the ISA formats

### What was built

- `lean/CodegenRv64Encoding.lean` (imports `VerifiedCodegen.lean`) models
  the RV64 encoder:
  - `Inst` is the subset the emitter uses, named as in the ISA manual:
    - the `OP` and `OP-IMM` operations;
    - `slli`, `srai`, `ld`, `sd`, the four branches, `jal`, `jalr` and `ecall`;
    - the D-extension operations and `fmv.d.x`/`fmv.x.d`.
  - `decode` is the specification:
    - the R/I/S/B/J field positions;
    - the scattered B and J immediate bits;
    - sign extension;
    - the manual's opcode, `funct3` and `funct7` tables.
  - `encode` writes the fields the way `riscv64.rs` does.
- `decode_encode`:
  - Statement: every well-formed instruction encodes to a word below
    `2^32`, and `decode` maps that word back to the same instruction.
  - Well-formed means registers below 32, immediates in their field's
    range, and even branch and jump offsets.
  - `bytes_encode` gives the little-endian bytes.
- `emit` mirrors `riscv64::emit` for every `M` instruction. It covers the
  11-instruction `li`, large frame offsets through `t0`, and the
  inverted-branch-over-`jal` trap checks. Two theorems cover it:
  - `emit_ok` proves every instruction `emit` produces is well formed, for
    any `M` with `u32` frame indices and sizes.
  - `emit_decodes` chains `emit_ok` with `decode_encode` over the laid-out
    sequence.
- The listing check:
  - The Lean file prints the words of 59 sample `M` instructions, covering
    every instruction kind, both signs, every condition, large offsets and
    full 64-bit immediates.
  - `riscv64::encoding_listing()` (re-exported as
    `verified_codegen::rv64_encoding_listing`) runs the Rust emitter on the
    same instructions.
  - `tests/verified_codegen_lean.rs` requires the two listings to be equal
    line for line. This ties the Rust bit-twiddling to the proven model on
    those samples. Outside the samples it is a test, not a proof.
- `scripts/ci/check-lean-models.sh` compiles the new file after
  `VerifiedCodegen.olean`.

### Checks

- With Lean 4.35.0 the file compiles with no warnings.
- Every theorem's axiom report uses only `propext`, `Classical.choice` and
  `Quot.sound`. The hygiene test bans `sorry`, `native_decide`,
  `bv_decide` and similar escape hatches.
- The listings agree: `cargo test --test verified_codegen_lean` passes
  with `VESPER_LEAN` set.

### Remaining gaps

- No encoding proof exists yet for x86-64 (milestone 6 below covers
  AArch64).
- The RV64 proof covers encoding only. Instruction semantics (that the
  emitted sequence implements `Mach.step`) are still checked by
  differential tests under QEMU.

## Milestone 6: AArch64 encodings proved against the A64 encoding diagrams

### What was built

- `lean/CodegenA64Encoding.lean` (imports `VerifiedCodegen.lean`) models the
  AArch64 encoder the same way milestone 5 models RV64.
  - `Inst` is the A64 subset `aarch64.rs` uses, named as in the Arm ARM:
    - `LDR`/`STR` (unsigned offset), `MOVZ`/`MOVK`;
    - `B.cond`, `B`, `BL`, `CBZ`/`CBNZ`;
    - `ADD`/`ADDS`/`SUB`/`SUBS`/`AND` (shifted register, `LSL` or `ASR`);
    - `MADD`/`MSUB`/`SMULH`/`UMULH`, `UDIV`/`SDIV`;
    - `ADD`/`ADDS`/`SUB` (immediate, optionally `LSL #12`), `EOR`
      (bitmask immediate), `CSINC`;
    - `FMOV` between `Xn` and `Dn`, `FADD`/`FSUB`/`FMUL`/`FDIV`, `FNEG`,
      `FCMP` (double);
    - `STP` (pre-index), `LDP` (post-index), `RET`.
  - `decode` is the specification. It dispatches on bits 31:26 and then on
    each encoding diagram's fixed bits (`sf`, `op`, `S`, `opc`, `ftype`,
    `op31`, `o0`, `op2`, bit 21 of the shifted-register forms, the shift
    type). It reads the operand fields at their positions and sign-extends
    `imm19`, `imm26` and `imm7`. `SMULH`/`UMULH` require `Ra = 31`, as the
    manual does. Bitmask immediates are decoded only for `N = 1` with
    `imms ≠ 111111` (a single 64-bit element), the only form the emitter
    writes; other `N`/`imms` values decode to `none` rather than being
    misread.
  - `encode` writes the fields the way `aarch64.rs` does.
- `decode_encode`:
  - Statement: every well-formed instruction (registers below 32, every
    immediate and offset in its field's range) encodes to a word below
    `2^32` that `decode` maps back to the same instruction.
  - Each case computes the fields the decoder reads on that instruction's
    path with `omega`, rewrites them, and lets the dispatch reduce.
  - `bytes_encode` gives the little-endian bytes.
- `emit` mirrors `aarch64::emit` for every `M` instruction: the
  four-instruction `movz`/`movk` immediates, the flag-based trap checks,
  `cset` as `csinc` with the inverted condition, and the frame's
  `stp`/`sub sp` (with the shifted form above 4 KiB)/`ldp`/`ret`.
  - `emit_ok` proves every item is well formed for any `M` whose frame
    indices fit the 12-bit scaled offset (frames are at most
    `MAX_FRAME_WORDS` = 4000 words) and whose frame size is below 2^24
    bytes.
  - `emit_decodes` chains `emit_ok` and `decode_encode` over the layout.
- The listing check:
  - The Lean file prints the words of 61 sample instructions: milestone 5's
    59 plus `enter 1000` and `enter 512`, which take the shifted `sub sp`
    forms.
  - `aarch64::encoding_listing()` (re-exported as
    `verified_codegen::a64_encoding_listing`) runs the Rust emitter on the
    same samples. The shared samples and the listing loop moved to
    `asm::encoding_samples` and `asm::encoding_listing`, so the RV64 listing
    is unchanged.
  - `tests/verified_codegen_lean.rs` requires the listings to be equal line
    for line, and checks the file's hygiene and axiom report like the RV64
    model's.
- `scripts/ci/check-lean-models.sh` compiles the new file.

### Checks

- With Lean 4.35.0 the file compiles with no warnings in about 20 s.
- Every axiom report uses only `propext`, `Classical.choice` and
  `Quot.sound`.
- `cargo test --test verified_codegen_lean` passes with `VESPER_LEAN` set:
  7 tests, including both encoding listings.

### Remaining gaps

- x86-64 had no encoding proof; milestone 9 adds it.
- As for RV64, this proves encoding, not semantics. That the emitted A64
  sequences implement `Mach.step` is still checked by running programs
  natively and under QEMU.
- The prologue and system-call code in `aarch64::start` are outside `emit`
  and not modelled.

## Milestone 7: abort-only `Fail[E]`, `?` and `handle Fail`

Claim #837 (lane V1 of the verified-compiler parallel plan). Refs #582,
#583. Base: `5a018bd` (origin/main).

### What was built

- Failure codes (`src/verified_codegen/fail.rs`):
  - Every error type the planned program raises or handles gets a
    program-wide code (1, 2, ...). Code 0 means no failure.
  - A raise set is computed for every planned function. It is a least
    fixpoint over the call graph, and a `handle Fail[E]` removes `E`'s code
    from its body's set.
  - Local raises come from three places:
    - `?` on `Result[T, E]`.
    - `Fail::raise(e)`.
    - Checked arithmetic, counted conservatively by operator: `+ - *`,
      unary `-` and compound assignment may overflow, and `/ %` may also
      divide by zero.
- The status protocol:
  - A function that may raise to its caller (any planned function except
    the entry) returns a status word. Word 0 of its incoming result area
    is the status. The words after it hold the result when the status is 0,
    or the raised error's payload. Parameters follow these words.
  - After each call to such a function, the caller tests the status
    (`LoadOut`, `Const`, `Compare`, `BranchZero`). On a failure it does one
    of three things:
    - jumps to the matching handler clause;
    - passes the status and payload on to its own caller;
    - at the entry, aborts the process with the existing exit status for
      `ArithOverflow` (101) or `RuntimeError` (102).
  - Any other error type that can reach the entry is a typed
    `Unsupported::Effect`.
- `?`: the `Err` tag is tested. On `Err` the payload is raised, and
  otherwise the `Ok` payload is copied.
- `handle Fail[E] in body with { return v => r, raise(e, k) => h }`:
  - The nested `$handler_body`/`$handler_clause` functions the IR already
    produces are called directly, with their captures passed as arguments
    (`inout` captures are copied back).
  - On success the `return` clause is called with the body's value. On a
    raise of `E` the `raise` clause is called with the payload. Every other
    failure passes on.
  - The continuation `k` must be unused, so only non-resuming handlers
    compile. It is erased from the clause's parameters.
  - These are refused with typed errors: stateful handlers, labeled `Fail`
    instances, destructuring clause patterns and every other effect.
- Implicit failures:
  - `ArithOverflow` and `RuntimeError` join the protocol only when the
    program handles or explicitly raises them somewhere. Otherwise they stay
    process aborts, so programs without handlers compile exactly as before.
    `trap_scopes_only_where_implicit_failures_are_caught` checks this.
  - When they are caught, a function that passes them on gets a new slot-IR
    row, `Lir::Scope { ovf, div0 }`, after its prologue.
  - The row selects to `M::Scope`, which emits no code. The three encoders
    resolve every overflow or division-by-zero branch through
    `Asm::trap_label`, so the checked instructions jump to the function's own
    raise stubs instead of the process exits. The default scope is restored
    at the function's end.
  - `FnLower::raise_runtime_error(variant)` raises a `RuntimeError` variant
    from lowering code. The index-bounds check of the coverage lane (V3) is
    meant to use it.
- `Never` values have no words. A `return` or `jump` of one, which can only
  follow a raise, lowers to the unreachable trap.
- The checker forbids an abortable effect crossing an active `inout`
  borrow (E0402), so failures never meet `inout` parameters. The raise path
  still writes them back, which keeps the protocol safe if that rule is ever
  relaxed.

### Lean

- `lean/VerifiedCodegen.lean` gains the `scope` row and instruction, the
  table row `scope: scope L9 L10` (the Rust listing must still match) and
  `sim_scope`.
- `CodegenComposition`, `CodegenControlFlow`, `CodegenValidation`,
  `CodegenFlowValidation` and `CodegenRv64Encoding` are extended for the
  new constructor. The RV64 encoding of `scope` is empty.
- New `lean/CodegenFailScope.lean` (imports `CodegenControlFlow`):
  - `abort_preserves_memory`: every selected row writes memory only in its
    trailing stores, which cannot abort, so the machine state at an
    aborting instruction has the memory the row started with.
  - `row_simulatesC`: with a trap scope, a source row whose failure is
    caught moves to the handler label with the store it started from. The
    target block branches to the same label with a related machine state.
    Uncaught failures, faults and unreachable code still abort identically.
  - `prefixes_simulateC`: lifts this to every finite execution prefix
    under an explicit invariant. The scope in effect at a row is the last
    `scope` row before it in program order (`scopeAt`), as in the
    assembler's linear pass.
  - `scopeAt_default`, `sourceRowC_default` and `targetRowC_default` show
    that the scoped semantics equals the existing one when no scope is set.
  - `caught_source`, `caught_invariant` and `caught_target` work through a
    concrete `u8` overflow caught by a handler.
- Checks:
  - With Lean 4.35.0, every file compiles with no warnings and no `sorry`.
  - The axiom reports use only `propext`, `Classical.choice` and
    `Quot.sound`.
  - `tests/refinement_places_lean.rs` audits every theorem of the new file.
  - `scripts/ci/check-lean-models.sh` compiles it.

### Tests

- `tests/verified_codegen.rs`, `fail_effects_agree_with_the_evaluator_and_cranelift`:
  - `fail_try.ves`: `?` on a user enum, propagation through two calls,
    `Fail::raise` of a struct, nested handlers and `return` clauses.
  - `fail_implicit.ves`: `u8` and `u64` overflow and division by zero
    caught by handlers, both directly and across calls, `i64::MIN / -1`
    caught as overflow, and a `return` clause that can itself overflow.
  - Every target is compiled, with and without the IR optimizer. The output
    is checked for determinism and run on x86-64 natively and on AArch64 and
    RV64 under QEMU 8.2.2. Results are compared with the evaluator and the
    Cranelift JIT.
- `unhandled_user_failures_at_the_entry_are_typed_errors` and
  `trap_scopes_only_where_implicit_failures_are_caught`.
- `tests/verified_codegen_fail_front_end.rs` runs on the real
  self-hosted front end:
  - Every function with scalar parameters and a `Fail`-only row (160) is
    wrapped in an entry that handles its non-arithmetic `Fail[E]`.
  - **54** of them now compile, up from 22 before this milestone, where
    every handler was refused. All 54 agree with the evaluator on x86-64,
    AArch64 and RV64, and 12 of the runs take a raise clause.
  - The remaining refusals belong to other lanes: dynamic indexing (103,
    V3), `Region` handlers (2) and one generic function (V2). No refusal
    names `Fail`.

### Remaining gaps

- Resuming handlers, stateful handlers and labeled `Fail` instances are
  refused.
- An `IndexOutOfBounds` that reaches the entry maps to the
  division-by-zero exit status, because the entry has one exit per failure
  kind.
- The status protocol, the call convention and the Rust lowering of
  raises, tests and handlers are covered by differential tests, not by the
  Lean models, like calls before this milestone.

## Milestone 9: x86-64 encodings proved against the Intel SDM formats

### What was built

- `lean/CodegenX64Encoding.lean` (imports `VerifiedCodegen.lean`) models
  the x86-64 encoder. Instructions have variable length, so an encoding is
  a list of bytes and the specification is a prefix decoder instead of a
  field reader over a 32-bit word.
  - `Inst` is the subset `x86_64.rs` uses, named as in the Intel SDM:
    - `MOV r64, [rsp + disp32]`/`[rbp + disp32]` and the stores, `MOV r64,
      imm64`, `MOV r32, imm32`;
    - `ADD`/`SUB`/`AND`/`CMP`/`TEST`/`MOV` (`MR` form, register operands),
      `IMUL r64, r/m64`, the `F7` group (`MUL`, `NEG`, `DIV`, `IDIV`),
      `CQO`, `XOR r32, r32`, the `81`/`83` immediate groups;
    - `Jcc rel32`/`rel8`, `JMP rel32`/`rel8`, `CALL rel32`, `SETcc`,
      `MOVZX r32, r8`;
    - `MOVQ` between general and XMM registers, `ADDSD`/`SUBSD`/`MULSD`/
      `DIVSD`, `UCOMISD`, `BTC r64, imm8`;
    - `TEST [rsp + disp32], r32` (the Windows stack probe), `PUSH`, `POP`,
      `RET`.
  - `decode` reads the `66`/`F2` legacy prefixes, `REX.W`, one- and
    two-byte (`0F`) opcodes, the ModRM byte (`mod`, `reg`, `r/m`), the SIB
    byte `24` for `[rsp + disp32]`, and little-endian `disp32`, `imm8`,
    `imm32`, `imm64`, `rel8` and `rel32` fields. It accepts only the operand
    forms the emitter writes (`mod = 11` register operands, `mod = 10`
    memory operands with base `rsp` or `rbp`) and returns `none` for the
    others. `89` is the store with a memory operand and `MOV` with a
    register operand, as in the manual.
  - `encode` writes the bytes the way `x86_64.rs` does.
- Theorems:
  - `decode_encode`: every well-formed instruction (registers below 8,
    every field in range) decodes back from its encoding, whatever bytes
    follow it. This is the property a prefix code needs, so instruction
    boundaries are recovered too.
  - `decodeAll_encodeAll` extends it to instruction sequences: the
    concatenated bytes decode back to exactly the sequence.
  - `encode_bytes`: every encoded value is a byte.
  - `emit` mirrors `x86_64::emit` for every `M` instruction, Linux and
    Windows. That covers the `jo`/`jb` checks, the `idiv` guard for
    `MIN / -1` and `rem` by `-1`, the NaN-aware `ucomisd`/`setcc`
    sequences, and the prologue with one Windows probe per 4 KiB page and
    the canonical Windows epilogue.
  - `emit_ok` holds for frame indices and sizes that fit `disp32`/`imm32`.
  - `emit_decodes` proves that the laid-out bytes of `emit m` are bytes
    and decode to exactly its instruction sequence.
  - Branches are laid out with the abort exits at offset 0 and every other
    label at the end, and a `rel32` is the displacement modulo 2^32. The
    Rust assembler refuses displacements outside `i32`.
  - The abort exits are the labels below `firstFree`, which a test checks
    against `FIRST_FREE_LABEL` (one past `TrapKind::ALL`). Review found that
    the three encoding models had hard-coded `l < 4` although Rust reserves
    only labels 0-2, so a branch to the first function label (3) was laid
    out as an abort exit. All three models now use `firstFree`, and every
    listing has a `jmp L3` sample, with label 3 bound after the instruction,
    that catches the difference.
- The listing check:
  - The Lean file prints the bytes of the 60 shared samples (milestone 5's
    59 plus `jmp L3`; the RV64 and AArch64 listings gain the same line).
  - It adds the Windows forms of `enter 4`, `enter 1000` (one probe) and
    `enter 2048` (four probes), and `leave` with an 8000-byte frame.
  - `x86_64::encoding_listing()` (re-exported as
    `verified_codegen::x64_encoding_listing`) runs the Rust emitter on the
    same instructions. `tests/verified_codegen_lean.rs` requires the two
    to be equal line for line, and checks hygiene and the axiom report.
- `scripts/ci/check-lean-models.sh` compiles the new file.

### Checks

- With Lean 4.35.0 the file compiles with no warnings in about 8 s.
- Every axiom report uses only `propext`, `Classical.choice` and
  `Quot.sound`.
- `cargo test --test verified_codegen_lean` passes with `VESPER_LEAN` set:
  10 tests, including all three encoding listings and the `firstFree`
  check.

### Remaining gaps

- This proves encoding, not semantics. That the emitted x86-64 sequences
  implement `Mach.step` is still checked by running programs.
- `x86_64::start`, `start_windows` and the external calls are outside
  `emit` and not modelled. The Windows unwind data is checked by the COFF
  tests, not by this proof.

## Milestone 8: generic and region-polymorphic functions, region memory

Claim #844 (lane V2 of the verified-compiler plan; references #582, #583).

### What was built

- Monomorphization (`src/verified_codegen/mono.rs`) runs before planning.
  - Starting at the entry, it collects every reachable instance
    `(function, type arguments)`. The arguments come from the checker's
    `World::calls` plus explicit `f[T](..)` arguments.
  - Each instance gets a copy of its Place IR with every type substituted,
    and every direct callee renamed to the callee's instance.
  - The nested functions of `handle` bodies are instantiated with their
    parent's arguments.
  - An instance is named `name<T=i64,...>`. A function with no type
    arguments keeps its name, so non-generic programs compile to the same
    bytes as before.
  - A call whose type arguments are not all concrete is a typed
    `Unsupported::Generic`.
- Regions lower to plain pointer words.
  - `RegionBox[r, T]` is one word: the heap word index of its payload.
  - Every `r: Region` entry of a function's row becomes a hidden capability
    parameter: one pointer word to the region's header, placed after the
    ordinary parameters. A call passes the caller's capability with the
    same label, which is how the checker resolves it.
  - A `region r(N) { .. }` block is an `Op::Handle` of the built-in
    `Region` effect. Lowering opens a header in the heap, calls the body's
    nested function with the captured places, the enclosing capabilities
    and the new one, then closes the header. The identity return clause is
    skipped. A body that assigns a place it captured by value is refused.
  - `r.box`, `r.get`, `r.set` and `r.used` follow
    `vesper.region.accounting/1`. A payload charges
    `slot_bytes(Layout::of(T).size)` bytes against the block's capacity,
    and a block allows at most `MAX_ALLOCATIONS` allocations. Payload words
    are the flattened words of `T`.
  - `take`, `checkpoint`, `rewind` and `commit` are typed
    `Unsupported::Effect` refusals.
- The program heap.
  - Linux ELF executables that use regions get a second, zero-fill RW
    `PT_LOAD` of 512 MiB at `HEAP_BASE = 0x1000_0000`. The code moves to
    file offset `0x100`, because two program headers do not fit below
    `0x80`.
  - Heap word 0 holds the words in use, word 1 the innermost open header
    and word 2 a pin. Payloads and headers are bump-allocated.
  - Closing a block resets the words in use to where the block started.
    The exception is when an outer region allocated while the block was
    open: then it resets to the pin instead, so it never frees live words.
  - Exceeding a region's bound, its allocation count or the heap aborts
    with the new `TrapKind::Region`, exit status 104 (`EXIT_REGION`).
    `FIRST_FREE_LABEL` is now 4.
  - macOS and Windows outputs refuse programs that use the heap with a
    `VerifiedError::Target` error, because their containers do not map it.
- Slot IR and machine.
  - New rows `LoadPtr { dst, ptr, offset }` and
    `StorePtr { ptr, offset, src }`.
  - New instructions `LdH(off)` (`r0 := heap[r1 + off]`) and `StH(off)`
    (`heap[r1 + off] := r0`), encoded for all three ISAs:
    - x86-64: `mov r11d, HEAP_BASE` and `[r11 + rcx*8 + disp32]`;
    - AArch64: `movz x16` / `add x16, x16, x1, lsl #3` / `ldr|str [x16, #imm]`;
    - RV64: `addi`/`slli` builds `1 << 28` in `t0`, then `slli`/`add`, and
      `ld|sd` (offsets of 2 KiB or more go through `t1`).
- Lean.
  - `VerifiedCodegen.lean`:
    - `Loc.heap`, `Failure.region`, the rows `loadPtr`/`storePtr` and the
      instructions `ldh`/`sth` with their semantics;
    - the table rows, and `sim_loadPtr`/`sim_storePtr`, now cases of
      `select_simulates`;
    - a pointer is the machine word its slot holds, read as a word index.
  - `CodegenRv64Encoding.lean`:
    - models the new RV64 sequences, and `heap_ok` proves them well formed,
      so `emit_decodes` covers them;
    - 65 listing samples (with `trap region`, `ldh 3`, `sth 3`, `ldh 300`
      and `sth 300`) match `riscv64::encoding_listing()`.
  - `CodegenA64Encoding.lean` (milestone 6, merged from main):
    - models the AArch64 `ldh`/`sth` sequences, and maps `trap region` to
      label 3;
    - `emit_ok` covers them (heap offsets below 4096, which
      `MAX_PAYLOAD_WORDS` guarantees), so `emit_decodes` does too;
    - its 67 listing samples match `aarch64::encoding_listing()`.
  - `CodegenX64Encoding.lean` (milestone 9, merged from main): new
    instructions `movR11` (`41 BB id`) and `heapLd`/`heapSt` (`49 8B|89 84
    CB disp32`) round-trip through the specification decoder. `ldh`/`sth`
    and `trap region` are in `emit`, `emit_ok` and the listing (69 lines).
  - All three encoding models set `firstFree := 4` (`TrapKind::Region` is
    label 3).
  - `CodegenValidation.lean`:
    - a heap load gives no range fact, and a heap store forgets every heap
      fact;
    - `transfer_sound` covers both rows.

### With `Fail` effects (milestone 7)

- A region block's body is planned like a call: its failures pass through
  the block. A failure that leaves the block (the status protocol returning
  to the caller) closes the block first, so the next allocation sees the
  right innermost region and heap top. `region_fail.ves` raises out of a
  region block under a `handle Fail[E]` and agrees with the evaluator.
- Region exhaustion is not a `Fail` effect: it always exits with 104 and no
  trap scope redirects it.
- Handlers in generic functions are resolved per instance (the handler
  table is keyed by instance and span).

### Checks

- `tests/verified_codegen.rs`, `generics_and_regions_agree_with_the_evaluator`:
  - Fixtures, each compiled with and without the IR optimizer:
    - `generics.ves`: generic structs and enums, generic recursion, and a
      generic calling a generic;
    - `regions.ves`: region-polymorphic functions with struct and generic
      payloads, recursion through a region-polymorphic function, `used()`,
      an inner block that allocates in the outer region, and a block per
      call;
    - `region_exhausted.ves`: the fourth 8-byte box in a 24-byte region;
    - `syntax_pages.ves`: the self-hosted front end's radix pages,
      `syntax_directory[T]`, `syntax_words`, `syntax_read_0` and
      `syntax_write_0`.
  - Results:
    - All four agree with the evaluator on x86-64 (native), AArch64 and RV64
      (QEMU 8.2.2).
    - The output is deterministic.
    - The macOS and Windows targets give the typed heap error.
  - Cranelift is compared only on `generics.ves`. It refuses recursion
    through region-polymorphic functions: "recursive effect specialization
    exceeds 64 inline frames".
- Lean 4.35.0:
  - `VerifiedCodegen`, `CodegenRv64Encoding`, `CodegenComposition`,
    `CodegenControlFlow`, `CodegenValidation` and `CodegenFlowValidation`
    compile with no warnings;
  - the axiom audits use only `propext`, `Classical.choice` and
    `Quot.sound`;
  - `cargo test --test verified_codegen_lean` passes with `VESPER_LEAN` set.

### Remaining gaps

- The heap exists only in the Linux ELF executables. Mach-O and COFF need a
  zero-fill section and relocations.
- The RV64 semantics proof (`CodegenRv64Semantics.lean`, #862) relates
  only the stack frame. `ldh`/`sth` and the `loadptr`/`storeptr` rows are
  outside `emit_sim`/`row_sim` (`DataM`, `FrameRow`) until `Corr` also
  relates the heap at `HEAP_BASE`. That extension also needs the frame to be
  disjoint from the heap, and heap indices to stay below `HEAP_WORDS`.
  `CodegenFailScope.lean` counts `sth` as a store.
- The Lean heap is unbounded. The physical bound (512 MiB) comes from the
  allocator rows that lowering emits; it is enforced at run time and tested,
  not proved.
- Physical words are reclaimed only when a block closes and no outer region
  allocated while it was open (the pin). Programs that allocate in an outer
  region from inside an inner block keep those words until the program
  exits.
- Constant tuple indexing (`page.slots[0]`) and `handle SyntaxMemory` belong
  to lane V3, which also takes `syntax_run`'s closure parameter.

## Milestone 10: front-end coverage driver and dynamic tuple indexing (#838)

### What was built

- `verified_codegen::coverage` plans every function of a program at once
  (`lower::plan_with` with several entries in a tolerant mode) and lowers
  each in a coverage mode that records each refusal and keeps going. The
  inventory therefore has every refusal, not just the first one `compile`
  returns.
  - A function that cannot be planned is recorded and left out, not fatal.
  - A function refused only for its effect row is still planned and
    lowered, so its body's refusals are listed too.
  - Every raising function uses the status protocol and the entry-only
    checks are skipped, because no function is the entry.
  - A function *compiles* when it has no refusal of its own and everything
    it calls (including the nested functions of its `handle` expressions)
    compiles. Calls to a refused function are listed as such, not as a
    refusal of the caller.
- `tests/verified_codegen_front_end.rs` runs it over the three units of
  `packages/vesper-compiler`: `stage1` (the five Loom.toml files that
  `bootstrap/stage1.rs` builds), `parser` (`parse_*`/`syntax_*`) and
  `checker` (the twelve files of `tests/self_host_check.rs`), each with
  the generated input provider. The result is checked in as
  `docs/parallel-results/verified-codegen-front-end-inventory.txt`
  (`VESPER_BLESS=1` rewrites it).
- Dynamic tuple indexing `(a, b, ...)[i]` (the parser's `syntax_text_*`
  byte tables, `page.slots[N]` and the generated `source_leaf_*`
  providers) lowers to a first-match chain over the index (`Const`,
  `Compare`, `BranchZero`, `Move`, `Jump`), so the Lean table needs no new
  rows. Elements may be several words. Out of range raises
  `RuntimeError::IndexOutOfBounds` through Milestone 7's
  `raise_runtime_error` (an abort, exit 102, when nothing handles it).

### Inventory after this milestone (on top of Milestones 7 and 8)

Coverage monomorphizes from every non-generic root (Milestone 8's
`mono::monomorphize`) and plans the union.

| Unit | Functions | Own refusals | Remaining constructs |
| --- | --- | --- | --- |
| stage1 | 161 | 0 | none: the Stage 1 front end compiles |
| parser | 436 | 122 | `SyntaxMemory` (118 functions and its two operations), `syntax_run`'s closure argument and its generic instance |
| checker | 298 | 0 | none |

No structural equality, string or float refusal occurs anywhere in the
front end.

Merging Milestone 8 also fixed main: `certificate.rs` (#854) matched `Lir`
exhaustively and did not know `LoadPtr`/`StorePtr`. The Rust mirror now
follows `CodegenValidation` (a heap load leaves no fact). The region
fixtures join the certificate test's imprecise list, because the bump
allocator's checked arithmetic reads heap words that carry no range fact.

### Checks

- `cargo test --test verified_codegen dynamic_tuple_indexing`: every
  element, both ends, negative and huge indices, `u8`/`u64` indices,
  multi-word elements and a handled `IndexOutOfBounds`, on x86-64,
  AArch64 (QEMU) and RV64 (QEMU), each with and without the IR
  optimizer, against the evaluator and Cranelift.
- `cargo test --test verified_codegen_front_end`: the inventory matches.

## Milestone 11: tail-resumptive handlers and function-value arguments (#838)

### What was built

`verified_codegen::evidence` rewrites the monomorphized Place IR before
planning, so the planner and the slot lowering see only first-order calls.

- **Function-value arguments.** A call `g(f)` that passes a named function
  gets an instance `g<p=f>` with the parameter removed. Every call `p(..)`
  becomes `f(..)`, through the nested functions that capture `p`. A
  parameter used other than by calling it is left alone, and the planner
  still refuses it as a function value. Monomorphization now also reaches
  functions that are only read as values.
- **Tail-resumptive handlers.** `handle E in body with { return v => r,
  op(x, k) => c }` for a user effect `E` compiles when every clause
  resumes once, in tail position (`k(e)` is what the clause returns). The
  clause loses its continuation and returns `e`, and the handler becomes
  evidence:
  - The clauses' captures become hidden parameters (`$ev<n>.<name>`) of
    the handler body and of an instance `f<E#n>` of every function with
    `E` in its row that the body reaches.
  - The region capabilities the clauses use join those rows. The checker's
    internal `$cap<start>` labels are renamed to the block's source label.
  - `E::op(a)` becomes `clause(evidence.., a)`; arguments a clause binds
    as `_` are dropped.
  - The `handle` instruction becomes a call of the body, then of the
    `return` clause.
  A clause may bind the resumed value before returning it
  (`let v = k(e); v`).
  A clause that does not resume, resumes twice or outside tail position,
  captures its continuation, or performs the handled effect is refused
  with a typed `Effect` error. So are a stateful or labeled handler and a
  handler of the same effect nested inside another.
- **Clause failures escape the body.** A clause runs in the handler's
  context, so a `handle Fail` between the handler and the operation must
  not catch what the clause raises. Each handler site gets its own escape
  code for every failure code (same payload). After a call to a clause,
  the caller rewrites the status to the escape code (`Plan::recode`, one
  `Const`/`Compare`/`BranchZero` chain per code). After the body call, the
  handler site restores the original code. The raise-set fixpoint applies
  the same maps. No new `Lir` or machine instruction is needed.
- A handler body or clause that assigns a local it captures by value is
  refused. This applies to `handle Fail` as well: Milestone 7 silently
  lost such a write (`x = 50` inside the body left `x` at its old value).
- Coverage no longer treats an effect-polymorphic function as an entry.
  Its handler instances are planned instead. A function whose effect no
  handler in the program instantiates is listed as such.

### Inventory after this milestone

| Unit | Functions | Own refusals | Remaining constructs |
| --- | --- | --- | --- |
| stage1 | 161 | 0 | none |
| parser | 434 | 2 | `parse_transport` and `syntax_byte`: entries whose `SyntaxMemory` only the host (the Runtime's external handler) handles |
| checker | 298 | 0 | none |

Every function `parse_tree_main` reaches now compiles, including
`syntax_run`'s `handle SyntaxMemory` and its `consume` argument (116
`<SyntaxMemory#0>` instances).

### Checks

- `cargo test --test verified_codegen tail_resumptive_handlers` covers:
  - a capture of the handler;
  - an operation performed in a callee;
  - a generic function given the body as a function value;
  - recursion through the effect, a `_` argument, and a `return` clause;
  - a clause failure that skips an inner `handle Fail`, and a body
    failure that the inner handler still catches;
  - a non-tail resumption being refused.
  It runs on x86-64, AArch64 (QEMU) and RV64 (QEMU), with and without the
  IR optimizer, against the evaluator, and against Cranelift except for
  the recursive case.
- `cargo test --test verified_codegen_front_end`: the inventory matches.
- The whole structured parser, with `fn main() -> i64 { parse_tree_main() }`
  and input `fn f() -> i64 { 1 }`, built for x86-64 with the code limit
  raised by hand: 1,699,167 bytes of code. Its exit status (197) is the low
  byte of the evaluator's result (8801903996191579333).

### Remaining gap: code size

Closed by Milestone 12.

## Milestone 12: long branches (#894)

### What was built

- **Trap exits are copied.** AArch64 `cbz`/`b.cond` and RV64 `jal` reach
  only ±1 MiB, and every checked operation branches to a shared trap exit.
  The emitter now places a fresh copy of the exits between functions once
  256 KiB of code has passed since the last copy (`EXIT_SPACING`). The
  assembler records every copy of a label (`Asm::bind_copy`) and resolves
  each fixup to the nearest one, so a trap branch never travels more than
  about 256 KiB plus the length of its own function.
- **RV64 far calls.** `M::Call` and the `_start` call are now
  `auipc ra, hi; jalr ra, lo(ra)` (`Fix::AuipcJalr`, with
  `hi = (rel + 0x800) >> 12` and `lo = rel - (hi << 12)`), which reach
  ±2 GiB. AArch64 calls were already `bl` (±128 MiB), and x86-64 uses rel32.
- **Limit.** `max_code_bytes` is now 64 MiB, which keeps every AArch64 `bl`
  in range. A single function longer than 1 MiB still fails with a typed
  `Limit` error from the assembler, not a bad encoding.

### Lean

- `lean/CodegenRv64Encoding.lean` adds the U-format `Inst.auipc` with its
  decoder, encoder, well-formedness, and a `decode_encode` case. It also
  adds the `farHi`/`farLo` layout items, and `.call` is emitted as the pair.
  `place_wf` covers both halves: `hiOf` and `loOf` keep the immediates
  within the U and I ranges.
- `lean/CodegenRv64Semantics.lean` gives `auipc` its meaning
  (`rd := base + pc + imm * 4096`) and extends `exec_shift`/`placeL_shift`
  to the new items, so `run_sim`/`row_sim` hold as before. The AArch64
  model does not change: exit copies are just more copies of the same
  instructions.
- The Rust RV64 listing still matches the Lean `listing`
  (`tests/verified_codegen_lean.rs`).

### Checks

- The whole structured parser (input `fn f() -> i64 { 1 }`) now builds
  under the default limit and exits with 197 on every target:
  - x86-64: 1,698,911 bytes of code;
  - AArch64 under QEMU: 974,504 bytes;
  - RV64 under QEMU: 5,086,616 bytes.
- `cargo test --test verified_codegen`, `verified_codegen_fail_front_end`,
  `verified_codegen_front_end` (the inventory is unchanged) and
  `verified_codegen_lean` with Lean 4.35.
