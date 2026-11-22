# Self-hosting SH4 lane 16: the eBPF backend in Vesper

Part of #1290 ([lane prompt](../agent-prompts/self-host-sh4/16-ebpf-backend.md)).
Stacks on #1259. The Rust backend stays the default; nothing in the
compiler calls the port yet.

## What changed

- New package `packages/vesper-ebpf-backend` (`@vesper/ebpf-backend`, about
  4,700 lines of Vesper) ports `src/ebpf_backend.rs`,
  `src/ebpf_backend/surface.rs` and `src/ebpf_backend/lower.rs` (2,400 Rust
  lines) onto the `@vesper/ebpf` port (`crates/vesper-ebpf/vesper`, its
  `insn::encode`, `helpers::allowed` and `ProgramType`) and the SH3 data
  packages (`@vesper/check-data`, `@vesper/place-ir`):
  - `asm.ves`: the part of `vesper_ebpf::Asm` the backend uses (labels,
    jump and local-call fixups with the Rust range rules, maps and their
    `LD_IMM64` relocations, subprograms, line records).
  - `extra.ves`: the task section the backend needs beyond check-data's
    tables: the options, the source text, ADT spans, `world.functions`
    (signatures, effect rows, spans) and `world.effects` (operation
    signatures). Strings are byte runs and types inline trees interned into
    the context, so the section needs nothing from the private parts of
    `tests/common/ir_words.rs`.
  - `backend.ves`: `Surface::analyze` (entry and action enum, recognised
    effects: `Packet`, `Bpf` helpers through `helpers::allowed`, maps and
    their `*_map` specification functions, `MapDef::validate`) and `lower`
    (call graph with tail self-calls as loops, recursion and stack checks,
    slot allocation with the same block-local reuse, instruction selection
    for every IR operation and terminator of the subset, line info, the
    program name, fixups). Errors are explicit state, not `Fail`: the first
    error recorded is the one Rust returns, with the same rule, function,
    span and message text.
  - `lib.ves`: the entry `ebpf_backend_words`.
- `tests/self_host_ebpf_backend.rs`: the encoder of the task section, the
  decoder of the result into `Result<Compiled, EbpfBackendError>` and the
  differential. The decoder runs the Rust verifier model on the decoded
  program (as `compile_world` does), so the `Debug` comparison covers
  instructions, maps, relocations, subprograms, line info, frames, loops,
  map bindings, the verifier report and verifier errors; accepted programs
  also compare their ELF objects (`Compiled::object`) byte for byte.
- `BUILD.bazel`: the test's data (the corpus and `//crates/vesper-ebpf:files`
  for the `@vesper/ebpf` dependency) and the `long` timeout.

Not in this PR: the stack-bound report (`src/stack_bound.rs`), the hybrid
module and the swap.

- `stack_bound`: SH4 lane 17 (#1291) owns the shared stack analysis (silicon's
  `analyze_stack` and `stack_bound::render`) in
  `packages/vesper-backend-checks`. This lane wraps it with the eBPF-origin
  checks (`ebpf.stack.kernel_limit` through `@vesper/ebpf`'s `check_stack`,
  `stack.analyzers_disagree`) once that package lands, instead of porting a
  second copy (agreed with lane 17 in the project).
- The hybrid module `ebpf_backend` and its `ebpf` hook install wait on
  lane 1's swap point (`src/codegen_swap.rs`).

## Differential

`cargo test --test self_host_ebpf_backend`, entry compiled unoptimized
(see the compiler gaps for why), Linux x86-64:

| Cases | Compared | Rust accepts | Differ |
|---|--:|--:|--:|
| Fixtures with every function as the entry, truncated fixtures (every 29 bytes), literal swaps, the out-of-subset programs of `tests/ebpf_backend.rs` and more (maps of every kind, helpers, option equality, enums, signed and unsigned scalars, tail loops, the 560-byte frame, an unbounded loop the verifier refuses), 200 generated subset programs | 243 | 137 | 0 |
| Every corpus world (`tests/fixtures`, `examples`, `packages`, `crates`), entry `main`, no source | 292 | 3 | 0 |

