# `.vsys` types in `use` across packages (SH5 lane 3)

Self-hosting wave SH5, lane 3. Claim #1326, prompt
`docs/agent-prompts/self-host-sh5/03-vsys-packages.md`. Base `bbda89e`
(main), not stacked.

## Repros on main first

| Gap (report) | On `bbda89e` | Scope |
| --- | --- | --- |
| SH4 lane 1 gap 1, lane 9 gap 5: `use @vesper/cranelift::{Builder};` is `E0111 module @vesper/cranelift has no item 'Builder'` | **fails** | **fixed here** |
| SH4 lane 11 gap 4 (shim report gap 4): a package cannot import a `.vsys` library | **passes** once the library's package names its `.vsys` in its manifest (`[package] vsys`, SH1 lane 7, #1164). Calls through the dependency already check, JIT, build and link in `--lib`. `@vesper/cranelift` on main has no such field yet, and SH4 lane 1 (#1309) adds it (`cranelift.vsys`) | nothing to fix in the compiler; tests here cover it with handle types |

So the compiler gap that was left is the name one: a library's types are
global names, so code could use `Builder` without an import, but no `use`
or path could say where `Builder` comes from.

## What changed

* **Loader** (`src/packages.rs`). Each package records the type names its
  manifest's `.vsys` libraries declare (`Package::foreign_types`: extern
  types, wire structs and enums, each with its library). They come from
  `PackageVsys::type_names`, which parses the library for the host target.
  The names do not depend on the target. A library that does not parse adds
  no names, and `Assembled::imports` still reports its error.
* **Resolver.** When `Resolver::lookup` finds no item and no re-export of
  that name in a package's root module, and the package carries a library
  that declares the name, the lookup resolves to that type
  (`Kind::Both`, as for a generated struct or enum). `Resolver::is_foreign`
  makes `name_of` keep the global name, so every importer names one
  nominal type. Because everything goes through `lookup`, these all work:
  `use dep::{T}`, `use dep::{T as U}`, the path `dep::T` (and `dep::E::V`
  for a library enum), and `pub use dep::{T}` in a middle package, followed
  through any chain of re-exports.
* **Edge cases.**
  * A root module's `pub use dep::{T}` is not one of its exported items,
    because the type is not a source item, so interfaces do not change.
  * It does not hide `T` from the package's other modules, which may still
    name the global type without an import, as before.
  * A source item of the same name in the carrying package wins over the
    library type. The importer declaring the name it imports is the usual
    `'T' is both declared in this module and imported`.
  * A package that does not carry the library, or a submodule of one that
    does, still gives E0111 with the existing hint.
* **Manuscript §16.3** (flagged in the PR): the "Packages Carry Their
  Imports" bullet now says a library's types may be imported from the
  carrying package's root module, and that every such name denotes the same
  nominal type.
* **Bazel**: `test/vsys_package_imports` takes `//packages/vesper-cranelift:files`.

No parser change, so the parser mirror rule does not apply.

## Tests

`tests/vsys_package_imports.rs` (9 tests; 7 fail on `bbda89e`, all pass
here):

* `a_dependency_s_extern_type_is_importable`: `@t/app` imports `Counter` (a
  `linear extern type`) and `Step` (a `.vsys` enum) from `@t/counter`, which
  carries `Probe`. It writes its own `inout c: Counter` function and runs in
  the JIT with the optimizer on and off. The answer is 41.
* `every_importer_names_the_same_type`: `@t/mid` re-exports `Counter` and
  aliases it (`Counter as Handle`). `@t/app` uses `@t/mid::{Counter}`, the
  path `@t/counter::Counter` and `@t/counter::Step::Ten`. A handle `@t/mid`
  opens is bumped by `@t/app` and closed by `@t/counter`: 15 in the JIT.
* `cli_checks_and_builds_an_executable_that_imports_a_type`: `vesper check`,
  then `vesper build` links the executable with a C `libprobe.a` and it
  exits 41 (Unix).
