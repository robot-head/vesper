# Self-hosting SH4 lane 12: the export ABI in Vesper

Claim #1286. Prompt: [12-codegen-export.md](../agent-prompts/self-host-sh4/12-codegen-export.md).
Package: [`packages/vesper-codegen-export`](../../packages/vesper-codegen-export/README.md)
(`@vesper/codegen-export`). Rust stays the default build; nothing in the
compiler calls the package yet.

Part 1 branched from the head of PR #1259 (`8ac0f94`, SH3 lane 1 part 1,
which stacks on #1232); `origin/main` was `e73cfd6`.

## Part 1: binding generators and directives

The lane's work splits by what it needs from other lanes:

| Rust | Lines | Needs | Status |
|---|--:|---|---|
| `rust_binding`: binding.rs, binding_v2.rs, binding_v4.rs (v1-v4) | ~2,000 | only the `LibraryInterface` | **ported** |
| `Directives::parse`: directives.rs | ~330 | the source text | **ported** (descriptor parse stays Rust) |
| `plan_with` and its `Planner` (the interface) | ~700 | the checked `World`, layouts, modes | part 2 |
| `glue::define`, the export signatures, `object` | ~600 | lane 1's lent Cranelift handles (#1275) | after lane 1 |
| 128-bit cells | | lane 3 (#1277) | after lane 3 |

### What changed

- **`packages/vesper-codegen-export`.** `iface.ves` reads a
  `LibraryInterface` and `BindingOptions` from words into flat tables:
  names are `@vesper/check-data` symbols and `AbiType`s are hash-consed
  nodes, so `t == first` in `array` is an id compare. The type predicates
  (`owning`, `abi_words`, `mirrored`, `lent`) are methods on it.
  `binding.ves`, `binding_v2.ves` and `binding_v4.ves` follow the three Rust
  files function for function. Rust's `BTreeSet<String>`,
  `BTreeSet<(String, u32)>` and `BTreeMap<String, u32>` are one sorted
  `StrSet` (text.ves). `directives.ves` reads `@export` lines as
  `Directives::parse` does and parses instance type arguments itself.
- **`tests/self_host_codegen_export.rs`** and its `words.rs` (the Rust half
  of the input format).
- **BUILD.bazel**: the test's data (the 40 port crates' `:files`,
  `packages/**`, the `.ves` sources) and size `long`.

### The differential

The corpus is every library a port crate links: one row per `Port::new`
call in `crates/*/build.rs` (51 libraries in 40 crates, including the four
hybrid modules, the five prism libraries and the joined sources of ignis,
bbr, tcp and wheel). Each is built with the `vesper` executable as the port
harness builds it (`build --lib --no-opt`; the binding does not depend on
the optimizer, and `--no-opt` is about 6x faster in a debug build). The
Rust generator reproduces all 51 committed bindings from those interfaces,
which checks the table.

| Check | Cases | Differences |
|---|--:|--:|
| Rust generator vs committed binding | 51 | 0 |
| Vesper binding vs Rust binding (each library under its own `rust_types`, none, and a made-up path) | 126 (4.9 MB of text) | 0 |
| Vesper directives vs `Directives::parse` (every `@export` source in the repository, 46 edge cases, the `.vsys` cases on 4 more targets) | 79 (30 refusals) | 0 |

ABI versions in the corpus: v1 3, v2 31, v3 14, v4 3.

### Numbers

Release build, the 51 libraries under their own options, ABBA best of six
(`CHECK_HARNESS_SPEED=1 cargo test --release --test
self_host_codegen_export binding_cost -- --ignored`):

| | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| binding generator | 7.8 ms | 14.9 ms | 1.91x |
| with input encoding and text unpacking | 7.6 ms | 19.3 ms | 2.53x |

Encoding the 51 interfaces takes 3.5 ms. The generator writes into one
`ChunkSeq[u8]`; most of the gap is per-name copies (`Names::write` pushes
byte by byte, and an identifier is copied once to check it against the 39
Rust keywords). The binding is generated once per library build, so this is
not on a hot path; the parity bar is the whole `build --lib` once emission
is swapped.

## Part 2: the v2 glue, emitted in Vesper

Stacked on lane 1's #1309 (lent Cranelift handles and the `export` hook).

### What changed

