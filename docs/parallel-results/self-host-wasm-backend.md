# Self-hosting SH4 lane 15: the wasm backend in Vesper

Claim #1289. Stacks on #1259 (SH3 lane 1, `@vesper/place-ir`). Prompt:
`docs/agent-prompts/self-host-sh4/15-wasm-backend.md`. Rust stays the default
compiler; nothing here changes a default build or deletes Rust code.

## What changed (part 1)

- `packages/vesper-wasm-backend` (`@vesper/wasm-backend`, 7.6k lines): a port
  of src/wasm_backend that takes the checker data (`@vesper/check-data`) and
  the Place IR (`@vesper/place-ir`) and writes the core module and the WIT
  text.
  - `enc.ves`: LEB128, value types and an instruction writer with the bytes
    wasm-encoder 0.261 writes.
  - `module.ves`: `module::Builder` (types, imports, functions, table,
    memory, globals, tags, exports, element and data segments) and its
    `finish`, section by section.
  - `ir.ves`: reads the harness's wasm section (options, signatures, effect
    declarations, call-site type arguments, closure and handler facts) and
    indexes the Place IR functions.
  - `plan.ves`: `layout` (shapes, field offsets, variant slots) and
    `analysis::plan` (the instances the entry reaches, their call targets and
    row labels).
  - `lower.ves`: `lower::Lowerer` for everything but handlers: locals, cells,
    places and copy-on-write, every `Op`, terminators and tail calls, checked
    arithmetic, `$alloc`, `$eq`, `$copy`, `$str_eq`, `$fmod`, trampolines and
    the indirect-call table. Every body allocates its locals and interns its
    types in the Rust order, so the bytes match.
  - `component.ves`: `component::exports`: WIT types (`Wit::ty`), the
    canonical ABI (sizes, flattening, lift and store), the export wrapper,
    `cabi_realloc`, `$wasi:run` and the WIT source.
  - `lib.ves`: the entry `wasm_backend_compile`, the steps of
    `wasm_backend::compile` up to the core module.
- `tests/self_host_wasm_backend.rs`: the input encoder (on #1259's
  `ir_words` and `check_words`), the differential and the timing below, and
  an ignored census of what the Rust backend accepts.
- BUILD.bazel data and size entries for the new test.

### The swap (on #1309's `vesper::codegen_swap`)

- `crates/vesper-hybrid/modules/wasm_backend` (`@vesper/hybrid-wasm-backend`)
  exports `compile_words` (export ABI 4) over `@vesper/wasm-backend`.
  build.rs and BUILD.bazel link it (`//:place_ir_package`,
  `//:wasm_backend_package`), and `port/wasm_backend.rs` is its blessed
  binding.
- `src/hybrid.rs` installs the `wasm` hook. The hook encodes the input
  (`tests/common/wasm_words.rs`, now shared by the hook and the test),
  calls the module and wraps the module with wit-component into the same
  `WasmArtifact` Rust returns. An error declines to the Rust backend, so
  messages keep Rust's text (the error classes already agree, above). So
  does a program with handlers or host capabilities, which is not ported
  yet. `VESPER_HYBRID_STATS=1` prints how many programs it compiled and
  declined.
- `hook_artifact_matches_rust` (tests/self_host_wasm_backend.rs): wherever
  the hook answers, its whole artifact (`Debug`) equals Rust's. That holds
  for 200 answered compilations of a third of the cases, and the hook never
  answers where Rust fails.
- The hybrid differential (crates/vesper-hybrid/tests/differential.rs)
  gains a fourth command, `build --target wasm32-wasip2 -o {out}`, and
  compares the component bytes. Over 538 files it runs 2,152 commands per
  compiler with **0 differences**. `wasm_module_runs_from_vesper` checks
  that `examples/transaction.ves` compiles from Vesper.

### Component wrapping stays in Rust (owner call)