* `a_library_build_imports_a_type`: `vesper build --lib` of a library whose
  internal function takes the imported handle. The Rust binding links
  `crate::probe::probe_open`.
* `a_package_imports_cranelift_handles`: the real `@vesper/cranelift`
  source as a dependency, with `use @vesper/cranelift::{Builder, Data,
  Module, Object}` and calls to `Cranelift::vcl_*`. Until the package's
  manifest names its `.vsys` (#1309), the test checks a copy of the package
  with the field added and the library text taken from its directive, so
  the package is not edited. Once the field is on main, it checks the
  package in place.
* `codegen_base_checks_over_its_cranelift_dependency`: loads and checks
  `packages/vesper-codegen-base` as is, over its real `@vesper/cranelift`
  dependency. The package arrives with #1309, and until then the test
  returns early. Locally, with #1309's `packages/vesper-codegen-base` and
  `packages/vesper-cranelift` checked out on this branch:
  * the package checks as is;
  * it also checks with `Builder, Module` added to its `use
    @vesper/cranelift::{..}`;
  * on `bbda89e`, that same edit gives the two E0111s.

  I did not commit any of those files.
* `only_a_package_that_carries_the_library_exports_its_types`,
  `an_imported_type_collides_with_a_declared_one` and
  `a_root_reexport_is_not_an_exported_item` cover the error and interface
  edges.

The existing suites pass unchanged: `vsys_packages` (16), `packages`,
`package_paths`, `package_graph_cli`, `package_submodule_exports` and
`interface`. `package_cli` has 3 local failures that need the `vesper-resolve`
tool built, and they are the same on main.

`self_host_check_names_packages`, the names/packages port against Rust,
reports 13 of 92 differing packages. Every difference is a value-enum or
constant import that the port does not have yet (#1310, PR #1316), and
`bbda89e` gives the same count. No corpus package imports a library type,
so this change adds no differences.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed
(`git diff --name-only bbda89e` lists only `src/packages.rs`, the test,
`BUILD.bazel`, the manuscript and this report). The bootstrap-stage1
workflow runs on the branch before merging; its result is in the PR.

## Numbers

The change adds no code generation and no run-time cost. The loader parses
each carried library once more, for its type names. I did not time it
separately, since it is one parse of a file the loader has already read. No existing package's output changes. This
holds because no package imported a library type before (it was an error),
and the new lookup runs only where the old one failed.

## What the SH4 lanes can do now

* Lanes 1, 7, 8, 9 and 10 (the packages over `@vesper/cranelift`) may add
  `Builder` and `Module` to their `use @vesper/cranelift::{..}` once #1309's
  manifest field is on main. Each lane does that in its own package.
* Lane 11 (#1294, #1315) and lane 13 can depend on `@vesper/cranelift` as a
  real package once #1309 lands, and stop putting the two packages together
  into one source. The calls already worked through a dependency (SH1 lane
  7), and now the types can be imported too.

## Compiler gaps

1. **The names/packages port does not know library types.**
   `packages/vesper-check-names-packages` mirrors `Resolver::lookup`, but it
   gets no carried library type names in its input. A graph like
   `a_dependency_s_extern_type_is_importable` would differ. The fix is to
   pass each package's `foreign_types` in the port's input and add the same
   fallback. That is the port lane's follow-up, because SH5 lanes do not
   edit SH2 packages.
2. **A library name itself cannot be imported.** `use @vesper/cranelift::{Cranelift}`
   is still E0111 with the "not an item" hint. Library names stay global,
   and `Lib::f(..)` and `Ffi[Lib]` need no import. An import that only
   documents where a name comes from would need a language decision, so I
   left it alone.
3. **Library types live only at the carrying package's root.**
   `use @dep::sub::{T}` is E0111 unless `sub` itself has `pub use` for `T`.
   This is by design, and the manuscript says so.
