# Verified direct code generator

Wave-eight lane 2 ([prompt](../agent-prompts/wave-eight/02-verified-direct-codegen.md)), claim #451. Design §42.1 pillar 3b and §7.

- Branch `claude/project-thread-oftl51`. It started from `1094e64` (a descendant of the required baseline `ea1ed29`), and `origin/main` was merged in before validation. The PR closes #451.
- Commits: the backend, tests and Lean model; the root patch (`src/lib.rs`, one line); the `src/main.rs` routing hunk; this report.

## What was built

`vesper build --codegen verified [--target T] [--no-opt] [--linker L] [-o OUT] FILE` compiles a checked Vesper program straight from Place IR to machine code. Cranelift is not used anywhere in this path. It writes a static x86-64 or AArch64 Linux ELF executable, or, for AArch64 macOS, a Mach-O object linked with the system `cc`.

### Supported subset

- **Types.** `u8`, `u32`, `u64`, `i64`, `bool` and `Unit`, plus structs (after generic substitution) and tuples of these, nested to any depth.
- **Arithmetic.** Checked `+ - * / %`, compound assignment and negation follow `vesper.arith.checked/1`:
  - overflow raises `ArithOverflow`;
  - division and remainder test for zero first, then `i64::MIN / -1` raises overflow, and `i64::MIN % -1` is 0.
  - `wrapping_add/sub/mul` are total.
- **Other operations.** Comparisons, `!`, `if`, and `let` with irrefutable tuple and struct patterns.
- **Calls.** Direct calls, including recursion. A self call in tail position becomes a jump, so loops (which Vesper writes as tail recursion) run in constant stack.
- **`inout` arguments.** These are copy-in/copy-out.
- **Effects.** Only unlabeled `Fail[ArithOverflow]` and `Fail[RuntimeError]` are allowed. The checker already forbids `Fail` from crossing an active `inout` parameter, so an abort can never lose a write-back.

### Result protocol (`vesper.verified-codegen/1`)

- The entry's result words go in a caller-reserved area. By default, the exit status is the low byte of the first word. With `ResultProtocol::Words`, the stub writes every result word to stdout and exits 0.
- An unhandled failure exits with `101` (overflow), `102` (division by zero) or `103` (unreachable).
- `Outcome::from_exit` and `decode` turn the exit status or output words back into a `runtime::Value`.

### Pipeline (`src/verified_codegen/`)

1. **`lower.rs`** plans the functions reachable from the entry and flattens each value into 64-bit frame words (`Shape`). It turns each Place IR instruction into rows of the slot IR `Lir`, and pre-allocates a slot for every block parameter and every instruction destination.
   - Anything outside the subset is rejected with a typed `VerifiedError::Unsupported { construct, function, detail }`. The `construct` field is one of: Float, String, Enum, Match, Index, FunctionValue, Generic, Effect, ExternalFunction, StructuralEquality, Type, Operation.
   - The match on `Op` ends in a wildcard arm that returns `Operation` for any IR operation this backend does not know. That arm already covers `Op::Vector`, which was added on main while this lane was in progress.
2. **`machine.rs`** defines `select`. This is a *total* function from one `Lir` row to a fixed sequence of abstract target instructions `M`, using three registers `R0`, `R1` and `R2`.
   - `table_rows()` lists one representative of every row kind, 82 rows in all.
   - `table_listing()` prints them in the exact text format that the Lean file prints.
3. **`x86_64.rs` and `aarch64.rs`** give each `M` a fixed encoding. `R0`, `R1` and `R2` are `rax/rcx/rdx` on x86-64 and `x0/x1/x2` on AArch64. Each file also has an entry stub with the abort exits. `asm.rs` resolves label fixups (rel32, imm26, imm19).
4. **`container.rs`** writes the output container, either a minimal static ELF `ET_EXEC` (one `PT_LOAD`, no libc) or a Mach-O `MH_OBJECT` that exports `_main`.

`--no-opt` skips `ir_opt`. Output is deterministic: the tests compile every case twice and compare the bytes.

### Public API (`vesper::verified_codegen`)

