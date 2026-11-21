# Self-hosting SH4 lane 11: native lowering of the numeric families

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1285. Lane prompt:
[`docs/agent-prompts/self-host-sh4/11-codegen-numeric.md`](../agent-prompts/self-host-sh4/11-codegen-numeric.md)
(plan PR #1293). Stacked on #1259 (SH3 lane 1 part 1) at `8ac0f94`;
`origin/main` was `e73cfd6`.

## Summary (part 1)

Part 1 ports everything in this lane that needs no lent builder from lane 1
(#1275), which has no PR yet:

- **The three generated AOT runtimes**, emitted from Vesper through the
  existing Cranelift shim: the region runtime (`src/regions/aot_runtime.rs`,
  14 functions and 6 fault messages), the `TiledSeq` runtime
  (`src/tiled/aot_runtime.rs`, 12 functions and 3 messages) and the `Box`
  runtime (`src/fip/aot_runtime.rs`, 2 functions and the live counter).
  **Every runtime symbol is byte-identical to the real compiler's output:
  0 of 232 symbols differ**, with identical relocations.
- **The vector lane-shape decisions** of `src/vector.rs` (elements, shapes,
  profiles, conversions, intrinsic lookup and properties, lane errors) and
  the chunk types of `src/native_vector.rs`: **98 of 98 decision lines
  equal** to the Rust API, with optimize on and off.

Rust stays the default build. No Rust lowering changed, no hook is
installed, and no Rust code was deleted.

## Why the runtimes come first

The plan's port lanes wait on lane 1 for emission because the Rust `Codegen`
must lend its live `FunctionBuilder` to a Vesper hook. The AOT runtimes are
different: each is a set of whole functions that the Rust code defines into
the module on its own (`define(m)`, `declare_or_define(m)`), with a fresh
builder per function. They need only a module handle, which the SH0 shim
(#1126) already has. So they could be ported and compared now, and their
swap later only needs lane 1's lent module handle.

## The comparison

`crates/vesper-codegen-numeric-test/tests/self_host_codegen_numeric.rs`, test
`runtime_symbols_match_the_rust_lowering`:

- **Reference:** the real compiler, not a copy of its code. Three programs
  (a region block, a `TiledSeq` and a `Box`) compiled with
  `native::object_with_options` for `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu`, with optimize off and on. Their objects hold
  the runtimes beside the program's own functions.
- **Vesper:** `runtime_object` (lib.ves) builds a module with the same
  Cranelift flags as `isa_with_flags` (`enable_llvm_abi_extensions`,
  `is_pic`, and `opt_level=speed` when optimizing) and defines the selected
  runtimes in the order the compiler does.
- **Compared:** for each runtime symbol, its bytes and its relocations
  (offset within the symbol, target symbol, relocation kind and addend), read
  with the `object` crate. Whole objects cannot be equal yet, because the
  Rust object also holds the program; that comparison comes with the swap.

| Program | Symbols compared per configuration | Configurations | Differences |
|---|--:|--:|--:|
| region block | 20 | 4 | 0 |
| `TiledSeq` (pulls in the region runtime) | 35 | 4 | 0 |
| `Box::new` and unpacking | 3 | 4 | 0 |
| **Total** | | | **0 of 232** |

As a check that the comparison can fail, changing one constant of the port
(`MAX_CHECKPOINTS` from 256 to 257) made `vesper_region_checkpoint` differ in
every configuration.

Each port function emits the Rust function's Cranelift calls in the same
order (blocks, values, function references and global values are numbered
the same), including details that change no instruction but could change
register allocation: the region runtime seals blocks only at the end, while
the tiled runtime seals each entry block first; the region messages are read
with `symbol_value` and the tiled ones with `global_value`; the tiled runtime
re-declares `malloc` and the other libc imports after the region runtime.

The lane-shape test, `lane_shape_decisions_match_the_rust_lowering`,
compiles `shapes.ves` alone (it needs no Cranelift handle) and compares its
`vector_decisions` text with the same text built from `vesper::vector`: one
line per element (bits, signedness, bit lane, lane mask, lane, chunk and mask
chunk types as shim codes, lanes per chunk), per valid shape up to 64 lanes
(bits, bytes, chunks, layout words, required profile, the four conversions),
per profile (fixed bits, the lattice, Cranelift requirements) and per
intrinsic path (55 reserved paths and 13 that are not: lookup, shape
arguments, mask result and operand, literal positions, lane errors). The
chunk types are private functions of `src/native_vector.rs`, so the test
mirrors those three small tables; everything else is the public Rust API.

## Stage 1 guard

This PR touches none of `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`,
and no Rust source of the compiler: the diff is the new package, the new test
crate `crates/vesper-codegen-numeric-test`, this report, and its workspace
member line in the root `Cargo.toml` (with its `Cargo.lock` lines, in its own
commit). The default compiler's objects are therefore unchanged.

The test has its own crate because it links both the compiler and the
Cranelift shim, and the shim's tests already depend on the compiler. A
dev-dependency of the root crate on the shim works in Cargo but is a cycle in
Bazel (`//:vesper` and `//crates/vesper-cranelift-sys`), which failed the
first CI run. The bootstrap-stage1 workflow run on this branch is
linked from the PR.

## Compiler gaps

Each was worked around in this lane.

1. **No `while` loop.** The Rust runtime zeroes its record with
   `for off in (0..RECORD).step_by(8)`. Vesper has ranges but no `while` or
   stepped range, so the port counts words (`for w in 0..10`) and multiplies.
   Repro: `fn f() { let mut i = 0; while i < 8 { i = wrapping_add(i, 1); } }`
   is a parse error (`expected RBrace, found Lower`).
2. **`handle` is a keyword.** A variable named `handle` (the region runtime's
   box handle) is a parse error far from its cause (`expected pattern` at the
   `let`, then a cascade). The port names it `hnd`. Repro:
   `fn f(handle: u64) -> u64 { handle }`.
3. **The shim cannot ask a module what it declares** (`Module::get_name`).
   The Rust runtimes start with `if defined(m) { return Ok(()) }` and the
   tiled runtime finds `vesper_region_alloc` by name. The port relies on the
   shim returning the same function index for a name declared twice, and its
   caller decides which runtimes to define. The swap needs the real query:
   lane 1 has `declarations()` on its list.
4. **A package cannot import a `.vsys` library** (shim report, gap 4): the
   test assembles `@vesper/cranelift` and this package into one source.
5. **JIT entries take no arguments** (shim report, gap 5): the test generates
   one entry per configuration, and the object leaves through
   `object_publish`.

No Rust bug was found.

## What is left

- **After lane 1's first PR:** emission of the in-function families through
  lent builders: `vector_op` and the slice loads and stores
  (`src/native_vector.rs`), `tiled_op` (`src/tiled/native.rs`), the float
  intrinsics and conversions (`src/float_math/native.rs`), region blocks
  (`src/regions/native.rs`), `atomic_call` and `parallel_for_call`
  (`src/native.rs`); then the `numeric` hook install in the hybrid build, a
  whole-object and CLIF comparison over the corpus, and the measurements.
- **After lane 3 (#1277):** the 128-bit helpers (`src/integer_widths/native.rs`).
- **Runtime swap:** the runtimes are ready to define into lane 1's lent
  module once it can answer `get_name` (gap 3).
- **Mach-O and COFF:** the root crate's `object` reader is built for ELF
  only, so the runtime comparison covers the two ELF targets; the emission
  is the same code for every target.
- **Scoreboard row:** added to the SH4 scoreboard in
  `docs/maintenance/self-hosting-plan.md` once the plan (#1293) has merged.
