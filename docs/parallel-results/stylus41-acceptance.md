# Macro and partial-buffer formatting acceptance (#113)

Owns only tests/stylus_cli.rs, fixtures/stylus, check/test-stylus scripts,
stylus.yml and this report. Production formatter and CLI/LSP adapters belong to
claims #111, #112 and #114 in docs/rfcs/stylus41-contracts.md.

The Rust harness exercises actual CLI atomic/check-mode behavior and schema1
source/recovery modes, fixed Expr macro formatting followed by explicit real
Forge expansion and ordinary run42, and exact token/comment conservation.
Actual ordinary E0302 linear leaks and E0303 duplicate uses remain checker
failures after formatting. Recovery cases retain damaged gaps, opaque payloads and fake declarations in
comments/strings. Current UTF16 CRLF stdio requests use two actual document
revisions and require their current diagnostic hashes/versions. Formatting
proposes edits while damaged source remains parse_error and hover unavailable.
The explicit CLI can leave wholly opaque buffers unchanged with zero edits.
Malformed lexical/UTF8, unsupported macro grammar, source cap, unknown flags,
directories and Unix symlinks preserve input. No native/WASI execution occurs
unless acceptance explicitly runs the successful checked expanded artifact.

The independent standard-library Python harness executes the same real boundary,
including idempotent zero-edit stdio formatting after applying safe edits and
real current-buffer change notifications. Both harnesses prefer an existing .exe.
Rust command readers cap retained output; Python checks size after capture. All
commands have finite timeouts and only disposable owned files are modified.

The native three-OS workflow retains old formatter/editor/recovery/macro tests,
runs actual leaf/root suites and strict Clippy, and uses exact package names to
verify that the default CLI graph excludes the Wasmtime engine/WASI/sandbox.
Existing native Cranelift internal helpers remain allowed. Push and pull-request
filters cover consumed crates, root source, all selected tests, fixtures/scripts
and manifests equally.

Static checkpoint: private-target `cargo test --locked -p vesper --test
stylus_cli --no-run` and strict Clippy for that actual integration test passed.
Rustfmt, Python AST/bytecode, shell syntax, YAML parsing, symmetric workflow
filters, existing integration target names and `git diff --check` passed. The
Rust suite contains seven tests on Unix, six on Windows.

Final local runtime acceptance used the actual integrated binary from root
checkpoint `a91659a`, including the `e8c22c2` lexical top-level recovery guard.
`VESPER_STYLUS_BINARY` selected that existing binary without writing the
coordinator build cache. The independently precompiled private Rust harness
passed all seven actual CLI/stdio tests with zero ignored. The shell/Python
harness passed 36 real commands, three stdio sessions and one explicit ordinary
run of successfully checked macro-expanded source yielding42. Current revisions,
UTF16 CRLF edit application, zero-edit repeated recovery formatting, actual
E0302/E0303 failures, file preservation and unavailable damaged-buffer hover all
passed. These results establish local Linux behavior only.

Native three-OS CI is pending the integration PR. No semantic repair, full macro
grammar, source Tier2, performance, RSS or proof claim is made.
