# Driver front-end steps in Vesper (hd_step group 2, #1893)

Part of removing `HybridDriver::hd_step` (#1367) for the Stage 2 retirement
(#1397). The group's steps are `Parse`, `EmitCst`, `ReportSyntax`, `Check`,
`EmitHir`, `EmitInterface` and `LibBuild` of
`crates/vesper-hybrid/src/driver.rs`.

## Slice 1: Parse and EmitCst for a standalone source

The driver module (`crates/vesper-hybrid/modules/driver`, new
`src/front.ves`) now runs these itself:

- **`--emit cst`.** The driver parses the source it read with the front
  end v2 and `@vesper/ast` (`lower_source`) and prints rowan's
  `{:#?}` dump from the builder events: `SyntaxKind(<raw>)@<start>..<end>`,
  two spaces per level, and each token's text as a Rust `str` Debug
  (`@vesper-std/toml::debug::put_debug_str`), cut at the first char
  boundary from byte 21 with ` ...` when it is 25 bytes or longer. Output
  goes to stdout or to `-o` through `@vesper-std/fs`, with the session's
  `io::Error` text on failure. No session pass runs after `Load`.
- **`Parse`.** The driver runs the parse module's `parse_words`
  (`@vesper/hybrid-parse`) and hands the stream to the session through the
  new `hd_parsed` host call, which decodes it with `parse_words::decode` as
  the parse swap does. The compiled text is the source as read and the sugar
  map is empty, as `forge_frontend::parse_compiler_input` leaves them for a
  parse with no diagnostics.

The session's own `Parse` and `EmitCst` still run for a package (its module
comes from the loader), `--include-tests` (the test-block parse,
`parse_for_tests`), a source over the 1 MiB byte budget, and any parse with
diagnostics: those go through source-sugar expansion and the Forge macro
path, which are not ported yet (`forge_frontend`, owned by #1874's thread).
So `ReportSyntax` stays in the session for now.

### Results

- Differential (`crates/vesper-hybrid/tests/driver_front.rs`): the hybrid
  and the default compiler over every `.ves` file in `examples/`, `tests/`,
  `packages/` and `crates/*/vesper/`, plus edge-case sources (long and
  multibyte tokens, escapes, CRLF, empty, syntax error, sugar). Commands:
  `emit --emit cst` to stdout and to `-o`, `-o` into a missing directory,
  and for `examples/` and `tests/` also `check`, `check --json` and
  `emit --emit hir --no-opt`.
  **`flip-gate differential differences=0 cases=4254`.**
- `emit --emit hir` without `--no-opt` differs on main's hybrid too
  (`tests/fixtures/wide_literals.ves`: the AST optimizer swap folds a
  `wrapping_mul` call that Rust's optimizer keeps). That is the optimizer
  swap's, not this slice's, so the differential runs it with `--no-opt`.
- Speed, ABBA (20 epochs of 5 runs each way, release, this host), hybrid over
  the default compiler:
  - `emit --emit cst packages/vesper-wasm-backend/src/lower.ves`: median
    **0.45x** (0.43..0.47). Main's hybrid, which runs the session's
    `EmitCst`, is 0.70x.
  - `check examples/counter.ves`: 1.12x (1.10..1.17); main's hybrid is
    1.09x (1.07..1.19). That gap is process startup, not the parse.
  - `check packages/vesper-std/toml/src/debug.ves`: 1.05x (1.01..1.09).
- Rust lines deleted: none yet. The session's `Parse`/`EmitCst` code stays
  for the fallbacks above.

## Slice 3a: EmitInterface

`vesper emit --emit interface` now runs the source-contract export of
`src/interface.rs` in Vesper (`crates/vesper-hybrid/modules/driver/src/interface.ves`).
The session still holds the checked world, so its `EmitInterface` step only
resolves and validates the package name and encodes the world's
declarations as a byte stream (`interface_words` in `src/driver/front.rs`:
LEB128 numbers and length-prefixed texts; ADTs with their nominal mode,
effects, traits, conformances and the non-generated functions). The driver
builds the `vesper_semver::Interface` JSON from it: ids with builtins under
`std/core`, type and mode lowering, capability identities (the effect id
and the compact JSON of its arguments), `BTreeMap`/`BTreeSet` order with
last-wins keys, the `unsupported` cases, and serde_json's pretty printer and
string escapes. A new step, `InterfaceWrite` (40), takes the JSON and writes
it to `-o` atomically or to stdout.

### Results

- Differential (`interface_export_matches_the_default_compiler` in
  `crates/vesper-hybrid/tests/driver_front.rs`): every checkable source in
  `examples/` and `tests/` with `--package @t/x`, every package directory
  in `packages/` and `crates/*/vesper/` (99 of them export, the rest fail
  to check and must fail the same way), three sources for generics,
  effects and arrows, to stdout and to `-o`, plus the missing and malformed
  package-name errors. **`interface differential differences=0 cases=1351`**
  (before the `-o` cases were trimmed to one package).
- Speed, ABBA (20 epochs, release, this host), hybrid over the default
  compiler:
  - `emit --emit interface packages/vesper-std/toml` (198 KB of JSON):
    median **0.64x** (95% CI 0.59..0.65).
  - `emit --emit interface --package @t/x examples/counter.ves`: 1.09x
    (1.03..1.12), process startup as in slice 1.
- Rust lines: the hybrid's JSON export (`interface::export` and
  `serde_json::to_vec_pretty`) is no longer called from the driver; the
  world encoder adds about 200 lines that go away once the checked world is
  held in Vesper. `src/interface.rs` stays for the default compiler until
  Stage 2.

## Next

- Slice 2: `EmitHir` from the Vesper AST (serde's pretty JSON of
  `ast::Module`).
- Slice 3b: `LibBuild`'s orchestration over the world the session keeps.
- `ReportSyntax` and the fallbacks follow the source-sugar and Forge ports.
