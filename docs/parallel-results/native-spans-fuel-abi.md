# Native spans, fuel and the capability-passing ABI (wave seven, lane 23)

Claim: #346 (§7 Stage 5, §29). Branch `claude/project-thread-dq8122`, based
on `main` at `283c811` (a descendant of the required baseline `c025b9a`).

The native backend now attaches source locations to the Cranelift
instructions it emits, and the Stage-5 verifiers and the coverage maps use
them. Instrumented fuzzing code has a fuel and stack bound, so a
non-terminating input stops with a typed result. Functions that mix a
capability with `Fail` are compiled once with a versioned
capability-passing ABI. Capability functions that take a top-level function
as an argument get an instance of their own. Generic ConstTime functions are
checked at each instance. The batch and session backends canonicalise NaNs
for DetFloat programs. `vesper verify --refute` and `vesper fuzz
--differential` can run on native coverage.

## What was built

| Item | Where | Tests (`tests/native_spans.rs` unless noted) |
| --- | --- | --- |
| Source locations on instructions (`CodegenOptions`, `source_offset`, `EmittedFunction::source_offset`) | `src/native.rs` | `source_locations_do_not_change_object_bytes`, `every_emitted_instruction_with_a_location_points_into_its_source` |
| Verifier findings at the instruction (`NativeFinding::location`, `render`) | `src/native_verifiers.rs` | `verifier_findings_point_at_the_instruction`; `tests/capability_verifiers.rs` (updated) |
| Coverage sites with locations (`CmpSite::location`, `EdgeSite`) | `src/native_coverage.rs` | `coverage_sites_carry_source_locations` |
| Fuel and stack bound (`NativeOutcome::Exhausted`, `set_fuel`, `set_stack_budget`) | `src/native_coverage.rs` | `non_terminating_inputs_stop_with_a_typed_result`, `fuzzing_a_non_terminating_target_reports_invalid_runs` |
| Signed comparisons (`unsigned_view`) | `src/native_coverage.rs` | `signed_comparisons_report_the_outcome_the_code_computed` |
| `select` and `br_table` logging (`SiteOp`) | `src/native_coverage.rs` | `select_and_br_table_operands_are_logged` |
| Capability-passing ABI for `[Capability.., Fail[E]..]` rows (`CAPABILITY_ABI`, `capability_abi_functions`) | `src/native.rs` | `realtime_fail_functions_are_compiled_once_and_checked_standalone`, `realtime_fail_rejections_are_found_in_the_standalone_function` |
| Function-value instances for capability functions | `src/native.rs` | `capability_functions_taking_function_values_get_their_own_instance` |
| Instance layouts (`EmittedFunction::vars`, `::instance`) and generic ConstTime checks | `src/native.rs`, `src/native_verifiers.rs` | `generic_consttime_functions_are_checked_with_instance_layouts` |
| NaN canonicalisation in the batch and session backends | `src/native_batch.rs`, `src/native_session.rs` | `detfloat_batches_canonicalize_nans`; unit test `native_session::nan_tests` |
| Result values from native coverage (`last_result`), `fuzz --native-coverage --differential`, `verify --refute --native-coverage` | `src/native_coverage.rs`, `src/crucible_commands.rs` | `refute_with_native_coverage_finds_a_computed_magic_value`, `differential_with_native_coverage_runs_both_sides_natively`, `native_coverage_cross_checks_results_with_the_evaluator` |

### 1. Source locations

`Codegen` sets Cranelift's `SourceLoc` to the start byte of the Place-IR
instruction it is lowering. Inside an instruction, each source expression
lowered through `expr` uses its own start byte and restores the enclosing
location afterwards. An inlined callee restores the caller's location when it
returns. A location is a byte offset into the checked module's source (the
all-ones value is Cranelift's "no location"). Locations show in CLIF text as
`@xxxx`. Object bytes do not change, because no debug information is emitted.

Verifiers report instruction indices in two ways: Vault and the stack
analysis use the entity index (`Inst::as_u32`), while Resonance and Kinetic
use the position in layout order. `verifier_inst` maps each back to the
instruction, and `NativeFinding::location` is that instruction's offset.
`render` prints `file:line:col` of the location when there is one, and of the
function item otherwise. For example:

```
tests/fixtures/capability_verifiers/reject/realtime_fmod.ves:6:5: error[realtime/rt-unknown-callee]: wrap_phase (inst 13): ...
```

Before this change, the location was `:5:1`, the `fn` line.

Coverage: each `CmpSite` has the location of its instruction. Each
instrumented block has an `EdgeSite` with its 13-bit label and the location
of its first original instruction, so an edge-map slot can be traced back to
source.

### 2. Fuel, signed comparisons, `select` and `br_table`