`component::encode` is 40 lines over wit-parser and wit-component, but what
it calls is large: wit-component 0.261 is about 16k lines (it validates the
core module, builds the component type, adapts the canonical ABI and writes
the component sections) and wit-parser about 19k. That is not "small next to
the part the backend uses", so it is not ported. The harness wraps the
port's core module and WIT text with the same calls (`wrap` in the test) and
compares the result with the Rust component byte for byte. When the swap
lands, wrapping stays a Rust call behind the hook (or a `.vsys` extern, as
the prompt allows). The owner's call (2026-11-21): keep it in Rust for now.
**Candidate SH5 lane:** port the part of wit-component and wit-parser the
backend uses (parsing the program WIT and the four WASI/Beacon packages,
embedding the component-type section, encoding the component), compared
byte for byte with the Rust components this harness already checks.

### Not ported yet (part 2)

Lexical handlers (`Op::Handle`: the dispatch, return, abort and resume
functions and the WasmFX run and op functions, CPS bodies), the WASI host
capability frames (`define_host`, `Stdout`, `Clock`, `Trace`) and evidence
binding at call sites. The port raises `K_NOT_PORTED` for these, and the
harness counts them separately. Without handlers or host frames the evidence
set is empty, so every effect operation traps as it does in Rust.

## Seed breadth 1: loops (SH6 lane 2, #1397)

The wasm seed needs the compiler packages to build to wasm, and they use
`loop`, `for`, `break`, `continue` and `return`. The Rust backend refuses
these (`loops::WASM_REFUSAL`) and is retired with Rust, so the port now
lowers them on its own:

- A body with a loop header (`Op::Loop`) or a cyclic block graph is laid
  out by the dominator-tree method of Ramsey's "Beyond Relooper" (ICFP
  2022): each block opens a `block` frame for each child in the dominator
  tree (the latest in reverse postorder outermost), and a `loop` frame
  when a back edge enters it. Every jump, branch arm and match arm is a
  `br` to the innermost frame for its target. Immediate dominators come
  from Cooper, Harvey and Kennedy's iteration.
- `return` in non-tail position (`Op::Exit` whose owner is the function
  itself) is a wasm `return`. A `return` that leaves an enclosing function
  from a handled body stays refused, with handlers.
- Loop-free bodies keep the topological layout, so their bytes still equal
  Rust's.

Tests (tests/self_host_wasm_backend.rs):

- `loops_run_like_the_evaluator`: ten loop programs (counters, ranges,
  nested `for` with `break`, `loop` values, `continue`, early `return`,
  `match` in a body, a struct across iterations, a loop in a callee) under
  the four option sets: 40 compilations, each run under Wasmtime with the
  evaluator's result.
- `port_matches_rust` counts the corpus programs Rust refuses for loops: the
  port compiles 8 of them (the 2 with a nullary `main` under the first
  option set run like the evaluator) and refuses 12 for a later construct.
  Everything Rust accepts is still byte-identical (640), with 0
  differences.
- `hook_artifact_matches_rust` accepts the hook answering a loop program
  that Rust refuses.
- The hybrid differential (crates/vesper-hybrid/tests/differential.rs)
  accepts a wasm build the default compiler refuses for loops and the
  hybrid compiles: 553 files, 2,206 runs per compiler, 0 differ, and 6 wasm
  builds with loops that only the hybrid compiles.

## Results

Cases: the programs of tests/wasm_backend.rs that define `main` and check,
then the corpus worlds with a `main` (323 in all), each under the four option
sets (optimize on/off, StackSwitching/Portable): 1,292 compilations.

| Outcome | Compilations |
|---|---|
| Core module, WIT and component byte-identical | 640 |
| Both refuse, same error kind (and construct) | 136 |
| Not ported yet (handlers, host frames) | 516 |
| Differences | **0** |
| Inputs the IR words cannot encode | 0 |

Of the 640 identical compilations, 160 also ran under Wasmtime with equal
results: those with a nullary entry, under the first option set (optimize
on, StackSwitching), whose module uses no WasmFX.

Each identical case is also run: the port's core module is wrapped with
wit-component and the result must equal the Rust component byte for byte.
Both components then run under Wasmtime 50 and their results (or traps)
must agree.

