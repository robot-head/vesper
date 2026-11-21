# Self-hosting SH4 lane 17: backend checks in Vesper

Claim #1291. Stacks on #1259 (base `8ac0f94`, its head). Prompt:
[17-backend-checks.md](../agent-prompts/self-host-sh4/17-backend-checks.md).
Rust stays the default: nothing here replaces or deletes Rust code.

## What changed

This is part 1 of the lane: the four Stage-5 verifiers of
`src/native_verifiers.rs` over the Cranelift functions native lowering
emits, and the report they print.

- **`packages/vesper-backend-checks`** (new, on `@vesper/check-data` and
  `@vesper/resonance`):
  - `input.ves`: the decoder of the input (below) into row tables: functions,
    values, blocks, instructions, a list pool, the world and capability
    facts and the policy.
  - `consttime.ves`: `vesper_vault::verify_constant_time`: the abstract
    state (secret bit, provenance, slot set), the forward fixpoint over block
    arguments and `try_call` returns, and the per-(instruction, rule) checks.
  - `stack.ves`: `measure_emitted`: the x86-64 and AArch64 prologue decoders
    over the compiled bytes, call edges, unresolved indirect calls and
    external symbols.
  - `stack_bound.ves`: silicon's `analyze_stack` (Tarjan, worst-case depths,
    worst paths, budget and recursion findings) and `stack_bound::render`,
    word-level so SH4 lane 16 can wrap it with the eBPF-origin checks.
  - `realtime.ves`: resonance's `call_graph_from_clif` (loop detection,
    callee resolution, symbol and effect tables); the confinement check is
    `@vesper/resonance`'s `verify_realtime`, already ported (P3b lane 9).
  - `detfloat.ves`: `vesper_kinetic::detfloat::audit` (FMA and libm calls,
    reduction shapes, NaN escapes).
  - `verify.ves`: `verify_emitted`, the name table, the capability
    verifiers and `policy_from_capabilities` (`derive_policy`, including the
    explicit `--verify=detfloat` case).
  - `lib.ves`: `render` (with `file:line:col` and the item-start scan) and the
    entries `backend_verify`, `backend_verify_count`, `backend_verify_decode`.
- **`tests/common/backend_checks_input.rs`** (new): the encoder. Everything
  Cranelift computes is input: the CLIF tables with opcode properties and
  `analyze_call`, the machine code compiled with the object ISA (as
  `measure_emitted` compiles it) and `verify_function`'s verdict. The
  verifiers' logic is not. It also prints Rust's report in the same dump
  format.
- **`tests/self_host_backend_checks.rs`** (new): the differential.
- `BUILD.bazel`: the test's data (anchored globs plus
  `//crates/vesper-resonance:files` for `@vesper/resonance`) and the `long`
  tag.

- `tests/fixtures/self_host/driver.rs`: one line keeps the package out of
  the Stage 1 parser's corpus, like the other packages that use `use`
  (#880); its small files have no loops, so the loop filter kept them in.

The package is not a port of a crate with a seed, so it does not join the
port harness; its check is the differential test.

## Differential

Every run emits a program with the real backend, runs Rust's verifiers and
the Vesper package on the result and compares the dumps byte for byte:
`render`'s text, then every report field (verifiers, function count, notes,
findings with spans and locations, stack roots and stack findings).

`cargo test --profile port-compiler --test self_host_backend_checks
-- --include-ignored` (Linux x86-64):

