# Source macro acceptance (#108)

This lane owns acceptance tests, fixtures, scripts and workflow only. Production
source parsing, typed quotation, resolved lowering, compiler authority and CLI
orchestration belong to the other claims in `macro41-contracts.md`.

## Actual boundary exercised

`tests/forge_cli.rs` has eight integration tests. Fifteen source fixtures exercise
the explicit `forge check` / `forge expand` path, followed by ordinary `check`
and explicit `run` of successful pure expanded artifacts. The fixtures check
identity/addition, capture in both directions, an initializer's outer binding,
definition-site helper identity despite a caller shadow, block arguments and
strings/comments which resemble macro syntax. Positive results are 42; changing
the repeated input to 20 must produce 40 with current Unicode/CRLF provenance.

The tests require actual expanded bytes and both BLAKE3 identities, distinct
expanded occurrences mapping to one original argument, and whole-expanded-function
audit scope. A double linear consumption must produce the normal checker's E0303
with two distinct expanded spans and its actual earlier-use cause. A retained
trailing semicolon must produce E0200. Failed expansion never writes a partial
artifact to stdout, and original source/configuration files remain unchanged.

Authority cases require real transitive Log use and actual implicit
Fail[ArithOverflow]/Fail[RuntimeError] requirements to pass both macro emits and
caller ceiling. A helper with the same declared Fail row but a pure body must
remain permitted; changing its body to checked addition must be denied under
the default pure macro policy. Effectful accepted artifacts are checked but are
not run with unhandled effects.

Malformed categories/arity, unsupported nested calls/generic emitted operations,
forged configuration approval, duplicate policy keys/grants, unknown identities,
reserved package names and source/policy byte limits fail closed. Ordinary batch
checking and a real framed stdio LSP session continue to reject unexpanded source;
a client `approved` property cannot make hover available.

## Reproduction and workflow

Run `scripts/test-source-macros.sh`; it builds the actual CLI unless
`VESPER_SOURCE_MACROS_BINARY` names an existing binary. The Python harness uses
only the standard library and executes the same real pipeline with disposable
owned files. Rust and scripts prefer an existing `.exe` unconditionally, bound
command time/output and clean up only their own temporary directories.

The native workflow runs Linux, Windows and macOS parser/host/compiler/CLI tests,
strict Clippy, formatting and the executable Python harness. Its symmetric
push/pull-request filters include all `src/**`, consumed syntax/core/Forge/span/
diagnostics/LSP/doc/runtime/runner crates, WIT, fixtures and scripts. It checks that
the default CLI's normal dependency graph contains neither Wasmtime nor the
sandbox crate.

A separate Ubuntu job runs the actual structured WASI component, hostile wire,
raw record and fresh-host cache suites. This is a standalone foundation; it does
not assert source Tier2 integration or implicit component execution.

## Validation checkpoint

- Private-target `cargo test -p vesper --test forge_cli --no-run`: passed.
- Private-target `cargo clippy -p vesper --test forge_cli -- -D warnings`: passed.
- Rust formatting, Python compilation, shell syntax and `git diff --check`: passed.
- Read-only execution of the structured foundation's built `quotation_execution`
  test binary: all four tests passed, including actual committed component run,
  hostile wire rejection, sandbox determinism/controls and legacy-world refusal.
- Actual Forge CLI runtime and native three-OS CI are pending the integrated
  source pipeline. Static test compilation is not runtime acceptance evidence.

The supported claim is fixed Tier1 Expr quotations in the explicit pipeline.
No source Tier2, derives, package linker, incremental macro queries, implicit
native/WASI execution or latency guarantee is asserted here.