The Rust census (ignored test `rust_acceptance_census`): of the 323 cases,
272 compile in each option set. The rest fail with Entry (29),
Unsupported(Operation) (15), FirstClassContinuation (2), ExportType,
Float32, GenericOperation, InoutEffectArgument and IntegerWidth (1 each).

### Speed

Ignored test `timing` (`CHECK_HARNESS_SPEED=1`, release test build, ABBA
best of six, one 4-core cloud host): the 160 compilations above that run,
five times each, default options.

| Side | Time | vs Rust |
|---|---|---|
| Rust `compile`, less `lower_world` + `optimize` and the wit-component wrap | 135 ms | 1.00x |
| Port (JIT, optimized), from the encoded input | 36 ms | 0.27x |
| Port plus encoding its input (15 ms) | 51 ms | 0.38x |

The hybrid CLI, `build --target wasm32-wasip2` over the 538 differential
files (debug builds of both compilers, best of two, one host): the default
takes 24.2 s and the hybrid 36.6 s (1.51x). The hook compiled 82 programs
and declined 141; the rest fail before the backend. Most of the gap is the
input encoder and the second, Rust compilation of each declined program, in
an unoptimized build.

So the ported backend itself is at parity with room to spare: about 45 µs per module against
170 µs. The Rust side builds `String` keys and `BTreeMap`s for instance
names, evidence keys and type displays, where the port uses interned
symbols and dense tables. The comparison is of the ported part only:
handler-heavy programs are not in it yet. Before this lane there was no
Vesper backend, so "before" is the Rust column.

## Stage 1 guard

This lane adds a Vesper package and a test. It does not touch
src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`, lean/**,
bootstrap/**, packages/vesper-compiler/** or the Rust wasm lowering, and adds
no swap point. The [Bootstrap Stage 1 run on this branch](https://github.com/robot-head/vesper/actions/runs/44865402211) checks that a fresh `bootstrap/run.sh stage1 --reproduce --outcome` matches bootstrap/stage1.lock.

## Compiler gaps

1. **`Err(e)?` in a `Fail` function leaves the `Ok` type unresolved.**
   `vesper check` accepts it, but the native JIT refuses the whole package
   with "native layout is not yet defined for _". Workaround:
   `Fail::raise(e)`.

   ```vesper
   copy struct E { k: u8 }
   fn f(x: u64) -> u64 eff [Fail[E]] {
       if x == 0 {
           Err(E { k: 1 })?;
       }
       x
   }
   ```

2. **Constants cannot be imported** (E0111) in this base, so each module
   repeats the opcode and kind constants it uses (about 300 lines). #1239 on
   main fixes this. The port switches to imports once its base has it.
3. **`effect` and `copy` cannot be names** (`fn perform(effect: u32)`,
   `let copy = ...`). #1264 makes `effect` a contextual name.
4. **A handler clause cannot capture a plain `u32` parameter** (E0404 "must
   be owned copy"):

   ```vesper
   fn g(t: u32) -> u32 eff [Fail[E]] { if t == 0 { Fail::raise(E { k: 0 }); } t }
   fn f(t: u32) -> u32 {
       handle Fail[E] in { g(t) } with {
           return v => v,
           raise(e, resume) => t,
       }
   }
   ```

5. **`match` is not an operand of `&&`** (`a && match x { ... }` is a parse
   error). Workaround: bind the `match` to a local.
6. **A `handle` statement needs a `;`** after its `with { }` block when more
   statements follow.

## What is left

- Part 2: handlers (direct and WasmFX), host capability frames, evidence
  binding, so the 60 not-ported compilations compare too.
- A declined program pays for the encoding and the Vesper attempt before
  the Rust backend runs. Part 2 shrinks the declined set; until then the
  hybrid's wasm builds are slower than the default (below).
- A `divergent` build for the wasm module (the harness-can-fail check the
  other swaps have).
- Candidate SH5 lane: component wrapping (wit-component and wit-parser).
