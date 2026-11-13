# Crucible native coverage instrumentation (wave six, lane 23)

Claim: #265 (§29.1.1, §7 Stage 5). Branch `claude/project-thread-c81u02`,
based on `main` at `a07fbb8` (contains integration B, #295, and baseline
`cda092f`).

`vesper fuzz --native-coverage` runs the fuzz target as Cranelift code that
the backend instruments with edge counters and comparison-operand logging.
The counters drive Crucible's corpus selection and the logged operands feed
its input-to-state solver, so the fuzzer reaches a planted magic-value bug
that random inputs and the evaluator-guided mode miss. Builds that do not ask
for it emit byte-identical code.

## What was built

| Item | Where | Tests |
| --- | --- | --- |
| `Instrument` hook: rewrites each finalized function before CLIF text, the integration-B observer and codegen | `src/native.rs` (`compile`, `define_instances_observed`, `object_instrumented`) | `programs_compile_to_identical_code_without_the_coverage_hook` |
| Reusable instrumented JIT (`InstrumentedJit`, `fn() -> i64` entry) | `src/native.rs` | all `--native-coverage` tests |
| Edge counters, comparison log, argument stubs, `CoverageBuffer` | `src/native_coverage.rs` | `instrumented_code_logs_edges_and_comparison_operands` |
| `NativeCoverage` (compile once, run, feed a Crucible `Probe`) | `src/native_coverage.rs` | same |
| `vesper fuzz --native-coverage` | `src/crucible_commands.rs` | `planted_magic_value_is_found_only_with_native_coverage`, `native_coverage_writes_regression_cases_and_saves_the_corpus`, `unsupported_targets_are_typed_errors` |

Fixture: `tests/fixtures/native_coverage/magic.ves` (canonically formatted).
Tests: `tests/native_coverage_cli.rs`.

### Instrumentation

The hook is `native::Instrument = &mut dyn FnMut(&str, &mut ir::Function) ->
Result<(), String>`. It runs after `FunctionBuilder::finalize`, so the CLIF
text, the verifier observer and machine code all see the rewritten function,
and Cranelift's verifier checks it at `define_function`. Only
`object_instrumented(.., Some(hook))` and `InstrumentedJit::compile` pass a
hook; `object_with_options`, `object_with_observer`, `jit`, the native batch
and the session backend pass `None`.

`native_coverage::Instrumenter::rewrite` inserts, using one host buffer of
`u64` words whose absolute address is an `iconst` in the code:

- **Edges** (every block, at its first insertion point): AFL-style
  `map[cur ^ prev] += 1; prev = cur >> 1`. `cur` is a 13-bit BLAKE3 label of
  `(instance key, block index)`, so labels are stable across compilations;
  the map has 8192 slots. Because effectful callees are inlined into their
  callers by effect specialization, the target's code is instrumented where
  it is inlined (in the wrapper).
- **Comparisons** (before every scalar `icmp`/`icmp_imm` up to 64 bits): the
  site label and both operands, zero-extended to 64 bits (an `icmp_imm`
  immediate is masked to the operand width), are appended to a log of 1024
  records. It is branch-free: the slot is `umin(count, 1024)`, slot 1024 is
  a scratch slot, and the count keeps growing. Site labels index a host
  table with the function, `CmpKind`, signedness and width.
- **Argument stubs**: the bodies of `__vesper_fuzz_arg_<i>` are replaced by
  a load of argument `i` from the buffer.

### Running a target

