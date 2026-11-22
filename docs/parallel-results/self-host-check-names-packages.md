# Self-hosting SH2 lane 6: name resolution, packages and interfaces

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1178. Base: `main` at `e43a56f`. Lane prompt:
[`docs/agent-prompts/self-host-sh2/06-names-packages.md`](../agent-prompts/self-host-sh2/06-names-packages.md).
Rust stays the default everywhere, and no Rust code was deleted.

This is the lane's first PR (part of #1178). It ports the package assembler,
the largest and most self-contained part of the lane. The rest is listed
under "What is left".

## Summary

- **`@vesper/check-names-packages`** (`packages/vesper-check-names-packages`):
  `Loader::assemble`, `Resolver` and `Renamer` of `src/packages.rs` (about
  1,000 of its 1,850 lines; the rest is the loader's file and manifest I/O,
  which stays in Rust). It parses each module with `@vesper/ast`, builds
  every module's name table, resolves `use` imports and `pub use`
  re-export chains across packages to a fixed point, gives the root
  module's re-exports their root names, renames every item and reference
  to its internal name, shifts every span into the virtual concatenation of
  the sources, and reports E0111, E0112 and E0113 with Rust's codes, spans,
  messages and help.
- **Census design.** One interner for every name; modules, uses, use names,
  enums and items are flat rows; the `Table`, `reexported`, `renamed` and
  `assemble` maps are `FlatTable`s keyed by packed `(u32, u32)` pairs. No
  `String`-keyed map and no `Box` tree: the renamer rewrites the
  `@vesper/ast` arenas in place.
- **Differential: 0 differences** on all 80 corpus packages that load
  (85 package directories under `tests/fixtures`, `examples`, `packages` and
  `crates`; the other 5 stop at manifest errors, which stay in Rust), the
  loader's reject fixtures included, and on five mutant package graphs
  that fire every assembler diagnostic. The compared text holds every
  diagnostic's file, code, span, message and help, the qualified-name map,
  the public items, and every module's renamed items, trait bounds and
  model uses in the canonical AST dump (every name and span of the tree).
- **Speed.** Wall clock: the whole load takes 0.72x to 0.87x of Rust's
  time even though Vesper parses every module twice, and the assembler alone
  takes 0.37x to 0.44x. Instructions (callgrind): the assembler alone is
  0.99x of Rust; the whole load is 1.54x because of the second parse, and
  would be about 0.89x without it (lane 1).

## What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-check-names-packages/{Loom.toml,README.md,src/*.ves}` |
| Test | `tests/self_host_check_names_packages.rs` |
| Bazel data and timeout for the test | `BUILD.bazel` |
| Scoreboard row | `docs/maintenance/self-hosting-plan.md` |

No field was added to `@vesper/check-data`: the assembler runs before the
checker builds a `World`, so it needs none. It uses check-data's
`pack_text` for the harness's text entries.

### How the differential works

`packages::load` is the reference. The test mirrors the loader's I/O
(`Loader::package`: manifests, the path-dependency closure in load order,
module files and their paths) and passes the graph and the sources to the
Vesper entry `assemble_text`. Rust's side is the loader's diagnostics after
the parse diagnostics (the loader reports those first; both sides parse
with parsers already shown equal by SH1 lane 9), `SourceMap::qualified`,
`Module::public_items` and, per module, its slice of the assembled
module's items, trait bounds and model uses, dumped with a copy of
tests/self_host_ast.rs's dumper. The qualified-name lines are sorted on
both sides (a map's order is not part of its meaning).

The mutants (`every_assembler_diagnostic_matches_rust`) cover: no module,
no item (with and without the `.vsys` help), not a dependency, an optional
dependency, `super` above the root (in a `use` and in a path), a private
item, an item both declared and imported, a name imported twice, one item
re-exported under two root names, a variant, operation or trait declared
in two modules, qualified paths that name a module or a missing module,
variants of unimported enums, root items reached unimported, names inside
nodes the AST lowering shares (array repetitions, or-pattern bodies, generic
impl bounds) in a file with a nonzero base, and the
accepted forms: re-export chains, dependency paths, generics, bounds,
handlers and closures.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed. The
bootstrap-stage1 workflow runs on the branch; its result is in the PR.

## Numbers

Release build, Linux x86-64, best of six ABBA runs per side
(`CHECK_HARNESS_SPEED=1 cargo test --release --test
self_host_check_names_packages speed -- --include-ignored`). Rust's side is
`packages::load` (read the files, parse, assemble); Vesper's is
`assemble_only` (decode the input, parse every module twice, assemble,
no text). "Assembler alone" subtracts each side's parse (Rust: one parse
of every module; Vesper: two runs of `parse_modules`); Rust's figure still
includes reading the files and manifests.

