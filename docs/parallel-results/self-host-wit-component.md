# Self-hosting SH5 lane 11: WIT component wrapping in Vesper

Claim #1334. Stacks on #1311 (SH4 lane 15, the wasm backend). Prompt:
`docs/agent-prompts/self-host-sh5/11-wit-component.md`. Rust stays the default
compiler; nothing here changes a default build or deletes Rust code.

The owner chose "measure first" (2026-11-21): part 1 measures what the backend
reaches in wit-component and wit-parser, part 2 ports that subset. After
part 1 the owner chose a direct writer without validation (2026-11-21, decision
card in the project thread).

Base: #1311 head `8aa8d59` (SH4 lane 15, with #1309 merged in); `origin/main`
at `bbda89e`.

## Part 1: what `component::encode` reaches

`src/wasm_backend/component.rs` `encode` is 40 lines. It parses five WIT
packages into a `wit_parser::Resolve` (the program's WIT and four fixed
packages: wasi:io, wasi:cli, wasi:clocks, vesper:beacon), embeds the world as
a `component-type` custom section (`embed_component_metadata`), then hands the
module to `ComponentEncoder` with validation on, which decodes that section
back into a second `Resolve`, validates the core module against the world,
and writes the component.

### Method

- `tests/self_host_wit_component.rs` `dump_wrap_inputs` (ignored) runs the Rust
  backend over lane 15's cases (`cases()` x `configs()`, 323 programs x 4
  option sets) and writes every input `encode` sees: the core module, the WIT
  text, the world, and Rust's component. **1,088 compilations** succeed in
  Rust; that is every input wrapping gets today. They contain 37 distinct WIT
  texts.
- A stand-alone driver calls the same `encode` (wit-component, wit-parser,
  wasm-encoder, wasmparser and wasm-metadata 0.261, pinned to the
  workspace's versions) on each dump, built with `-C instrument-coverage`.
  All 1,088 outputs equal Rust's component byte for byte, so the driver
  measures the real path. `llvm-cov report` gives the lines and functions
  executed. "Lines" below are llvm-cov's executable lines, not source lines
  (the crates are 16.0k / 18.9k / 18.5k / 42.7k / 2.3k source lines).
- "No imports" is the subset without host imports or stack switching (1,026
  inputs; lane 15's 640 identical cases are inside it, since the port
  declines handlers and host frames). The other 62 inputs are handler
  programs: 38 use stack switching, 24 import `vesper:beacon/trace`.

### Size

| Crate | Executable lines | Reached (all 1,088) | Reached (no imports) | Functions reached (all) |
|---|---|---|---|---|
| wit-component | 9,429 | 2,346 (25%) | 1,475 | 212 / 701 |
| wit-parser | 9,131 | 3,667 (40%) | 3,521 | 292 / 723 |
| wasm-metadata | 310 | 114 | 114 | 14 / 39 |
| wasm-encoder | 10,267 | 1,394 | 882 | 240 / 1,585 |
| wasmparser | 20,352 | 6,841 | 6,050 | 891 / 2,647 |
| **Total** | 49,489 | **14,362** | 12,042 | 1,649 |

Where it goes, by crate:

- **wit-parser (3.7k)**: the WIT lexer and parser (`ast.rs`, `ast/lex.rs`,
  `ast/resolve.rs`: 1.7k), `Resolve` (`resolve/mod.rs`: 1.1k: package
  merging, world elaboration, `select_world`), and decoding the embedded
  `component-type` section back into a `Resolve` (`decoding.rs`: 0.5k). Most
  of it parses the four fixed packages again on every call and then
  re-reads the world from bytes it just wrote.
- **wit-component (2.3k)**: `encoding.rs` (1.0k: the root component, core
  instances, `canon lift` / `canon lower`, the per-interface export shim
  components), `validation.rs` (0.4k: matching core exports and imports to
  the world), `encoding/{types,wit,world}.rs` (0.5k: component types and the
  `component-type` section), `metadata.rs` (0.2k: embed/extract),
  `encoding/fixup.rs` (0.2k, imports only: the indirect-lowering shim and
  fixup modules). `gc.rs`, `linking*`, `printing.rs` and the adapter path are
  never reached: there is no WASI preview-1 adapter, the module is native
  preview 2.
- **wasmparser (6.8k)**: validation, about two thirds of it of the core
  module the backend wrote (operators, core types), one third of the
  component. It decides only whether wrapping fails; it never changes the
  bytes.
- **wasm-encoder (1.4k)**: the component section writers. The port already
  has its own core encoder (`enc.ves`, `module.ves`).

### What the output contains

Every one of the 1,088 components has the same shape, and only a small
part of the component model:

- the core module, byte-identical to the input plus one `producers` custom
  section (`processed-by wit-component 0.261.0`);
- one core instance of it, an alias of its `memory`;
- for each exported interface (`wasi:cli/run@0.2.0` always, and
  `vesper:program/entry@0.1.0`): the function types, an alias of the core
  export `<iface>#<name>`, a `canon lift` (with `(memory 0)` when the
  result is lifted from memory), and a nested shim component that imports
  the type definitions and the function, re-exports them, is instantiated
  `with` them and is exported as the interface instance;
- a `component-name` section and a `producers` section.

The WIT types reached are `bool`, `u8`, `u32`, `u64`, `s64`, `f64`,
`string`, `tuple`, `record` and `variant` (the backend writes options and
results as variants). No `list`, `enum`, `flags`, resources, `realloc`,
`post-return`, or parameters on any entry (all 1,088 entries are nullary;
the backend refuses more than 16 flat parameters and adds `cabi_realloc`
only for string parameters). No Nexus plugin world is reached by the cases.
The 24 import inputs add a component-type import of `vesper:beacon/trace`
(three records, one variant, six functions), `canon lower` with
`(memory 0)` and `string-encoding=utf8`, and wit-component's indirect
lowering: a shim core module with a `funcref` table and `call_indirect`
trampolines, and a fixup module filling the table.

### Time

Release build, one 4-core cloud host, all 1,088 inputs, best of three
(microseconds per component):

| Step | µs |
|---|---|
| Parse the five WIT packages, `select_world` | 133 |
| `embed_component_metadata` | 9 |
| `ComponentEncoder::module` (decode, validate the core module) | 57 |
| `encode` (with component validation; 50 µs without) | 97 |
| **Total** | **~300** |

Lane 15 measured the Rust backend at about 170 µs per module and the
Vesper port at about 45 µs, so **wrapping costs more than compiling**: in
the hybrid, about 6x the ported backend. Re-parsing the four fixed WASI and
Beacon packages and the encode/decode round trip are most of it.

### Recommendation

Port the subset as a direct writer, not as a port of wit-component:
`@vesper/wit-component` reads the backend's WIT (the grammar above, about
300 lines), builds the type table, and writes the component sections in
wit-component's order, with its names and type indices. It skips what
does not change the bytes: re-parsing the fixed packages, the
`component-type` round trip, and validation. An estimate of 1.2-1.5k
Vesper lines against the 14.4k Rust lines reached, at a few µs per
component.

