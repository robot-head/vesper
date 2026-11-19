# P3b gaps lane 8: qualified paths and submodule items

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1093. Base: `main` at `9305b24`. Prompt:
[08-modules-and-paths.md](../agent-prompts/p3b-gaps/08-modules-and-paths.md)
(plan PR #1098).

## Summary

The module gaps the P3a and P3b ports reported are closed in the package
loader and the parser. No backend changed: the loader still assembles a
package into one ordinary module, so the evaluator, the JIT and native code
see the same program as before, only with names resolved that used to be
refused.

| Gap (report) | Before (`9305b24`) | After |
|---|---|---|
| `self::prefix::decode_int(..)` (Prism H3 2, Pact 3) | `E0002 expected RBrace, found ColonColon` | checks and runs |
| `use .. as` (Prism H3 2) | already worked (`use self::enc::{run as enc_run}`) | unchanged, now with paths too |
| `.vsys` library from a submodule (Kinetic 5) | already worked without `use`; `use self::Crmath` gave E0111 | unchanged; the E0111 now says a library needs no import |
| exports only from `lib.ves` (Flux 2) | submodule `pub fn`s never exported | `pub use self::m::f;` in `lib.ves` exports `f` |
| `use self::reader::{u64}` (Aegis 4) | `E0101 unknown type '@demo/shadow::reader::u64'` | `u64` stays the scalar type |
| package error offsets (Kinetic) | a `.vsys` package's errors printed as `at 1234..1240` in the combined sources | file, line and column |
| `Result::Ok(())` pattern (Prism H3 4) | already parsed on this base | regression test added |
| `f(in q[..])` (Flux 1) | `E0002 expected expression` | `q[..]`, `q[a..]`, `q[..b]` |
| `state`, `linear`, `with` as names (Bench 5, Pact 6, Chorus 5) | `E0002 expected a lower-case identifier` | accepted as names; other keywords are named in the diagnostic |

## What changed

### Qualified paths (`src/packages.rs`, parser)

A path may start with `self::` (the package root), `super::` (the parent of
the current module, repeatable) or `@scope/name::` (a dependency), wherever
an item name may appear: a value, a struct literal, a type, an effect row or
a pattern. The parser keeps the path as one name (`self::codec::shape::Point`);
the loader resolves it:

1. the root gives a package and a base module;
2. the longest prefix of the remaining segments that is a module of that
   package is the module, provided at least one segment is left;
3. the next segment is the item, looked up like an import (it must be `pub`
   unless the path names the current module; `E0112`, else `E0111`);
4. any further segments (`::new`, `::Small`) are kept after the item's
   internal name, exactly as an imported head is today.

A path needs no `use`. `use super::m::{f}` is the same rule for imports.

### Re-exports and submodule exports

`pub use` is no longer refused. It binds its names like `use` and makes them
items of the module, so another module can import them, reach them by
path, or re-export them again; a chain resolves to the original item (a
fixed point over all modules before names are bound).

**Export rule (decided here).** A library still exports the `pub fn`s of
its root module, and the root module exports a submodule or dependency
function by re-exporting it:

```vesper
// src/lib.ves
pub use self::frontier::{advance, merge as frontier_merge, Pair};
```

exports `vesper_<lib>_advance` and `vesper_<lib>_frontier_merge`. An item
the root module re-exports takes its root name as its internal name, so the
export path, the binding generator, `// @export instance` and diagnostics
all see it as a root item, and one item has exactly one exported name
(re-exporting it twice from the root is `E0113`). Submodule `pub fn`s that
are not re-exported stay internal, so no existing library gains symbols.
The alternative, exporting every submodule `pub fn` under a mangled symbol,
was rejected: it would add symbols (and export ABI refusals for helper
types) to every existing port, and the binding names would carry module
paths.

`src/native_export/directives.rs` did not need to change: directives are
still read from the root module, which is where the re-exports are.

### Namespaces

Each item lives in the namespaces of its kind: a function in the value
namespace, an effect in the type namespace, a struct or enum in both. An
import binds only those, and the root-module names a submodule may not use
unimported are kept per namespace too. `use self::reader::{u64}` therefore
binds the function `u64` for calls and leaves `u64` in types (including
`ChunkSeq[u64]` and `f[u64](..)`) the scalar type.

### Per-file diagnostics

Packages that import `.vsys` libraries are checked by the export path,
whose errors carried offsets into the combined sources. When that build
fails, `vesper build --lib` now checks the module again through the Bridge
lowering and reports each diagnostic at its own file and line (and as JSON
with `file` under `--json`). Packages without `.vsys` imports already did.

### Parser

- Paths are accepted in every name position: expressions, struct literals,
  patterns, types, generic arguments (`ChunkSeq[super::m::T]`,
  `f[self::m::T](..)`), effect rows (`eff [c: self::m::E]`) and `handle`.
- `p[..]`, `p[a..]` and `p[..b]` mean `p[0..p.len()]`, `p[a..p.len()]` and
  `p[0..b]`. An open upper bound copies the place path `p` (a name and
  fields; anything else is refused with a diagnostic), and the copy gets
  synthetic spans that start inside the `..` token, where no source node
  starts, so span-keyed tables never see two nodes at one span.
- `state`, `linear` and `with` are names wherever a lower-case name is
  expected (bindings, parameters, fields, members, expressions); they stay
  keywords where their forms start. Every other keyword in a name position
  now gets `expected a lower-case identifier, found the keyword `match`,
  which cannot be a name`.

## Manuscript changes (flagged)

- §2.2: `UseDecl` takes `Vis?` and a `super` root; `Path` takes a
  `PathRoot` (`self::`, `super::`, `@scope/name::` and module segments) and
  a lower-case final item; `IndexSel` allows open bounds; a "Contextual
  names" paragraph lists `state`, `linear`, `with` and describes open
  intervals.
- §11.3 "Modules and imports": new "Namespaces", "Qualified paths" and
  "Re-exports" paragraphs (the export rule above); the sentence that ruled
  out `pub use` and inline qualified paths is replaced. A qualified path in a
  single file is `E0110`, like `use`.
- §13 export ABI: a package's exports are its root module's `pub fn`s,
  including its `pub use` re-exports.

## Tests

- `tests/package_paths.rs`: every path form in one package with a
  dependency, and the Prism H3 and Aegis shapes, each run by the evaluator,
  the JIT with and without optimization, `vesper run`/`jit`, and (outside
  Windows) a linked executable, which all agree; every new diagnostic
  (private item by path, unknown module, `super` above the root, double
  root re-export, unimported root re-export, a path in a single file); the
  parser fixes (contextual names, the three open intervals over `ChunkSeq`
  fields and `Str`, `Result::Ok(())`), and the keyword diagnostic.
- `tests/package_submodule_exports.rs`: `pub use` exports in the assembled
  module, the evaluator and the `--interface` JSON; a submodule calling a
  `.vsys` library; a `.vsys` package error at its own file and line.
- Existing suites: `tests/packages.rs`, `tests/package_cli.rs`,
  `tests/package_graph_cli.rs`, `cargo test -p vesper-syntax`.

## Measurements

Setup: cloud x86-64 container (4 vCPU), Rust 1.97 `--release`. "Main" is a
release `vesper` at `9305b24`; "branch" is a release `vesper` at `70d28e4`
(this lane's first commit; the later commits change only how paths in effect
rows, handlers and type arguments parse, which no port uses). Instructions
are callgrind `Ir`, whole process. This lane changes name resolution only,
so the expected result is "no change", and that is what was measured.

**Existing ports compile to the same bytes.** Every port library was built
with `vesper build --lib` by both compilers and the outputs compared
(interface JSON, `.text`, whole object):

| Ports | Result |
|---|---|
| aegis, alloc, aura, beacon, bench, crucible, ebpf, grammar, loom-audit, loom-calculus, loom-solver, pact, prism-h3, quarry, semver, serde, silicon, simpulse, tensor (main) | interface, `.text` and object byte-identical |
| flux (#1074), kinetic (#1078), chorus (#1081) (open PR branches) | byte-identical |
| ignis, warp-bbr | not built this way (they need their crate's build flags); `cargo test` on CI covers them |

So every committed port's numbers are unchanged by construction.

**Local rewrites (not committed; for lane 12 or the port lanes).**

- *Prism H3:* the `enc_*`/`dec_*` names undone: `encoder.ves` and
  `decoder.ves` declare `encode`, `decode_section`, ..., and `lib.ves` calls
  `self::encoder::encode(..)` with no import list
  (8 functions; the patch is in the project files as
  `p3b-gaps/prism-h3-qualified-paths-local-rewrite.patch`). The
  interface JSON, and so the binding, is identical; `cargo test -p
  vesper-prism-h3 --features vesper-port` passes with the branch compiler.
  `examples/abba once <w> 20`:

  | Workload | Main, original | Branch, rewrite | Change |
  |---|---:|---:|---:|
  | decode | 710,596,532 | 710,596,848 | +0.00004% |
  | encode | 326,777,158 | 326,776,514 | -0.0002% |
  | frames | 253,157,707 | 253,157,953 | +0.0001% |
  | messages | 389,552,336 | 389,552,583 | +0.0001% |

- *Flux (PR #1074 branch):* the frontier functions moved out of `lib.ves`
  into `frontier.ves` (`live_holds`, `acquire`, `valid`, `advance`,
  `retire` and `FrErr`) and exported with
  `pub use self::frontier::{FrErr, live_holds as frontier_live_holds, ..}`.
  (project files: `p3b-gaps/flux-frontier-submodule-local-rewrite.patch`).
  The interface JSON is identical; `cargo test -p vesper-flux` passes with
  the branch compiler. `examples/abba worker <w>` over 20 batches, with
  identical output digests:

  | Workload | Main, original | Branch, rewrite |
  |---|---:|---:|
  | join | 1,452,104,003 | 1,452,103,986 |
  | example | 106,572,437 | 106,572,420 |
  | resume | 170,123,266 | 170,123,249 |

- *Pact, Aegis, Kinetic, Chorus, Bench:* their gaps here were syntax
  (`self::m::g()`, `use self::reader::{u64}`, `use self::Crmath`, `with`,
  `state`); each repro now checks and runs (tests above). Their sources are
  unchanged, so their numbers are the byte-identical rows above.

## Stage 1 guard

No edits to `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`. The
`Bootstrap Stage 1` workflow (`bootstrap/run.sh stage1 --reproduce
--outcome` against `bootstrap/stage1.lock`) passed on this branch at
`70d28e4` (run 44436687340) and was dispatched again after merging `main`.

## What is left

- The self-hosted Stage 1 parser (`packages/vesper-compiler`, Stage 1 guard)
  does not parse paths, `pub use`, open intervals or contextual names; it
  stays stricter, as for receiver loans. Package fixtures stay out of its
  corpus (the new tests write their packages to a temporary directory).
- Imports of whole modules (`use self::m;` then `m::f`) and relative paths
  without a root (`m::f`) are not added; the manuscript says so.
- `vesper fmt` keeps its existing layout for destructuring `let` patterns
  (`let S {\n a\n }\n = s;`), unrelated to this lane.
- Port adoption (undoing `enc_*`/`dec_*`, moving Flux's frontier into a
  submodule, Aegis `rd_u64` -> `u64`, Bench/Pact renames) is lane 12's or
  the port lanes' to commit.
