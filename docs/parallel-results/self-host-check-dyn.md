# Self-hosting SH2 lane 11: the dyn passes in Vesper

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1183. Base: `main` at `e43a56f`. Lane prompt:
[`docs/agent-prompts/self-host-sh2/11-dyn.md`](../agent-prompts/self-host-sh2/11-dyn.md).
Rust stays the default checker everywhere, and no Rust code was deleted.

This is the first PR of the lane: the two passes ported and checked against
Rust. The hybrid swap is a follow-up (see "What is left").

## Summary

- **`@vesper/check-dyn`** (`packages/vesper-check-dyn`) ports
  `src/dyn_fn.rs` and `src/dyn_trait.rs` (2,330 lines of Rust, 4,900 of
  Vesper). Both are syntactic passes over the surface AST that run before
  type checking, in the order `pact_frontend::check_package_with` runs them:
  - `fnobj.ves` (`dyn_fn`): function objects become hidden one-method
    traits `__FnObject{kind}_{fnv16}`, and closures where one is expected
    become hidden `__Closure{i}_{start}` structs and conformances, with
    synthetic spans, the origins table and `relocate`.
  - `dyntrait.ves` (`dyn_trait`): `in/inout/sink dyn Trait` parameters
    become hidden bounded type parameters `__dyn__p`, with the second-class
    escape check (E0630, with `forwards`), object safety (E0631, E0632) and
    `params` / `boxed`.
  - `core.ves`: the pass state, AST node helpers over the @vesper/ast index
    graph, types as hash-consed @vesper/check-data ids (equality, `Display`,
    `Debug`), the type walker and `free_names`.
- **Differential**: 0 differences on every case. Each case compares every
  diagnostic's code, span and text, the origins and `relocate` probes,
  `params`, `forwards`, `boxed`, and the canonical dump of the lowered
  module.
- **Speed**: the Vesper passes, Vesper parse included, take 0.70x of Rust's
  time on the dyn suites and mutants and 0.28x on the repository's sources.
- **One compiler change**: the native monomorphization cap is now a named
  `MAX_NATIVE_INSTANCES = 4096` (it was a bare 1024, in two places). The
  package's entry needs 1,031 instances.

## What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-check-dyn/{Loom.toml,src/*.ves}` |
| Differential and timing | `tests/self_host_check_dyn.rs` |
| Visibility only, for the test | `src/dyn_trait.rs` (`uses_dyn`, `Forward`, `Callee`, `Lowered`, `lower` are `pub`), `src/dyn_fn.rs` (`Lowered::{base, end, origins}` are `pub`) |
| Instance cap as a named constant | `src/native.rs`, `src/native_export.rs` |
| Bazel data and timeout | `BUILD.bazel` |
| Report and scoreboard row | this file, `docs/maintenance/self-hosting-plan.md` |

No `@vesper/check-data` fields were added: the passes run before the world
exists, so they need only `Names`, `Types` and the diagnostics table.

No port-harness shard entry was added: like `@vesper/check-data`, the
package has no `vesper build --lib` object yet. It joins shard 7 with the
hybrid follow-up.

## Design

- **Input is source text.** The entry takes the module's source and parses
  it with @vesper/ast `lower_source`, which SH1 lane 9 showed matches the
  Rust parser node for node. That keeps the entry at three words.
- **Rewrites in place.** Expression nodes are overwritten in the AST
  arenas. Type, effect and list nodes are shared, so they are copied on
  write.
- **Types are ids.** Every type the passes compare or print is converted
  to a hash-consed `Types` id, so equality is an id compare and messages
  use check-data's `Display`/`Debug`, which already match Rust.
- **The environment is a binding stack with marks**, and the declaration
  tables are symbol-indexed with method chains, in place of the Rust
  `BTreeMap`s and cloned `HashMap`s.
- **Synthetic spans.** Rust's `dyn_fn` base is
  `serde_json::to_string(module).len() + 1_000_000`, rounded up to even.
  Vesper has no JSON serializer of the Rust AST, so the caller computes the
  base and passes it in.

## The differential

`tests/self_host_check_dyn.rs`, which includes the shared harness by
`#[path]`. Release build, Linux x86-64:

| Test | Cases | Differences |
| --- | --: | --: |
| Dyn suites (`tests/dyn_trait.rs`, `dyn_box.rs`, `dyn_effects.rs`, `dyn_fn.rs` source strings), 27 text mutants of each, and hand-written cases for each diagnostic | 733 (114 sources, 333 lowered) | 0 |
| Every `.ves` file in the repository, gated and forced (both passes run even when the module needs neither) | 930 files x 2 | 0 |