- **Fuel.** Every instrumented block decrements a fuel word. Every function
  entry also compares `get_stack_pointer` with a limit the host sets: by
  default, 512 KiB below the caller's frame. When either check fails, the
  block branches to a per-function exit that stores an "exhausted" flag and
  returns zero values. Every later block does the same, so execution ends
  after at most the rest of the current block. Each block is split after the
  check (`Layout::split_block`). `NativeCoverage::run` then returns
  `Exhausted { stack }` and drops the result. The defaults are 1,000,000
  block entries and 512 KiB of stack. In the CLI, `--fuel` also sets the
  native block budget, and an exhausted native run counts as an invalid run,
  like evaluator fuel exhaustion. The backend emits no conditional traps, and
  the trap at the end of a `match` sits in a block of its own, so the
  remainder of a block after exhaustion cannot trap.
- **Signed comparisons.** Operands are still logged zero-extended, so the
  solver sees the values the code compared. For a signed ordering comparison
  whose operands have different sign bits at their width, `feed` reports the
  mirrored kind (`Lt` becomes `Gt`, `Le` becomes `Ge`, and back).
  `Probe::cmp`'s unsigned evaluation then gives the outcome the code
  actually computed. The fix needs no change to `vesper-crucible`, which
  lane 19 owns.
- **`select` and `br_table`.** A `select` condition is logged as
  `cond != 0`, because a select hides a branch from edge coverage. A
  `br_table` index is logged as `index < cases`. `feed` also reports
  `index == k` for each of the first 256 cases, under separate site labels,
  so the solver has the case values to substitute. The source backend
  currently emits no `br_table`, and emits `select` only for closure tags.
  The test therefore instruments a hand-built function, checks it with
  Cranelift's verifier and compiles it for both ISAs.

### 3. The capability-passing ABI and function values

- **`[Realtime, Fail[E]]`.** `fail_only` now also accepts rows that mix
  unlabeled `Fail[T]` instances with capability effects. Capability effects
  have no operations, so the hidden failure-record pointer is the only
  capability such a function is passed. It is compiled once instead of being
  inlined into each handler. Record tags count only the `Fail` instances.
  When at least one function uses this ABI, the object has a local data
  symbol `__vesper_capability_abi` whose bytes are
  `vesper.native.capability-abi/1`, a newline, and one symbol per line.
  Objects without such a function are unchanged. Rows without a capability
  effect compile exactly as before. Lane 24's verifiers now check these
  functions on their own: Resonance rejects `p % q` (libc `fmod`) inside a
  `[Realtime, Fail[ArithOverflow]]` function at that function, on line 2.
- **Function values.** When a function with a non-empty, capability-only
  row takes function values, and every function-value argument at a call
  site is a top-level, non-generic function, that call gets its own
  instance, keyed for example `apply$fn[f=triple]`. The function-value
  parameters are bound at compile time and are not passed, and calls through
  them become direct calls, which the verifiers can follow. Other call sites
  (closures, captured values, function values computed at run time) are
  still specialized into their callers, as before.
- **Instance layouts.** `EmittedFunction` has a new `vars` field (the
  generic substitution) and an `instance(world)` method (the concrete
  function). The ConstTime policy now has one entry per emitted instance of
  a generic ConstTime function, and the verifier maps secret parameters to
  that instance's concrete layouts. The wave-six fixture
  `consttime_generic.ves` moved from `reject/` to `accept/`. The
  `capability.generic-unsupported` finding is gone.

### 4. NaN canonicalisation

`NativeBatch::compile` and the session backend (`isa_flags`, JIT and object)
now set `enable_nan_canonicalization` when `needs_nan_canonicalization`
holds, as object emission and the JIT already did. The batch backend used to
reject every function with an effect row. It now accepts capability-only
rows, so DetFloat programs run in the batch backend at all. With this
change, `x / x` at `x = 0.0` returns the canonical NaN `0x7ff8000000000000`
on x86-64; without it, x86-64 returns `0xfff8000000000000`.

### 5. Native coverage for refute and differential campaigns

`NativeCoverage` passes a scalar result to a result sink
(`__vesper_fuzz_result`), whose body the instrumenter replaces with a store.
`last_result()` returns that value, and native runs are now also
cross-checked against the evaluator on the value, not only on the outcome
class.

- `vesper verify --refute --native-coverage`: the contract runs as
  instrumented native code whose edges and comparison operands guide the
  search, and the evaluator's result decides. A disagreement stops the
  campaign as a crash. An exhausted native run keeps the evaluator's result.
- `vesper fuzz --native-coverage --differential <shadow>`: both sides run as
  native code. The shadow's edges and sites are labelled apart. Each side is
  cross-checked with the evaluator, and their native results (value or
  raised `Fail` instance) are compared.