What it gives up: wasmparser's validation of the core module and the
component. The backend's core bytes are already checked against Rust's
byte for byte in the tests, and Wasmtime validates the component when it
loads it. The hybrid hook can keep a debug-build validation through Rust if
the owner wants it. Rust stays the default and remains the fallback: the
port declines any WIT outside the subset (it parses only the shapes above),
and the hook falls back to Rust's `encode`.

Part 2 covers the no-import shape first (every case the hook answers
today, since lane 15 declines host frames), then the Beacon import shape.

## Part 2: `@vesper/wit-component`

### What changed

- `packages/vesper-wit-component` (`@vesper/wit-component`, 1.6k lines with
  comments):
  - `wit.ves`: reads the backend's WIT (the grammar in the file comment:
    `record`, `variant`, scalars, `string`, `tuple`, the entry functions,
    the `program` world's exports) into an interned type table. A scalar,
    a tuple of the same elements, or a definition is one row, so a row is
    the identity wit-component's type maps key on (`TypeId` for named
    types, the structure for anonymous ones). Anything else is declined.
  - `comp.ves`: the component, written directly in wit-component's order:
    the core module plus its `producers` section; the root types of the
    exported interfaces (`declare_types_for_imported_intrinsics`); the core
    instance and the `memory` alias; per interface the core-export aliases,
    `canon lift` (with `memory` when the result is lifted from memory and
    `string-encoding=utf8` when it holds a string), and the nested shim
    component (types imported as `import-type-*`, functions as
    `import-func-*`, then re-exported typed), its instance and its export;
    then `component-name` (wasm-encoder's `append_names`, instances listed
    twice as it does) and `producers`. `B` is wasm-encoder's
    `ComponentBuilder` (sections merge consecutive items, each index space
    counts and keeps its debug names); `E` is a `ValtypeEncoder`'s maps
    (root, nested-import and nested-export modes).
  - `lib.ves`: the entry `wit_component_wrap` / `wit_component_wrap_bytes`:
    `[world kind, WIT length (u32 LE), WIT, core module]` in,
    `[0, packed component]` or `[1, reason]` out.
