# Self-hosting SH4 lane 7: instances, the native ABI and calls

Claim #1281. Stacks on #1259 (SH3 lane 1 part 1, `@vesper/place-ir` and
`src/ir_swap.rs`), which stacks on #1232. Prompt:
`docs/agent-prompts/self-host-sh4/07-codegen-calls.md`. Rust stays the
default compiler; nothing here changes a default build or deletes Rust code,
and no Rust lowering file is edited.

## What changed (part 1: the decisions)

Native lowering decides, before it emits any function, which monomorphic
instances an entry needs, what each one is called and how it is called.
Part 1 ports those decisions. Emission (call lowering, entries) needs lent
Cranelift builders and comes after lane 1's first PR (#1275).

- `packages/vesper-codegen-calls` (`@vesper/codegen-calls`):
  - `instances.ves`: `instances_for_entry` (the work list over function
    bodies, `world.calls` instantiations, method calls through
    `world.types`, function-value instances bound at compile time, `dyn`
    coercions and erasure, boxed-object vtables, vtable stubs, the
    references of the lowered program and `drops::drop_callees`),
    `instance_key` (including Rust's `{:?}` of the substitution),
    `concrete`, `call_vars` and the predicates `contains_function`,
    `capability_only`, `fail_only`, `capability_abi`,
    `specialized_function` and `function_value_instance`.
  - `abi.ves`: `scalar`, `signature_with_abi`, `FnAbi`, `NativeAbi::plan`
    and `internal` (the register-status Fail ABI and register results),
    `register_result_words`, the entry checks of `compile_entry` for an
    entry that handles its effects, and the symbols and signatures
    `declare_instances_with_trace` declares.
  - `data.ves`: the calls section the decisions read beyond
    `@vesper/check-data`'s world: the function table, each body as
    pre-order expression rows, `world.calls`, the `dyn` tables, the fip
    caller-slot functions and `trace_effects::active`.
  - Layouts come from `@vesper/check-layout`; the lowered program (whose
    reads decide which function values are references, and whose handle
    drops add instances) from `@vesper/place-ir`.
- `tests/self_host_codegen_calls.rs`: the differential, the test-source
  differential and an ignored timing test.
- `BUILD.bazel`: data and a long timeout for the new test.

### Where the Rust side comes from

`src/native.rs` keeps discovery and the ABI private, and port lanes do not
edit it. The test reads Rust's decisions from what it publishes:
`native::object_with_observer` hands every defined function to an observer
with its instance key, source function, symbol, `FuncId` and Cranelift
signature, in definition order, and `native::capability_abi_functions` runs
discovery alone. A compile that fails part way still checks the functions
defined before the failure (a prefix). A refusal before the first
definition is compared message for message when the package refuses too.

### Input

One input per world: `@vesper/check-data`'s names, types and world,
`@vesper/place-ir`'s program section (`ir_swap::lower`, the program
`instances_for_entry` reads), then the calls section with the tasks
(entry, optimize). The test builds it with `ir_words::Tables` and appends
its own section. `ir_words::program_input` keeps the pool-label interning
private, so the test repeats it (`pool_label`); if #1259 changes that, this
test follows.

## Numbers

Full corpus, optimized test build (`port-compiler` profile), up to 8
entries per world (`main` first, then non-generic functions in name order),
each with optimize off and on (`CODEGEN_CALLS_STEP=1 CODEGEN_CALLS_ENTRIES=8`):

| | Count |
|---|--:|
| Worlds | 288 (0 not encodable) |
| Entry builds compared | 2,306 |
| Complete compiles, every declaration compared | 1,600 |
| Compiles that failed part way, prefix compared | 2 |
| Refusals compared (entry checks, discovery errors) | 696 |
| Not verifiable (Rust failed after discovery, before its first definition) | 8 |
| Declared functions compared | 6,246 |
| **Differences** | **0** |

The test-source differential takes the Vesper programs embedded in 20
feature test files (the Fail status ABI, the capability ABI, function-value
instances, `dyn` and boxed objects, fip, floats, wide integers, handles,
handlers): 155 programs, 560 builds, 422 complete, 136 refusals, 754
declared functions, 18 capability-ABI keys, 0 differences. It covers
generic instance keys (19 distinct), function-value keys (2), register
results (32 two-word returns), `dyn` instances (13), `f32` (11) and `f64`
(20) signatures.

The CI default is every 4th world with 2 entries per world (the test
sources with 4). Most of the test's time is Rust's whole compile of each
entry, not the package: at every 8th world with 4 entries the optimized
build spent 57 s in Rust compiles and 17 ms in the package.

### Time

`codegen_calls_speed` (ignored; `CHECK_HARNESS_SPEED=1`), ABBA best of six,
all 288 worlds, 4 entries each, optimize on:

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| Discovery (Rust: `capability_abi_functions`, less one `ir_swap::lower` per entry) vs discovery plus declarations (Vesper: the entry, less the same input with no tasks) | 402.7 ms | 10.9 ms | 0.03x |
| What was subtracted: Rust's lowering vs the package decoding its input | 4.23 s | 127.8 ms | 0.03x |

The Vesper side does more work than the Rust number it is compared with (it
also plans the ABI and builds every signature). Most of Rust's discovery
time is `concrete`, which clones each function's whole body for every
instance it visits; the Vesper tables refer to the body by row and the
types by id. Encoding the input (Rust) is not counted: that is the swap
input cost lane 2 (#1276) removes. Callgrind numbers come with the swap,
when the whole compile can be measured with the family swapped.

## Stage 1 guard

No file under src/, lean/, bootstrap/ or packages/vesper-compiler/ is
touched, and nothing installs a hook, so the default compiler and the
verified backend are byte-identical to #1259's. The Bootstrap Stage 1
workflow is run on this branch (linked from the PR).

## Compiler gaps

None blocked the port. Two notes for lanes reading check-data:

1. `Known::is_integer` in `@vesper/check-data` (world.ves) predates
   `i128`/`u128` (SH2 lane 2) and leaves them out, so `Known::is_scalar`
   does too. This package keeps its own name table (`Syms`). Lanes that use
   `Known` for `numeric::is_scalar_name` should check wide integers.
2. Constants still cannot be imported across modules (check-data gap 1):
   each module repeats the tags it needs.

## What is left

- `register_locals` and `in_param_registers`, after lane 4 (#1278) merges
  its change to them.
- After lane 1 (#1275): call emission (`call_function_into`,
  `bound_function_arguments`, the Fail record and status checks at call
  sites), entry compilation and `jit_run_entry`/`WordFunction`, compared
  as CLIF text and object bytes through lane 1's harness, and the `call`
  hook in the hybrid module `codegen_calls`.
- Entry kinds other than `Handled`: `FailOnly` (freestanding images),
  `CapabilityOrFail` (verifiers) and `TraceHost` (`include_entry` and the
  trace wrapper); and the export-set boundary of `NativeAbi::plan` with
  lane 12.
- The SH4 scoreboard row.