| Package | Modules | Whole, Rust | Whole, Vesper | Ratio | Parse ratio | Assembler alone, Rust | Vesper | Ratio |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| `packages/vesper-compiler` | 33 | 24.0 ms | 20.9 ms | 0.87 | 0.53 | 7.2 ms | 3.1 ms | 0.43 |
| `packages/vesper-ast` | 15 | 46.9 ms | 33.8 ms | 0.72 | 0.43 | 13.6 ms | 5.3 ms | 0.39 |
| `packages/vesper-check-data` | 7 | 8.7 ms | 7.4 ms | 0.85 | 0.51 | 2.5 ms | 1.1 ms | 0.44 |
| `packages/vesper-std/collections` | 7 | 4.2 ms | 3.6 ms | 0.85 | 0.54 | 1.4 ms | 0.5 ms | 0.37 |

Wall clock on these hosts moves by up to 30% between runs. The
instruction counts below are the steadier number.

Instruction counts (callgrind, release, `packages/vesper-compiler`, 33
modules; collected only inside the measured call with `--toggle-collect`,
the JIT compile excluded; `callgrind_one_side`):

| Measured call | Rust Ir | Vesper Ir | Vesper / Rust |
|---|--:|--:|--:|
| Whole (`packages::load` / `assemble_only`) | 157,558,143 | 242,545,243 | 1.54 |
| Parse every module once | 120,386,660 | 102,863,091 | 0.85 |
| Assembler alone (whole minus one parse; Vesper minus two) | 37,171,483 | 36,819,061 | 0.99 |

The instruction counts are less kind than the wall clock. The assembler
alone runs about as many instructions as Rust's (0.99x), and the whole
Vesper load runs 1.54x of Rust's instructions because it parses every module
twice (gap N1). Vesper still wins on wall time (0.72x to 0.87x above): it
interns names and allocates far less than Rust's cloned `Item` trees and
`String` maps, so it spends fewer cycles on memory, which callgrind's
instruction count does not show. Once lane 1 lets the module keep each
parsed AST, the second parse goes away: about 103M Ir here, which brings
the whole load to about 0.89x of Rust's instructions.

Why it is faster: Rust's assembler clones every item before renaming it,
keys its tables by `String` and `Vec<String>` paths, and rebuilds
`format!` strings for every lookup; Vesper compares interned `u32`
symbols, hashes packed pairs, and renames in place.

## Compiler gaps

| # | Gap | Minimal repro | Workaround here | Fix |
|---|---|---|---|---|
| N1 | A struct that owns sequences cannot be read out of a `ChunkSeq` (census C5) | `struct L { a: ChunkSeq[u32] }`, a `ChunkSeq[L]` `ls`, then `ls.try_get(0)` is E1522 | each module is parsed twice, once for its tables and once to rename it: the two parses are about 85% of Vesper's time | SH2 lane 1 (#1173): keep one parsed module per row |
| N2 | `FlatTable` values must be `copy` (census C6) | `flat_table_new[u32, (u32, u8)]` works, `flat_table_new[u32, ChunkSeq[u32]]` does not | (target, kind) packed into a `u64`; scope lists as ranges of one pool | SH2 lane 1 |
| N3 | A `const` cannot be imported from another module (check-data gap 1) | `const K: u32 = 1;` in `a.ves`, `use self::a::{K};` in `b.ves` is E0111 | codes and kinds are functions (`unknown_import()`, `kind_both()`) | open |

