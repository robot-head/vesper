# Self-hosting SH0 lane 1: hybrid Stage 2 build (#1123)

> Base: `main` at `b71ac81` (2026-11-19, the self-hosting plan merge, #1131).
> Prompt: `docs/agent-prompts/self-host-sh0/01-hybrid-build.md`. The
> [manuscript](../manuscript/README.md) is the sole specification; this lane
> needs no change to it.

## What changed

- **`crates/vesper-hybrid`** (new, non-default): the `vesper-hybrid`
  binary. It is the default `vesper` CLI, with the root package's
  `src/main.rs` included unchanged (and through it every command module),
  plus a swap-in module that installs Vesper objects into the compiler's
  function tables before `main` runs. Its `build.rs` compiles each swapped
  module's Loom package with the default `vesper` executable
  (`vesper-port-harness`) and checks the committed binding.
- **`src/layout.rs`**: the swap-in point of the pilot. The word arithmetic of
  `Layout::of` (first field offset, field placement with its overflow check,
  product and enum sizes, word offsets) is now the function table
  `WordLayout`; `RUST_WORD_LAYOUT` is the Rust implementation and the
  default; `install_word_layout` replaces it for the process. Nothing in the
  default compiler calls `install_word_layout`.
- **`crates/vesper-hybrid/modules/layout`**: the pilot, `@vesper/hybrid-layout`,
  the same five functions in Vesper (60 lines). It lives with the hybrid
  crate, not under `packages/vesper-compiler/**` (Stage 1 lock).
- **Tests** (`crates/vesper-hybrid/tests/`): `differential.rs` (the
  differential harness), `kernel.rs` (the pilot function by function against
  the Rust table) and `manifest.rs` (the hybrid lists every root dependency;
  it is not a default member).
- **Bazel**: `tools/bazel/vesper_hybrid.bzl` (`vesper_hybrid(modules = ...)`)
  and `crates/vesper-hybrid/BUILD.bazel`. `//:bin/vesper` builds each module
  object (`vesper_port_object`), the build script links it, and the tests
  run with `//:bin/vesper` on `PATH`.
- **CI**: `.github/workflows/hybrid.yml`, "Hybrid compiler", not required:
  Linux, macOS and Windows build the default compiler, build and test the
  hybrid, then rebuild it with a divergent pilot that the harness must catch.
- **Root patch** (own commit): the `crates/vesper-hybrid` workspace member
  line and `Cargo.lock`. The crate is a member but not a default member, so
  a plain `cargo build` or `cargo test` never builds a second compiler.

## Build design

The constraint is the one the ports hit: the compiler cannot depend on a
crate whose build needs the compiler. Three choices keep the hybrid build
out of the default compiler's way.

1. **A separate binary crate, not a feature of `vesper`.** `vesper-hybrid`
   depends on the `vesper` library as it is. A Cargo feature on `vesper`
   (`swap-layout`) would have been unified into the default compiler by
   resolver 2 in every workspace-wide build (`cargo build --workspace`,
   `cargo test --workspace`, Bazel's metadata), and would have needed a
   root `[features]` table, which the root patch rule does not cover. A
   `cfg` cannot be set on a dependency at all.
2. **Swapping at run time through a function table.** Each module presents
   a table of plain `fn` pointers (`vesper::layout::WordLayout`) with the
   Rust implementation as the default. The hybrid fills the table with
   wrappers over the Vesper binding. The default compiler pays one
   `OnceLock` load per `Layout::build` call and an indirect call per
   arithmetic step (measured below).
3. **The CLI is included, not forked.** `crates/vesper-hybrid/src/main.rs`
   is `include!("../../../src/main.rs")`. `mod` items in an included file
   resolve next to that file, so every `src/*_commands.rs` module compiles
   unchanged, and `crate::` paths in them name the hybrid crate root, as
   they name the root binary in the default build. The included `main` stays
   the entry point, so the hybrid installs its tables from a static
   constructor (`.init_array` on ELF, `__DATA,__mod_init_func` on Mach-O,
   `.CRT$XCU` on Windows), which only stores function pointers. Other
   targets are a `compile_error!`.

**No cycle.** `vesper-hybrid` -> `vesper` (normal) and `vesper-hybrid` ->
[build] `vesper-port-harness`, which runs a `vesper` executable and does not
link the library. `cargo tree -p vesper -e normal,build` does not contain
`vesper-hybrid`, and the port closure guard is unchanged.

**No feature unification.** `cargo tree -p vesper -e normal,build -f "{p}
[{f}]"` lists the same packages with the same features whether or not
`-p vesper-hybrid` is built with it (the only new lines are
`vesper-hybrid`, `vesper-port-abi` and `vesper-port-harness` themselves).
`cargo build -p vesper --bin vesper` after `cargo build -p vesper
-p vesper-hybrid` is fresh, and the binary is byte-identical.

**Where the module objects come from** (build.rs): `VESPER_HYBRID_OBJECT_<LIBRARY>`
and `VESPER_HYBRID_BINDING_<LIBRARY>` (Bazel); else `VESPER_PORT_COMPILER`
(CI builds `vesper` once); else the harness builds the in-tree compiler.
The harness takes one prebuilt object per build script, so build.rs hands
each module's to it in turn.

**Dependencies.** The included CLI sources need the root package's
dependencies, so `crates/vesper-hybrid/Cargo.toml` repeats them with the
same specifications. `tests/manifest.rs` fails when the root gains one the
hybrid lacks. Lane 2 (#1124) moves tool subcommands out of `src/main.rs`;
the hybrid follows automatically (it includes whatever `main.rs` is), and
the manifest test names any dependency it then needs.

## Swap-in recipe (SH1 and later)

For a compiler module `src/<m>.rs`:

1. **Pick the boundary.** Find functions of the module that the rest of the
   compiler calls and whose arguments and results cross export ABI v3
   (scalars, copy structs and enums, slices, sequences, strings, handles).
   Prefer coarse functions: each call crosses an opaque C call (see the
   numbers).
2. **Add the swap-in point to `src/<m>.rs`**, and nothing else there: a
   `#[derive(Clone, Copy)] pub struct <M>Table { pub f: fn(..) -> .., .. }`,
   `pub const RUST_<M>: <M>Table` holding the existing Rust code (closures
   or the existing private functions), a `static` `OnceLock<<M>Table>`,
   `pub fn install_<m>(t: <M>Table) -> Result<(), <M>Table>`, and a private
   `fn table() -> &'static <M>Table` that falls back to `RUST_<M>`. Call
   through `table()` where the Rust code was. The default compiler must not
   change behavior; the differential harness checks it.
3. **Write the Vesper package** at `crates/vesper-hybrid/modules/<m>/`
   (`Loom.toml` named `@vesper/hybrid-<m>`, `src/lib.ves`), one `pub fn`
   per table entry with the same meaning, total where the Rust is total.
   Encode `Option` results as an out-of-range sentinel when the hot path
   would otherwise allocate (gap 1). Run `vesper fmt`.
4. **Register it**: append `("hybrid_<m>", "modules/<m>", "port/<m>.rs")`
   to `MODULES` in `build.rs` and `"hybrid_<m>": "modules/<m>"` to
   `modules` in `BUILD.bazel`, then
   `VESPER_PORT_BLESS=1 cargo build -p vesper-hybrid` and commit
   `port/<m>.rs`.
5. **Install it** in `src/hybrid.rs`: include the binding
   (`#[path = "../port/<m>.rs"] mod <m>_binding;`), build a `<M>Table` of
   non-capturing closures over it (count calls for `VESPER_HYBRID_STATS`),
   and call `vesper::<m>::install_<m>` in `install`.
6. **Test it**: a `tests/<m>.rs` that compares each Vesper function with
   the Rust table on edge values, as `tests/kernel.rs` does; extend
   `pilot_module_runs_from_vesper` so the module reports calls; add a
   divergent edit (build.rs `divergent` feature and BUILD.bazel
   `divergent`) and confirm the differential harness catches it. If no
   corpus file exercises the module, add corpus files first.
7. **Measure** callgrind instructions inside the module's entry points
   (default vs hybrid) and ABBA wall clock on a large package, and list the
   gaps. Rust stays the default; nothing is deleted.

## The pilot

`WordLayout` has five entries; `modules/layout/src/lib.ves` implements each
bit for bit, including the edge cases the Rust reaches only through
overflow: `next_offset` reinterprets the size as an `i32`
(`offset.checked_add(size as i32)`), and `product_size`/`enum_size`
reinterpret a negative end as a `u32`. `tests/kernel.rs` checks every pair
of 16 `i32` and 12 `u32` edge values for each function and 4,098 word
indices; all agree.

## Differential harness

`tests/differential.rs` runs `check --json`, `emit --emit ir` and
`emit --emit object` with both compilers on every `.ves` file under
`bootstrap/corpus`, `examples/` and `tests/` (536 files, 1,608 runs each),
from the repository root, in parallel, and compares exit status, stdout,
stderr and object bytes.

| Run (Linux x86-64, port-compiler profile) | Differences |
|---|--:|
| hybrid vs the default compiler of this branch | 0 |
| hybrid vs `main`'s compiler (`b71ac81`) | 0 |
| hybrid with the divergent pilot (enum tag takes two words) vs `main` | 37 |

`pilot_module_runs_from_vesper` runs the hybrid on `examples/counter.ves`
with `VESPER_HYBRID_STATS=1` and requires a non-zero count of calls
answered from Vesper (14 there; 492,518 building the Aura port package;
3,654,707 building the Pact port package).

## Numbers

Instructions (callgrind, `vesper build --lib` of `crates/vesper-beacon/vesper`,
12,570 pilot calls), inclusive in `vesper::layout::Layout::of`:

| Compiler | `Layout::of` | Whole run |
|---|--:|--:|
| `main` (`b71ac81`) | 31,895,905 | 4,143,076,750 |
| this branch, default (Rust table) | 32,153,937 (+0.8%) | 4,152,986,241 (+0.24%) |
| `vesper-hybrid` (Vesper table) | 32,380,144 (+1.5%) | 4,156,523,240 (+0.32%) |

So the swap-in point costs about 20 instructions per arithmetic step in the
default compiler (an indirect call that LLVM no longer inlines, and the
table load), and the Vesper object about 18 more (the call into the export
ABI, its 64-bit argument widening, and the stats counter). Whole-run
differences beyond `Layout::of` are within run-to-run variation of
callgrind on this program (hash seeds).

Wall clock, ABBA (`B H H B R B B R`, two rounds), `vesper build --lib` of
`crates/vesper-pact/vesper` (3.65 M pilot calls): `main` 15,892 ms (n = 8),
branch default 15,772 ms (n = 4), hybrid 15,870 ms (n = 4): 1.00x within
noise. On the whole differential corpus, the hybrid took 10.50 s against
10.24 s for `main` (1.025x; process start-up dominates small files).

Building the hybrid: with the compiler library already built, the hybrid
binary takes about 25 s (port-compiler profile) on this 4-core host, a
second copy of the CLI's 16k lines of command modules.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
bootstrap-stage1 workflow was dispatched on this branch; its result is in
the PR. The default binary changes (it contains the swap-in point), so its
digest differs from `main`'s, as for any commit to `src/`; outcome mode
ratchets the verified artifacts, not the seed digest. Behavior is
unchanged: the differential harness finds no difference between this
branch's `vesper`, `main`'s and the hybrid.

## Compiler gaps

1. **An `Option` (or any aggregate) result allocates per call in the
   generated binding.** `pub fn f(x: i32) -> Option<i32>` gets a Rust
   wrapper that builds `vec![0u64; 2]` for the result words on every call
   (see `bounds` in `crates/vesper-port-pilot/port/binding.rs`). `Abi::WORDS`
   is a constant, so a stack array `[0u64; WORDS]` would do. The pilot
   returns `i64` with an out-of-range sentinel (`no_fit`) instead.
2. **No total checked arithmetic.** Rust's `offset.checked_add(n)` has no
   Vesper builtin: `+` raises `Fail[ArithOverflow]`, and catching it across
   the export ABI costs a failure word per call. The pilot widens to `i64`,
   adds with `wrapping_add` and range-checks. `checked_add_k` returning
   `Option[k]` (one per integer kind, like `wrapping_k`) would match compiler
   code directly; it is a manuscript call for a later lane.
3. **No inlining across the boundary.** Each table entry is an opaque C
   call into the object (and the wrapper widens every argument to `i64`):
   about 18 instructions per call for one-instruction bodies. Fine-grained
   swaps cost more than coarse ones; SH1 should swap whole entry points
   (`Layout::of`), which needs gap 4.
4. **Compiler data cannot cross the export ABI.** The real entry point,
   `Layout::of(world: &World, ty: &Type)`, takes the checker's `World` and a
   recursive `vesper_core::ast::Type`. Export ABI v3 has no way to lend
   Rust-owned recursive enums or maps to Vesper, so the pilot could only
   take the arithmetic. Either the AST moves to Vesper first (SH1, with
   lane 5's interning and arenas) or the ABI gains lent opaque handles with
   Rust accessor imports (`.vsys` handles in the other direction).
5. **No hook for code before `main` in Vesper or a library entry for the
   CLI.** The hybrid needs a static constructor because the CLI's entry is
   `fn main` in `src/main.rs`. A `pub fn run_cli()` in the library would
   let the hybrid call it after installing; that is a change to
   `src/main.rs` (lane 2's file in this wave), so it is left for after
   lane 2.

6. **The Stage 1 self-hosted parser has no `pub`.** `pub fn a() -> i64 { 1 }`
   gives a different node count from the seed parser in
   `tests/self_host_front_end.rs` (`differential_over_repository_sources`),
   so `crates/vesper-hybrid/modules` joins the skip list in
   `tests/fixtures/self_host/driver.rs`, as the ports do. The same test
   fails under Cargo on `main` for `crates/vesper-port-pilot/vesper/*.ves`,
   which are not skipped; Bazel passes because those files are not in the
   test's data. Not fixed here (Stage 1 lock).

## What is left

- Bazel and macOS/Windows have not run locally (no Bazel on this host); the
  PR's CI runs them.
- Only one module is swapped. Gap 4 decides how far table-style swaps go;
  SH1 should plan whole-function swaps once the AST is in Vesper.
- When lane 2 (#1124) lands, check that the hybrid still includes the CLI
  unchanged and that the manifest test still covers its dependencies.