| Test | Runs | Differences |
|---|--:|--:|
| `verifier_fixture_reports_match_rust`: `tests/fixtures/integration_b/verifiers.ves`, 4 entries x 2 ISAs x optimize on/off x 14 policies (every policy of `tests/native_verifiers_cli.rs`, plus loop bounds, DetFloat options and an all-verifier policy) | 224 | 0 |
| `capability_reports_match_rust`: the capability fixtures (accept, reject, lane 15's accepted programs) through the capability policies, with and without an explicit DetFloat request and a narrowed base | 108 | 0 |
| `corpus_reports_match_rust`: 145 corpus programs with a `main`, 2 ISAs x optimize on/off, all verifiers under a synthesized policy and the capability path | 1,160 | 0 |
| `optimized_package_agrees` (ignored): the optimized JIT build of the package on the fixture | 28 | 0 |

The non-ignored tests run the unoptimized JIT build of the package (see the
first compiler gap). The same three tests pass in the debug build.

## Speed

ABBA, best of six, 5 rounds over the 145 corpus programs (x86-64,
optimized emit), `port-compiler` profile, optimized package:

| Verifiers | Rust | Vesper (check) | Ratio | Vesper decode |
|---|--:|--:|--:|--:|
| all four | 514 ms | 76 ms | 0.15x | 98 ms |
| without stack | 66.4 ms | 61.6 ms | 0.93x | 93 ms |

The first row is not like for like: Rust's stack verifier compiles every
function to machine code, which the Vesper input carries precomputed. The
second row is: the verification alone runs at 0.93x of Rust. Decoding the
word input costs more than the checks (93 ms), so a run from the encoded
input is 2.3x of Rust. The decode goes away with the swap, which reads
Cranelift's tables through lane 1's lent handles instead of a copy.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `src/ir_opt*`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed, and no
Rust under `src/` changed at all: the diff is a new package, two test files,
this report and `BUILD.bazel`. The bootstrap-stage1 workflow's lock is
unaffected for the same reason (see the PR for the run).

## Compiler gaps

1. **`ir_opt` takes 27 minutes on the package in a debug compiler.** The
   reachable program is 901 functions, 8.8 MB of IR JSON; `vesper emit
   --emit ir` takes 4 s with `--no-opt` and does not finish in 5 minutes
   with the optimizer (egglog's `run_rules` in every sample).
   `WordFunction::compile(.., true)` took 1,599 s in a debug test binary and
   122 s under `port-compiler`, against 31 s and 2.7 s unoptimized. The
   largest functions are 250-370 kB of IR each (resonance's
   `pdc::p_compile`, `realtime::verify`, `analyze_stack`, `derive_policy`,
   `read_fn`). SH4 lane 5's native-path optimizer policy (#1279) is the fix;
   until then the differential runs the unoptimized build.
2. **Constants are not importable on this base.** `use self::input::{FW}`
   fails with E0111 for a `pub const`. Main has importable constants
   (#1239); #1259 does not yet, so each module carries copies of the
   constants it uses. They become imports when this branch merges main.
3. **Shift amounts are `u32` and variable shifts are partial.**
   `wrapping_shl_u64(1, n)` with `n: u64` is E0200, and `x >> n` with a
   variable `n` raises `Fail`:
   ```vesper
   fn bit(n: u64) -> u64 { wrapping_shl_u64(1, n) }        // E0200
   fn byte(w: u64, i: u64) -> u64 { (w >> (i * 8)) & 255 }  // E0401 Fail
   ```
   Every shift needs `wrapping_u32(..)` and `wrapping_shr_u64`.
4. **Division by a variable needs a handler.** `rows.len() / width` raises
   `Fail[RuntimeError]` even behind `if width == 0 { return 0; }`, so
   `util.ves` has a `dv` helper with its own `handle Fail`.
5. **No bitwise not on integers.** `!x` with `x: u64` is E0200 (expected
   `bool`); the port writes `x ^ 18446744073709551615`.
6. **Debug native lowering overflows a 2 MB stack** on some corpus
   programs (`Codegen::expr_at` -> `checked_arithmetic` -> `ir_graph_at`
   recursion), so the corpus test runs on a 512 MiB thread. This is Rust's
   lowering, not the package.

## What's left

- Part 2: coverage instrumentation through lane 1's lent builder, batch
  builds and resource traces.
- Part 3: the hybrid `backend_checks` module and its `verifiers` hook
  install (the swap), the full suite on the hybrid build, and the SH4
  scoreboard row.
- Importing the constants once #1239 is in the base.
