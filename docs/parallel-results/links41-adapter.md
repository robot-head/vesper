# Checked-source intra-doc link adapter (#179)

Read the published #177–180 contract before writes. Owned changes are limited to
the documentation adapter, its private link attachment module, narrow doc CLI
help/typed-renderer wiring, nine new adapter tests and this report. No checker,
AST/IR/native, Forge, LSP session, dependency, build/Bazel, workflow or test-wrapper
changes. Library commit `3f02e1c` is a dependency only; its local cherry-pick
`61e2e76` must not be imported as adapter-owned work.

Both `document_source` and `document_checked` preserve their actual checking
boundaries. The former retains its parser/checker diagnostics and preflight
guards; the latter uses the actual private CheckedSource's retained bytes, AST
and World. After extracting and validating real documentation metadata and its
typestate graph, the private adapter projects the finite documented declaration
inventory, analyzes the same original source and validates the resulting report.
Only Resolved reports attach to a returned Document. The first actual semantic
link failure includes its code and absolute exclusive-end UTF8 byte range in the
bounded CLI error. No failed link analysis returns a partial document.

Source-defined functions, types, effects, effect operations, trait names and
inherent methods are compared with actual compiler metadata in tests. A source
method on a builtin type, such as `i64::value`, remains a real documented method;
this does not invent a documentation target for the builtin type itself. Private
generated Pact/model helpers, builtin-only declarations and trait methods remain
outside the inventory. No World clone, source hash dependency, external lookup,
source execution, cached approval or proof badge is introduced.

The CLI routes its existing report through `render_document_html` before output
creation, retaining the original file-read, final-symlink, create-new, write and
failure-cleanup behavior. JSON keeps actual link metadata with the complete typed
document. Unverified proof status and all other documentation/search limitations
remain explicit.

## Actual validation

Against the real library dependency `3f02e1c`, with the released private
`/tmp/tooling41-lint-target` cache, one Cargo job, debug0 and incremental0:

- 106 tests across seven suites pass, zero failures or ignores: root library44,
  signature database9, existing doc CLI11, new doc links9, existing doctest14,
  LSP CLI5 and navigation14.
- The new tests verify actual checked World/source inventories; exact direct
  source/sealed-snapshot JSON and HTML parity; genuine type/effect ambiguity;
  unknown and unsupported references with exact original spans; declaration edits;
  Unicode/CRLF and lexer exclusions; actual syntax/type/linear diagnostic
  precedence; source-defined builtin-receiver methods; excluded private helpers
  and builtins; and a genuine 1,025-independent-reference Resource refusal.
- `cargo clippy --locked -p vesper --lib --bin vesper --test doc_links --test
  doc_cli --test doctest_cli --test lsp_cli --test lsp_navigation --test
  database_signatures -- -D warnings` passes.
- `cargo fmt --all -- --check` and `git diff --check` pass.

The first reference-limit fixture used whitespace-separated `[main] [main]`,
which Markdown correctly treats as excluded reference-style links. The fixture
now uses comma-separated independent references and preserves the actual refusal
assertion. The parser-precedence fixture is balanced but syntactically invalid,
so it exercises real parser diagnostics rather than existing delimiter preflight.
The positive source binds a linear method receiver before calling it because
direct temporary linear projection is unsupported by the unchanged core checker.

These are local Linux results, not hosted macOS/Windows evidence, and they do not
claim general Markdown support, whole-workspace testing, a latency/RSS bound or
compiler/proof correctness. Root performs final combined integration and any
later library-hardening validation separately.