- **Constants:** `CODEGEN`, `ABI`, `EXIT_OVERFLOW`, `EXIT_DIVISION_BY_ZERO`, `EXIT_UNREACHABLE`, `MAX_FRAME_WORDS`.
- **Types:**
  - `Arch`, `Os`, and `Target`, which has `X86_64_LINUX`, `AARCH64_LINUX`, `AARCH64_MACOS`, `from_triple` and `host`;
  - `ResultProtocol`, and `Options`, which carries limits on functions, rows and code bytes;
  - `Unsupported`, `VerifiedError`, `Container`, `Artifact` and `Outcome`.
- **Functions:** `compile(world, entry, &Options)`, `decode`, `build_command` (used by the CLI), and `select`, `table_rows` and `table_listing`.
- **Re-exported IR types:** `Lir`, `M`, `Kind`, `Loc`, `Slot`, `Label`, `Reg`, `Sign`, `ArithOp`, `WrapOp`, `Cond`, `TrapKind`.
- **Limits:** these are enforced as typed `VerifiedError::Limit` errors, never panics. Frames are capped at 4000 words, and code at 1 MiB so that AArch64 imm19 branches always reach.

## What the Lean file proves

`lean/VerifiedCodegen.lean` has no imports and builds on core Lean 4.24.0 alone. It defines three models:

- **The slot IR model.** Words are `Int`s in `[0, 2^64)`, a store maps slots to values, and `lirStep` gives the checked arithmetic denotation, including the abort cases.
- **The machine model.** It has three registers and word memory. `Mach.step` uses hardware-style overflow and carry checks, and models a `fault` for division by zero and `MIN / -1`.
- **The table.** `select` in Lean mirrors the Rust table row for row.

The state relation is `Rel s m := ∀ l, m.mem l = enc (s l)`. Three theorems are proved for every typed input store:

- **`select_simulates`.** For every data row `r` (constants, moves, argument and result copies, every arithmetic, wrapping, negation, `!` and comparison row, for every kind), running `select r` from any machine state related to `s` gives one of two outcomes. Either the run succeeds in a state related to `lirStep r s`, or both sides raise the same failure (overflow or division by zero). The machine never reaches its `fault` state, which shows that the zero and `MIN / -1` checks always run before the division.
- **`select_branch`.** The branch row reads the same condition as the slot IR.
- **`select_control`.** Label, jump and trap rows select to the expected instructions.

The per-row lemmas (`sim_arith`, `sim_wrapping`, `sim_neg`, `sim_not`, `sim_compare` and others) and the arithmetic facts (`tdiv_fits_i64`, `toS_mod_eq` and others) are all checked by `#print axioms`. They depend only on `propext`, `Classical.choice` and `Quot.sound`. The file contains no `sorry`, `admit`, `axiom`, `native_decide` or `bv_decide`.

The Lean file prints its table between `BEGIN TABLE` and `END TABLE`. `tests/verified_codegen_lean.rs` checks that this output is byte-for-byte the Rust `table_listing()`.

**What this is not.** It is a theorem about the Lean models, not about the Rust emitter. The following are covered only by tests, not by the proof:

- the Rust lowering from Place IR to `Lir`;
- the frame layout and calling convention;
- control flow across rows (calls, the self-tail-call jump);
- the x86-64 and AArch64 byte encodings, and the containers.

The x86-64 and AArch64 instruction semantics in the model are an abstraction of the real ISAs, so correspondence to real hardware is only tested. Rust ownership in this code is not evidence that the Vesper source typing rules are implemented. The backend relies on the checker having accepted the program.

## Checks

These were run locally on Linux x86-64 with Rust 1.100.0 and Lean 4.24.0 from the GitHub release zip. The build environment was `CARGO_BUILD_JOBS=3 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`.