- **`packages/vesper-codegen-export/glue`** (`@vesper/codegen-export-glue`):
  `glue.rs` in Vesper over `@vesper/cranelift`. After the runtime prelude it
  emits v4's `box_new`, `vesper_box_free` when the module lacks it, the
  `seq_new`, `seq_free` and `live_allocations` entry points, and the drop
  glue of every handle type (sequences of owning elements, boxes, boxed
  objects through their vtable, tuples, `Option`/`Result`, nested handles,
  owned strings). `export_glue` is the hook: its words are a flags word
  (what the module already holds) and the interface in part 1's words.
- **The swap point moves into `glue::define`** (owner approved,
  2026-11-21). Lane 1's dispatch replaced the whole glue, but the glue
  starts by defining the sequence runtime and `vesper_box_alloc`, which
  belong to the seq and drops families. The hook is now called right
  after them; the Rust fallback and the default build are unchanged. This
  touches only src/native_export.rs and src/native_export/glue.rs (8 lines
  each way); src/codegen_swap.rs is unchanged.
- **`crates/vesper-hybrid`**: module `hybrid_codegen_export`
  (modules/codegen_export) installed as the `export` hook, the interface
  encoded with part 1's words.rs.
- **`crates/vesper-hybrid/tests/codegen_export.rs`**: the default `vesper`
  and `vesper-hybrid` build every library of `PORTS` (now
  tests/self_host_codegen_export/ports.rs, shared with part 1's test) with
  `build --lib`, `--no-opt` for all and with the optimizer for three.

### The differential

| Check | Objects | Differences |
|---|--:|--:|
| `vesper` vs `vesper-hybrid` objects, 51 libraries (`--no-opt`, plus 3 optimized) | 54 | 0 |

The hybrid's stats show the hook emitted the glue of all 51 builds of v2
libraries (48 unoptimized, 3 optimized); v1 libraries have no glue.

## Stage 1 guard

Part 1 adds a package, a test and BUILD.bazel entries. Part 2's only Rust
edit outside the hybrid is the swap point's move from
`native_export::compile_exports` into `glue::define`: with no hook
installed (the default compiler) the glue runs exactly as before, so the
default compiler and its objects are unchanged. Nothing touches
src/verified_codegen/**, src/ir.rs, `ir_opt`, lean/**, bootstrap/** or
packages/vesper-compiler/**. The bootstrap-stage1 workflow run on the
branch is linked from the PR.

## Compiler gaps

1. **`handle` and `sink` are keywords**, so they cannot name a field, a
   method or a local (`d.handle`, `let sink = ..`, E0002). The port uses
   `is_handle`, `handle_of` and `by_value`. Known (#1244).
2. **`pub const` in a package submodule is not importable** (`use
   self::iface::{K_BOX}` is E0111 "module self::iface has no item"). Each
   file repeats the constants it uses, as @vesper/check-data does. Repro: a
   package with `src/a.ves` holding `pub const X: u8 = 1;` and `src/lib.ves`
   holding `use self::a::{X};`.
3. **No method call on a call's result** (`sub(in s, a, e).as_slice()` is
   E0202 "unknown field"); bind the result to a local first.
4. **A `Str` view parameter is second-class even for `.len()`**:
   `p.as_bytes().len()` is E1531; `let b = p.as_bytes(); b.len()` works.

## Gaps against Rust (documented, not in the asserted corpus)

- The instance type-argument parser covers what directives use: upper-case
  and scalar names with arguments, tuples (`(T)` is a one-tuple, as in
  Rust), decimal lane counts, and lower-case row variables (shown as Rust's
  `eff Effects { .. }`). Rust's full type grammar also accepts arrays
  `[T; N]`, `fn` types, `dyn`, paths and labelled rows, which the port
  refuses ("the type arguments do not parse"). The swap should parse them
  with @vesper/front.
- The `.vsys` descriptor parse (`vesper_ffi::sys::parse`, which names a
  `vsys` import's library and symbols) and `Directives::add_packages` stay in
  Rust: the dump shows each import's handler, Rust path and filled-in text.

## What is left

- Part 2: `plan_with` on @vesper/check-data's `World` and
  @vesper/check-layout's layouts, compared as interface JSON over the same
  corpus.
- The export signatures (the wrappers `object` compiles per exported
  function); 128-bit cells after lane 3.
- The full `--lib` suite and the port harness's export tier on the hybrid,
  and the SH4 scoreboard row.