- `tests/self_host_wit_component.rs`: the differential, a decline test, the
  ABBA timing and part 1's dump (all ignored but the first two). Lane 15's
  case list and Wasmtime runner are copied in, so this binary does not also
  run that harness's tests.
- The hybrid swap: `crates/vesper-hybrid/modules/wasm_backend` exports
  `wrap_words` over the package (binding regenerated); `src/hybrid.rs`'s
  `wasm` hook wraps the port's module with it and falls back to
  wit-component where it declines. `VESPER_HYBRID_STATS=1` prints how many
  components it wrapped. `wasm_module_runs_from_vesper` checks that
  `examples/transaction.ves` is wrapped from Vesper.
- BUILD.bazel: `//:wit_component_package`, the module's dependency, the
  new test's data and size. tests/fixtures/self_host/driver.rs skips the
  package (it uses `use`, #880), as for the other ports.

Rust stays the default: the default compiler's `component::encode` is
untouched, and the port runs only in the hybrid build.

What it does not do, by the owner's call: validate. wit-component runs
wasmparser's validator over the core module and over the component; the port
writes the bytes and trusts that the core module is valid. Validation never
changes the bytes, and Wasmtime validates a component when it loads it.

### Results

All 1,088 inputs Rust wraps over lane 15's cases (`port_matches_wit_component`):

| Outcome | Inputs |
|---|---|
| Component byte-identical | **1,064** |
| Declined: host imports (`vesper:beacon/trace`), Rust wraps | 24 |
| Differences | **0** |

The 1,064 include every one of lane 15's 640 identical compilations (there
the port's core module and WIT equal Rust's, so the wrap inputs are the
same), the 38 that use stack switching, and the handler programs the Rust
backend lowers. 247 of them (nullary entry, first option set, no stack
switching) also run under Wasmtime 50 from the port's component with the
same result as from Rust's. `declines_outside_the_subset` checks that a
plugin world, another package, `list`, an unknown type, an import, a string
parameter (it needs `realloc`) and truncated text are declined.

The hybrid differential (crates/vesper-hybrid/tests/differential.rs, release
builds of both compilers, one run): 538 files, 2,152 runs per compiler,
**0 differ**; default 14.99 s, hybrid 16.41 s (1.09x) for all four commands.

### Speed

Ignored test `timing` (`CHECK_HARNESS_SPEED=1`, release test build, ABBA
best of six, one 4-core cloud host): the 1,064 components, five times each.

| Side | Time | Per component | vs Rust |
|---|---|---|---|
| Rust `component::encode` (five WIT packages, embed, wit-component with validation) | 1,361 ms | 256 µs | 1.00x |
| Port (JIT, optimized), from its input to the packed component | 60 ms | 11 µs | **0.044x** |

Before this lane the wrap had no Vesper version, so "before" is the Rust
column. Without validation and the four re-parsed packages Rust's own work
is still most of it: the port writes what wit-component writes and nothing
else. The port's time includes packing the component into words for the
harness (`pack_text`).

## Stage 1 guard

This lane adds a Vesper package, a test, and the hybrid hunks. It does not
touch src/verified_codegen/**, src/ir.rs, `ir_opt::optimize`, lean/**,
bootstrap/**, packages/vesper-compiler/** or src/wasm_backend/**, and adds no
swap point. The bootstrap-stage1 workflow runs on this branch once #1311 has
merged and `main` is merged in (owner's CI rule: no runs while stacked).

## Compiler gaps

None blocked the port. Two notes for whoever writes the next one:

- Every arithmetic operator raises `Fail` on overflow, so plain `a - b` in a
  total function fails to check (E0401); the package uses `wrapping_sub`
  where the operands are known ordered. A checked-but-total subtraction
  form for `a >= b` facts would read better (the bounds-facts lanes already
  know `a >= b` in most of these places).
- Building a short name (`"import-type-" ++ name`) takes a fresh
  `ChunkSeq` and two `extend`s each time; a `Str` concatenation or
  formatting builtin would save the allocations.

## What is left

- **Host imports.** The 24 Beacon `trace` inputs: the import's instance
  type, `canon lower` with `memory` and `string-encoding`, and
  wit-component's indirect lowering (the shim module with its `funcref`
  table and `call_indirect` trampolines, and the fixup module filling it).
  Lane 15 does not port host frames yet, so the hook never needs this
  today; it is the next part once it does. `wasi:cli/stdout` (a resource)
  and `wasi:clocks` are not reached by any case yet.
- **String parameters** (`realloc` = the module's `cabi_realloc` alias) and
  the **Nexus plugin** world (a root `run` export, no interface): neither is
  reached by the cases; both are declined.
- Rust stays the default; deleting nothing.
