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
input). The loader supplies the type names declared in its `.vsys`
libraries, including per-target files, as an optional graph suffix. The
assembler resolves those names through the owning package's root module
and preserves their global spellings. Graphs that omit the suffix have
no foreign type entries.

## Assembly API

- `assembly_begin(in Str) -> Assembly` builds the resolver tables from the
  encoded package graph.
- `assemble_next(inout Assembly, in Str) -> Option[AssembledModule]` takes
  the same encoded graph and yields the next owned, renamed AST and its file
  index. It yields `None` when the cursor reaches the end. Spans are relative
  to the virtual concatenation and symbols belong to each AST's interner.
- `assembly_metadata(sink Assembly) -> AssemblyMetadata` releases the
  resolver tables and returns the public names, qualified-name map, file
  display names and offsets, and diagnostics gathered by the cursor. Pull
  every module before taking the metadata for the whole package graph.

## Differential and timing entries

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

- A module's `Ast` owns sequences and the assembly cursor returns it by
  value. Callers can process modules as they arrive or keep individual
  ones for later passes. Each module is parsed twice, once for the resolver
  tables and once to rename it.
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
fire every assembler diagnostic, compared against `packages::load`. A
separate consumer keeps two owned ASTs live across calls and checks the
file and package metadata against the Rust loader. `CHECK_HARNESS_SPEED=1`
runs the ABBA timing. The ignored `callgrind_one_side` test measures one
side under callgrind.