All ten diagnostic codes fire in the first test: E0102, E0200, E0205,
E0207, E0310, E0401 and E0633 from `dyn_fn`, and E0630, E0631 and E0632
from `dyn_trait`. The test asserts that.

The differential compiles `check_dyn` unoptimized; the timing test
compiles `check_dyn_count` optimized. Both tests together take 80 s in a
debug build (65 s of it compiling the package) and 84 s in release. Bazel
runs them with the `long` timeout.

## Timing

ABBA, minimum of six runs each, release, Linux x86-64
(`CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_check_dyn --
--include-ignored`). Each side parses with its own parser and runs both
passes; Rust's side skips the JSON span base (the harness computes it
before timing).

| Set | Rust | Vesper | Ratio |
| --- | --: | --: | --: |
| Dyn suites and mutants (733 cases) | 44.0 ms | 30.7 ms | 0.70x |
| Repository sources, forced (930 files) | 1,090 ms | 303 ms | 0.28x |

Most of the gap on the larger set is the Rust passes cloning the module
and their maps per function; the Vesper passes rewrite one arena in place.

No callgrind numbers yet: under callgrind, compiling the package in the
test process (about 490 billion instructions) did not finish within an
hour, before any timed call. They come with the hybrid follow-up, which
builds the package once as a library object.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
instance cap only decides whether native lowering refuses a program.
The bootstrap-stage1 workflow run on the branch is linked from the PR.

## Compiler gaps

1. **The native instance cap was 1024** (fixed here). The package's
   `check_dyn` entry monomorphizes to 1,031 function instances
   (`check_dyn_count`: 977), and native lowering refused it with "native
   monomorphization exceeds 1024 function instances". The checker
   (`pact_frontend::mono::MAX_INSTANCES`) already allows 4096, so the
   native paths now share a named `MAX_NATIVE_INSTANCES = 4096`. Later
   checker ports will be larger still; a cap that scales with the program,
   or none, is the real fix. Repro: any package whose entry reaches more
   than 1,024 instances, such as this one at `e43a56f`.
2. **Constants cannot be imported across modules.** `use self::core::{NONE}`
   does not resolve a `const`, so `lib.ves` repeats
   `const NONE: u32 = 4294967295;`. Repro:
   `vesper check` on a package with `a.ves` holding `pub const N: u32 = 7;`
   and `lib.ves` holding `use self::a::{N}; pub fn f() -> u32 { N }` fails
   with E0111 "module self::a has no item 'N'".
3. **`eff` was a keyword** while this was written: a local named `eff`
   failed with E0002, so the effect variables are `hfx`. SH2 lane 3
   (#1218, now merged) makes it contextual.
4. **Word entries take at most three parameters.** `check_dyn` packs its
   options into one `flags` word (bit 0 forces both passes).
5. **No JSON in Vesper.** The `dyn_fn` span base depends on the length of
   the module's `serde_json` text, so the caller passes it. A self-hosted
   compiler needs either a Vesper serializer of the AST with the same
   output, or (better) a base that does not depend on JSON, which changes
   Rust's spans and so is an owner call.
6. **No shared AST dumper for tests.** The test copies the canonical dump
   and the Rust string-literal reader from `tests/self_host_ast.rs`; a
   `tests/common` module would remove the copy (not this lane's path).
7. **Optimizing the package is slow in a debug-built compiler.** With
   `optimize` on, compiling the two entries (`check_dyn`, 1,031 instances,
   and `check_dyn_count`) took 691 s under a debug build of the compiler,
   which hit Bazel's 15-minute test limit. Unoptimized, `check_dyn` takes
   65 s. Release builds compile both in under
   a minute. Repro: `Package::load("packages/vesper-check-dyn")
   .entry("check_dyn", true)` in a debug test binary.

## What is left

- **Hybrid swap** (`crates/vesper-hybrid` module `dyn`): a swap point in
  `check_package_with` and an AST word encoding in each direction (the
  passes take source today, while the hybrid build has a parsed module). A
  scoreboard row with whole-corpus numbers comes with it.
- `check_forwards`, `refuse` and `DynObjects`, which read the checked world
  and run after type checking, need `@vesper/check-data` World fields.
