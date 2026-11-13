# Actual function-parameter syntax inventory — claim #159

The syntax lane implements the frozen API in params41-contracts.md:
`MacroSource::parameter_name_spans(FunctionId) -> Result<&[Span], Error>`.
It returns a private retained inventory aligned with the actual parsed header,
not spans supplied by a public SourceFunction clone. Source-bound FunctionIds
reject another source even when its function headers are unchanged.

Each name is the actual Lower or SelfKw lexer token within its real Param.span,
following an optional in/inout/sink token. The inventory keeps source/header
association, order and exact byte ranges through comments, CRLF and Unicode
prefixes. It also covers ordinary support functions without changing their
generic/projected/duplicate syntax or adding a parameter-count restriction.
No parser changes, manufactured Function or placeholder body were needed.

Macro-bearing functions now accept plain nongeneric parameters. Projected or
generic forms still refuse explicitly. Their parameter count shares the existing
max_parameters limit with macro slots: default8, HARD32. Original duplicate names
refuse with DuplicateName before later alpha-renaming can repair invalid source.
Parameter types, pass semantics and linear obligations remain the responsibility
of the actual final compiler checker; this syntax snapshot grants no authority.

The new retained-span storage and monotone header scan are charged before
allocation/traversal. Exact name comparisons and duplicate comparisons consume
bounded work. Outer inventory growth uses exact reservations with copy work
prepaid, preserving the existing finite source/token/metadata/work limits.

Validation on Linux, Rust1.99.1:

- `cargo test --locked -p vesper-syntax`:71 tests in7suites, zero failures or
  ignored tests, including both private-construction compile-fail doctests.
- Eight new parameter tests cover all pass tokens, self, actual comment/Unicode
  offsets, immutable inventories, function/source identities, duplicate refusal,
  default/HARD count boundaries, unchanged support forms and every UTF8 prefix.
- The existing12 source-macro syntax tests pass. Their obsolete parameter-only
  unsupported case now uses a still-unsupported projected parameter.
- Strict all-target syntax Clippy, rustfmt and whitespace checks pass.

The work used the dedicated existing syntax cache and did not install or build
Wasmtime. No workflows, Bazel/build files or test wrappers reserved by #158 were
changed. Full source compilation, hygiene, actual CLI/stdio and native-platform
coverage belong to the other claimed lanes and integration.