The same 127-case run (40 generated programs) with the entry compiled
optimized (`EBPF_BACKEND_OPT=1`) also differs in 0 cases.

Error results cover every rule: `ebpf.entry`, `ebpf.action`, `ebpf.type`,
`ebpf.effect`, `ebpf.map`, `ebpf.recursion`, `ebpf.operation`,
`ebpf.fallible`, `ebpf.call`, `ebpf.stack`, `ebpf.generic`, and the
verifier model's `loop.unbounded`/complexity errors on the decoded program.
Cases where Rust panics are skipped (none in these runs).

## Numbers

`CHECK_HARNESS_SPEED=1 cargo test --profile port-compiler --test
self_host_ebpf_backend ebpf_backend_speed -- --ignored`, Linux x86-64, entry
compiled optimized, over the 137 programs Rust accepts. Wall clock on the
cloud hosts moves by up to 30% between runs.

| What | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| The backend alone (Rust: `compile_world` minus `ir::lower_world` and the verifier model; Vesper: the entry on encoded input, reading the input included) | 6.8 ms | 24.6 ms | 3.61x |
| `compile_world` against encoded input + entry + decode + verify, ABBA, x20 | 8.22 s | 7.67 s | 0.93x |
| The same with the encoding (`ir::lower_world` and both sections) | 8.20 s | 9.04 s | 1.10x |

The verifier model (361 ms of the 383 ms `compile_world`) dominates the
whole compile, so the end-to-end ratio is near 1 while the backend alone is
3.6x of Rust. The backend share is small and noisy (it is a difference of
two larger times); the next lane part profiles it. Encoding costs 37.6 ms
with `ir::lower_world` (15 ms of it); lane 2 (#1276, encode-once swap
inputs) removes most of that once the swap lands.

Before: no Vesper backend. Rust stays the default (port is not at parity).

## Stage 1 guard

No guarded file is touched: nothing under `src/verified_codegen/**`,
`src/ir.rs`, `ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**`, and no swap point is added. The
`bootstrap-stage1` workflow runs on this branch to show fresh
`bootstrap/run.sh stage1 --reproduce --outcome` digests equal to
`bootstrap/stage1.lock`; the run is linked from the PR.

## Compiler gaps

1. **`ir_opt::optimize` on a large package.** Compiling the entry optimized
   takes 460 s in a debug build of the compiler (10 s unoptimized); every
   sample is inside egglog, which `ir_opt` runs once per block with a
   quadratic `disjoint` fact list. This is lane 5's subject (#1279); the
   differential compiles unoptimized by default so the test stays inside
   CI time.
2. **No `ChunkSeq::last`.** E1525:
   ```vesper
   fn top(in s: ChunkSeq[u64]) -> u64 {
       match s.last() {
           Option::Some(x) => x,
           Option::None => 0,
       }
   }
   ```
   Worked around with `try_get(len - 1)`.
3. **`if` as an operand of `&&`.** E0002 (`expected expression`):
   ```vesper
   fn f(a: bool, b: bool) -> bool {
       a && if b { true } else { false }
   }
   ```
   Worked around with a `let` before the condition.
4. **`effect` cannot name a parameter or function** (keyword, E0002). Not a
   bug, but the Rust code uses `effect` as a name throughout; the port uses
   `eff` and `effect_rule`.

## What's left

- The eBPF stack wrapper on lane 17's `@vesper/backend-checks` (PR #1319):
  build a `StackGraph` and `StackPol` from the lowered program, run
  `stack_bound::analyze_stack`, append the eBPF findings
  (`ebpf.stack.kernel_limit` from `@vesper/ebpf`'s `check_stack`,
  `stack.analyzers_disagree`) as report rows, then `render`, and compare
  with `src/stack_bound.rs`.
- The hybrid module and the swap through lane 1's `src/codegen_swap.rs`,
  with the hybrid build's `vesper build --target bpf` objects compared
  against the Rust build's.
- Rust stays the default until the port is at parity in function and speed
  (owner rule); deleting the Rust module is the owner's call.