Not gaps, but notes for later lanes:

- `effect` is a keyword, so it cannot name a function (`fn effect(..)` is
  E0002); the renamer's function is `one_effect`. This is the
  manuscript's grammar (unlike `eff`, which lane 3 makes contextual).
- The AST lowering shares nodes where Rust's parser clones them: an array
  repetition's element (`[x; 2]`), an or-pattern arm's body, and one
  `TraitRef` between the trait bounds of every method of a generic `impl`
  (`impl [K: Ord] T[K] { .. }`). Code that rewrites the AST in place must
  treat it as a graph: the renamer marks renamed nodes, renames a revisited
  node again from its file-local span, and moves its span once (`revisit`
  in `src/rename.ves`; the `shared` mutant covers all three).

## What is left

1. The hybrid swap: a swap point (`AssembleTable`) in `src/packages.rs`
   taking the loaded graph, and the `names_packages` module of
   `crates/vesper-hybrid` that returns the renamed modules through the
   parse swap's AST reader (`crates/vesper-hybrid/src/parse_words.rs`).
   Once lane 1 lands, the module keeps each parsed AST instead of parsing
   twice, and the swap can reuse the parse swap's ASTs.
2. `src/interface.rs` (`vesper interface` export over the `World`) and
   `src/package_graph.rs` (the graph report and DOT output).
3. The `World` construction half of `src/check.rs` (items, generics,
   imports into the `World` tables), on `@vesper/check-data`'s fields.
4. A Linux port shard 7 entry once the module has a divergent build to
   check (today the differential runs in `Test all targets` and Bazel).

## Value enums and importable constants (#1310)

#1239 added value enums (`enum E: T { A = e, .. }`) and importable
constants to `src/packages.rs`, and the crate ports moved to them (#1263,
#1266, #1268). The port did not know them, so 11 crate port packages
differed from Rust. CI missed this because the test's Bazel data left out
the crate ports, which are under `crates/*/vesper`.

The follow-up ports #1239's changes:

- `lookup` falls back to the constant `const$NAME`.
- `Resolver::group` handles value enums that are declared or re-exported,
  listed by `(module, enum)`.
- Group imports, including the E0112 for a private enum and the E0113 for
  an enum name that is both declared and imported or imported from two
  enums.
- Re-exported groups, and root re-exports of constants, which keep
  their `const$` prefix.
- `m::E::M` paths.
- In the renamer: `E::M` member names, literal patterns for imported
  constants, untyped members as literals, and calls of imported
  constants, with an empty callee span as the lowering gives.

A `values` mutant covers each form. The Bazel data now includes the
crate ports' `:files`, and the package floor is 75 everywhere.

The cost of the fix, measured with callgrind on `packages/vesper-compiler`
(whole Vesper load):

| Build | Vesper Ir | Rust Ir | Ratio |
|---|--:|--:|--:|
| main before the fix | 262,135,502 | 165,218,242 | 1.59x |
| the fix | 267,391,635 | 165,218,242 | 1.62x |

The fix adds 2.0% to Vesper's load. One parse of every module is now
104.8M Ir, so the assembler alone is about 57.7M Ir, against 52.5M before
the fix. An earlier draft of the fix checked every name expression through
two hash tables. That cost 20M Ir, so the constant checks now use an array
indexed by symbol id, and a callee node the renamer makes is recognized by
its index. Wall clock on this host moved by more than the fix costs, so
the earlier wall table is not re-measured here.