Measured with 2000 executions and the default seed, for a contract and a
shadow that differ only at `key == 4660 * 4660 + 77` (computed, not a
literal of the module):

| Campaign | `--native-coverage` | evaluator-guided |
| --- | --- | --- |
| `verify --refute --contract safe` | refuted: `safe(21715677)` | no counterexample |
| `fuzz --target halve --differential halve_fast` | divergence at `halve(21715677)` | no failure |

## Acceptance

`source_locations_do_not_change_object_bytes`: for every `.ves` file with a
`main` under `examples/`, `tests/fixtures/capability_verifiers`,
`tests/fixtures/capability_effects` and `tests/fixtures/native_coverage`, on
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, object bytes
with locations equal object bytes without them. No example uses the
capability-passing ABI, and no example object contains its marker. The
existing identity tests (`native_coverage_cli`, `native_verifiers_cli`,
`capability_verifiers`) still pass.

I also checked the CLI against `main` once: for all 26 tracked `.ves` files
that emit an object (52 objects, both targets), I compared the bytes of
`vesper emit --emit object` on this branch with those on `283c811`. Every
example and fixture is identical, except
`tests/fixtures/capability_effects/accept/closures.ves`, which changes as
intended: `apply` now gets its own instance.

The ABI and function-value tests emit objects for both ISAs and run the
JIT on the host. CI runs the host tests on x86_64 Linux (BuildBuddy),
AArch64 macOS and x86_64 Windows. The `vesper build` run of the mixed-row
program is limited to Linux.

## Checks

On the cloud host (Linux x86_64):

- `cargo fmt --all -- --check`: clean.
- `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  clean. The only output is the existing unused-`serde_json` manifest
  warning in `vesper-pact`.
- `cargo test -p vesper --locked --test native_spans`: 16 passed.
- `cargo test -p vesper --locked --test capability_verifiers --test native_verifiers_cli --test capability_effects --test native_batch --test source_native_suspension --test native_suspend_abi --test native_coverage_cli --test fuzz_cli`:
  all pass.
- `cargo test --workspace --locked`: see the PR. Locally, tests that need
  `lean` on `PATH` fail for environmental reasons.

## Decisions

- **Locations are always on.** They do not change machine code, and the
  automatic capability verifiers need them without a flag. The
  `CodegenOptions::source_locations` switch exists for the identity test.
- **Fuel exits return zeros** instead of trapping or unwinding. A trap would
  kill the fuzzing process, and Cranelift-generated code has no unwinding
  here. The stack bound uses `get_stack_pointer`, not Cranelift's
  `stack_limit`, which traps.
- **The capability ABI reuses the failure record.** With no operations on
  capability effects, a second hidden parameter would carry nothing. The
  version marker is emitted only when the ABI is used, so every other object
  keeps its bytes.
- **Function values are bound at compile time** (defunctionalized
  instances), not passed as run-time code pointers. The backend resolves
  closures statically (tags), and direct calls are what the realtime and
  DetFloat verifiers can check. A run-time closure ABI with code and
  environment pointers would leave indirect calls that Resonance reports as
  unresolved.
- **Files outside the exclusive paths**, each change additive and explained
  here: `src/crucible_commands.rs` (the `--native-coverage` routing for
  refute and differential; no wave-seven owner),
  `tests/capability_verifiers.rs` (expectations for instruction locations,
  the generic fixture and the `apply` instance), and the fixture move
  `capability_verifiers/reject/consttime_generic.ves` →
  `capability_verifiers/accept/`.
- No new dependencies, no root patch, and no new unsafe code. The fuel and
  stack checks only add accesses to the existing coverage buffer, which is
  inside `InstrumentedJit::compile`'s existing safety contract, and the stack
  limit is compared, never dereferenced.

## Remaining gaps

- Effects with operations other than `Fail` (user effects with resuming
  handlers) still specialize their functions into callers. Passing handler
  closures at run time needs a handler ABI, which this lane did not design.
- Function-value arguments that are closures, captured values or computed
  at run time are still specialized into their callers, as are functions
  returning function values.
- Locations are start offsets only (no end), and the backend has one
  location per Cranelift instruction. Instructions inserted by Cranelift
  itself (legalization, register allocation) are not covered, because the
  verifiers check IR.
- The fuel bound counts instrumented block entries and applies only to
  instrumented (coverage) code. Ordinary JIT, batch and object code is
  unbounded, as before.
- The stack budget assumes the calling thread has at least 512 KiB of stack
  left. `set_stack_budget` lowers it.
- Rust ownership is not evidence for the source typing rules. This lane
  changes no source rule.
