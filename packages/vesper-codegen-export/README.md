# @vesper/codegen-export

The export ABI of `vesper build --lib` in Vesper (self-hosting wave SH4,
lane 12, #1286). It ports `src/native_export.rs` and `src/native_export/**`.
Rust stays the default build: nothing in the compiler calls this package yet.

## What is ported

| Rust | Vesper | Differential |
|---|---|---|
| `rust_binding` (binding.rs, binding_v2.rs, binding_v4.rs): ABI v1-v4 | `binding.ves`, `binding_v2.ves`, `binding_v4.ves` | byte-identical text |
| `Directives::parse` (directives.rs) | `directives.ves` | equal dumps |

Not yet: `plan_with` (the interface planner, which reads the checked
`World`), `glue::define` and the wrapper signatures (emission, after SH4
lane 1's lent Cranelift handles), `Directives::add_packages` and the `.vsys`
descriptor parse.

## Entries

- `binding_text(in input: Str) -> ChunkSeq[u64]`: the binding text of one
  `LibraryInterface` and `BindingOptions`, encoded by
  `tests/self_host_codegen_export/words.rs` (its doc comment is the word
  format), returned as `pack_text` words.
- `directives_dump(in input: Str) -> ChunkSeq[u64]`: `Directives::parse` of
  a source for one target, as the dump `directives_dump` in
  `tests/self_host_codegen_export.rs` writes.

## Layout

- `iface.ves`: the interface tables (`Iface`): names are
  `@vesper/check-data` symbols, `AbiType`s are hash-consed nodes, so equal
  types have equal ids. The type predicates `owning`, `words` (`abi_words`),
  `mirrored`, `lent`, `handle_of` and `array_elem` live here.
- `text.ves`: byte-sequence helpers and `StrSet`, the sorted string set the
  generators need for Rust's `BTreeSet<String>`, `BTreeSet<(String, u32)>`
  and `BTreeMap<String, u32>`.
- `binding.ves`: `Gen` (interface plus options), the v1 generator and the
  shared pieces (`rust_type`, `ident`, `camel`, `error_variant`, copy types).
- `binding_v2.ves`: the v2 and v3 generator: handle types, views, wrappers,
  methods, `repr` mirrors.
- `binding_v4.ves`: trees, `<Elem>Seq` views and lent extern handles.
- `directives.ves`: the directive reader and a parser for instance type
  arguments.

## Tests

`tests/self_host_codegen_export.rs` builds every library a port crate links
(`PORTS`, the `Port::new` calls of `crates/*/build.rs`) with the `vesper`
executable and compares the bindings, and reads every `@export` source in the
repository plus a set of edge cases for the directives.
`CODEGEN_EXPORT_PORTS=aura,warp` limits the corpus.