`NativeCoverage::compile(source, world, target)` appends argument stubs and a
wrapper `fn __vesper_fuzz_case() -> i64` to the module. The wrapper calls the
target and nests one handler per declared `Fail[T]` instance: it returns `0`
when the target returns and `k + 1` when instance `k` is raised. The module
is checked again and compiled once, unoptimized at the IR level (so a
stub's placeholder body cannot be folded into the target), with every
function instrumented. `run(args)` stores the arguments, clears the edges,
`prev` and the log, and calls the entry.

`feed(probe)` reports every non-zero edge slot as `Probe::edge` with its hit
count (capped at 128, the top hit-count bucket) and every logged comparison
as `Probe::cmp(site, kind, lhs, rhs)`. When the target has an `i64`
parameter, each comparison is also reported in the zigzag tape encoding of
the sign-extended operands (site label with the top bit set; ordering
comparisons become equalities there, because zigzag does not preserve
order), so the solver can find signed operands in the tape.

In `vesper fuzz --native-coverage` each execution draws the arguments, runs
the native code, feeds the probe, and runs the evaluator on the same
arguments. Both must agree: native return with evaluator success passes,
native `Fail[T]` with an evaluator error is the failure `native Fail[T]`
(the evaluator's message is the detail), and any disagreement is
`native-divergence`. Evaluator fuel exhaustion is an invalid run, as in the
other modes. Evaluator coverage and the source-literal hints of integration
B are not used in this mode. `--corpus` and `--regressions` work as before;
regression cases are `// @test regression` comments plus a zero-argument
function, because the source language has no `@test` attribute yet.

## Acceptance

`planted_magic_value_is_found_only_with_native_coverage`: in `magic.ves`,
`unlock(key: u64, len: u32)` overflows only when `key` equals
`4660 * 4660 + 77 = 21715677`, which is computed and is not a literal of
the module; `gate(x: i64, y: i64)` needs `x == -81000003` and `y > 1000000`.
With 2000 executions and the default seed:

| Mode | `unlock` | `gate` |
| --- | --- | --- |
| `--native-coverage` | found after 65 executions: `unlock(21715677, 65536)` | found after 65 executions: `gate(-81000003, 1000004)` |
| evaluator-guided (default) | no failure in 2000 | no failure in 2000 |
| `--random` | no failure in 2000 | no failure in 2000 |

`programs_compile_to_identical_code_without_the_coverage_hook`: for every
example and the fixture with a `main`, for `x86_64-unknown-linux-gnu` and
`aarch64-unknown-linux-gnu`, object bytes and CLIF are identical between
`object_with_options`, `object_instrumented(.., None)`, a hook that changes
nothing, a plain compile after an instrumented one in the same process, and
the CLI's `vesper emit --emit object`. The instrumented object differs, has
edge counters, is accepted by Cranelift's verifier and generates code for
both ISAs.

The end-to-end JIT tests run on the host: x86_64 Linux (BuildBuddy and the
cloud host), AArch64 macOS and x86_64 Windows in CI.

## Checks

On the cloud host (Linux x86_64, Rust 1.97):

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -p vesper --locked --all-targets -- -D warnings`: clean.
- `cargo test -p vesper --locked --test native_coverage_cli --test fuzz_cli --test native_verifiers_cli`:
  5 + 8 + 8 passed.
- `cargo test -p vesper --locked`: see the PR.
- `vesper fmt --check tests/fixtures/native_coverage/magic.ves`: unchanged.

## Decisions

- **New unsafe code.** `InstrumentedJit` in `src/native.rs` has two `unsafe`
  blocks, the same operations `jit_with_options` and `native::batch` already
  use: transmuting the finalized entry to `extern "C" fn() -> i64` and
  calling it, and `JITModule::free_memory` on drop. They are needed to
  compile a campaign once instead of once per input (integration B's
  `--native` compiles a wrapper per input). `src/native_coverage.rs` has no
  unsafe code: the buffer is `Box<[AtomicU64]>`, written by compiled code
  only during `run` and read through atomics between runs, and the
  `NativeCoverage` field order drops the JIT before the buffer.
- **Absolute buffer address** instead of a data symbol or a hidden
  parameter, so the hook needs no module declarations and the function
  signatures stay unchanged. An instrumented object is therefore only
  useful for code-generation validation.
- **Evaluator cross-check** on every execution: native failures are only
  reported when the evaluator also fails, so a coverage campaign cannot
  report a backend bug as a program bug without saying so.
- Root patch: `src/lib.rs` adds `pub mod native_coverage;`. No new
  dependencies, no `Cargo.toml` or `Cargo.lock` change, and
  `python3 tools/bazel/sync_build_files.py` reports no change.

## Remaining gaps

- No fuel bound in native code: a non-terminating input hangs the campaign.
- Only unlabeled `Fail[..]` effects are handled by the wrapper; targets with
  other effects, `inout` or aggregate parameters are typed errors.
- Comparisons of 128-bit or vector values, `select` and `br_table` operands
  are not logged; `Probe::cmp` evaluates the "taken" bit on unsigned
  operands, so signed comparisons with negative operands may count under
  the wrong outcome as a coverage feature (the solver input is unaffected).
- Comparison sites map to source functions, not source spans: the backend
  attaches no source locations to Cranelift instructions.
- Native coverage for `vesper verify --refute` and for `--differential`
  campaigns is not wired.
- `@test` as a source attribute (still a comment marker).
