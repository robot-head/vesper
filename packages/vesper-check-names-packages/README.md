# @vesper/check-names-packages

The package assembler of `src/packages.rs` in Vesper (self-hosting wave SH2,
lane 6, #1178): `Loader::assemble`, the `Resolver` and the `Renamer`. Rust
stays the default; nothing here replaces Rust code yet.

Given the package graph the Rust loader read from disk, it parses every
module with `@vesper/ast`, builds each module's table of names, resolves
`use` imports and `pub use` re-exports across packages to a fixed point,
gives the root module's re-exports their root names, renames every item and
reference to its internal name (`demo__geometry__shapes__area`), shifts
every span to its file's offset in the virtual concatenation, and reports
E0111 (unknown import), E0112 (private item) and E0113 (ambiguous name)
exactly as Rust does.

| File | Rust | Vesper |
|---|---|---|
| `src/state.ves` | `Loader`, `Table`, `Resolver`'s maps | `St`: one interner, flat rows, `FlatTable`s keyed by packed `(u32, u32)` pairs |
| `src/load.ves` | `Resolver::new`, `Kind::of` | the input decoder, `collect` (a module's names, `pub` set, imports, enums) |
| `src/resolve.ves` | `Resolver` | `name_of`, `mangle`, `shown`, `root_of`, `use_module`, `lookup`, `reexports`, `overrides`, `qualified` |
| `src/rename.ves` | `Renamer` | in-place renaming of the `@vesper/ast` arenas |
| `src/lib.ves` | `Loader::assemble`, `generic_scopes`, `global_names` | `bind`, `assemble_module`, the entries |

The loader's I/O and manifest checks (`Loader::package`) stay in Rust: the
test mirrors them and passes the graph in (`src/load.ves` documents the
input).

## Entries

- `assemble_text(in Str) -> ChunkSeq[u64]`: the assembled program as one
  text packed by `pack_text` (`@vesper/check-data`): the diagnostics, the
  qualified-name map, the public items, then every module's renamed items,
  trait bounds and model uses in the canonical dump of
  `packages/vesper-ast/src/dump.ves`.
- `assemble_only(in Str) -> u64`: the same work without the text, for the
  timings.
- `parse_modules(in Str) -> u64`: decode and parse every module once, the
  parse share of the timings.

## Design notes

- A module's `Ast` owns sequences, so it cannot be kept in a `ChunkSeq`
  (E1522, SH2 lane 1). Each module is parsed twice: once for its tables and
  once to rename it. Vesper still beats Rust's time, since its parse is about
  half of Rust's.
- The lowering shares some nodes where Rust's parser clones them: an array
  repetition's element (`[x; 2]`), an or-pattern arm's body, and one
  `TraitRef` between the bounds of every method of a generic `impl`. The
  renamer marks the expressions, patterns, parameters and trait references
  it has renamed; a revisit renames again from the node's file-local span
  (the same names and diagnostics as Rust's clones), and the span moves
  once.
- Constants cannot be imported across modules yet, so codes and kinds are
  functions (`unknown_import()`, `kind_both()`).

## Tests

`tests/self_host_check_names_packages.rs`: every Loom package of the corpus
(the loader's accept and reject fixtures included) and mutant graphs that
fire every assembler diagnostic, against `packages::load`, at 0
differences. `CHECK_HARNESS_SPEED=1` runs the ABBA timing; the ignored
`callgrind_one_side` test measures one side under callgrind.