- `cargo test --locked --test verified_codegen --test verified_codegen_lean --test verified_codegen_cli`, with `VESPER_LEAN=…/lean-4.24.0-linux/bin/lean`: 12 passed, 0 failed.
  - **`fixtures_agree_with_the_evaluator_and_cranelift`.** Each of the eight fixtures in `tests/fixtures/verified_codegen/` runs on the evaluator and on Cranelift (the JIT, using a status wrapper for failing programs). It also runs as a verified-codegen executable for each target, both with and without `ir_opt`, and every run must give the same `Value` or failure kind. The fixtures cover recursion, loops, nested structs, `inout`, every kind, overflow, division by zero and Unit.
  - **`checked_arithmetic_boundaries`.** Over 90 generated programs cover every operator and kind at the overflow boundaries, the corner cases of `MIN / -1`, `MIN % -1`, `x / 0` and `x % 0`, and the narrow-kind wrapping masks. The results are compared with the evaluator.
  - **`constructs_outside_the_subset_are_typed_errors`.** Floats, strings, enums, `match`, generic functions, closures, handlers (a user effect), indexing and struct equality each produce the expected `Unsupported` construct. Vector operations reach the wildcard `Operation` arm; that path has no fixture of its own.
  - **`entry_and_target_errors`**, **`containers_are_well_formed`** (ELF and Mach-O headers parsed back) and **`x86_64_code_disassembles`** (with `objdump`, when present).
  - **`tail_self_calls_are_loops`.** A 10,000,000-iteration loop runs in constant stack and returns `30000007`.
  - **Lean.** The source hygiene test, the table coverage test, and the full compile with table equality and the per-theorem axiom audit. The Lean step takes about 34 s.
  - **CLI.** `vesper build --codegen verified` builds and runs a program on the host (exit status `6765 % 256`; an overflowing program exits 101). Bad flag combinations are rejected (`--codegen` without `build`, an unknown codegen, `--verify` or `--tee`), and so are unsupported programs.
- **Execution.**
  - **x86-64 Linux** ran natively.
  - **AArch64 Linux** ran under `qemu-aarch64-static` (QEMU 8.2 from Ubuntu). This validates the program under QEMU's AArch64 model only.
  - **AArch64 macOS** reported unavailable locally, because the Mach-O output can only be linked and run on a Mac. The macOS (AArch64) CI job runs it there.
  - When the runner for a target is missing, the test prints `unavailable` and does not pass silently.
- **Workspace.** `cargo fmt --all -- --check`, `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings` and `cargo test --workspace --locked` were run on the merged tree. Their results are recorded in the PR.

## Decisions

- **No `object` dependency.** The prompt suggested `object` at the version already in `Cargo.lock`. It is there only as a dependency of other crates, so using it from the root package would have meant changing the root manifest. Only two tiny, fixed containers are needed (one ELF segment, and a Mach-O object with one section and one symbol), and they take about 150 lines. The tests parse both back and run them.
- **One slot per value, no register allocator.** Each `Lir` row loads its operands into `R0`/`R1`, computes, and stores the result back to its frame slot. The code is slow, but every row is local, so a per-row Lean simulation composes. Register allocation such as linear scan is left for later, and it would need its own proof of the store relation.
- **Aborts are exit statuses.** No unwinding and no runtime. A trap jumps to a stub that calls `exit(101/102/103)`. This matches the evaluator's failure kinds and keeps the process separate from the test harness, so emitted code is never mapped executable in-process (the lane forbids `unsafe`).
- **Mach-O is an object, not an executable.** Writing a signed macOS executable by hand is not cheap. A relocation-free `MH_OBJECT` exporting `_main`, linked by `cc`, is enough, and the stub uses `svc #0x80` with `x16` for `write` and `exit`.
- **Tail self calls only.** Self calls in tail position become jumps. Mutual tail calls stay real calls, limited by stack depth.
- **No `inout` with tail calls.** A function with an `inout` parameter never becomes a loop, because its copy-back must run on return.

## Remaining gaps

- **Unsupported constructs.** Enums and `match`, generic functions (only substituted struct types are supported), floats, strings, vectors, regions, closures, indexing, effect handlers, and every effect except unlabeled `Fail` are rejected with typed errors.
- **Not implemented:**
  - no RV64 target;
  - no register allocation, and no peephole optimization beyond `ir_opt`;
  - no Windows PE output.
- **Unproved.** None of these are proved: the lowering, the encoders, the containers, the frame and calling convention, and control flow across rows. A next step is to model the encoders against an ISA specification.
- **Not addressed.** WCSD stack bounds (the frame size is capped, but there is no static stack-depth proof) and fiber trampolines.
