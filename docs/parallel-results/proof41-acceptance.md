# Formatter document proof acceptance (#119)

Owns only new format_proof_cli.rs, format_proof fixtures, check/test-format-proof
scripts, format-proof.yml and this report. The model, actual Doc projection,
Lean AST emission and source/CLI kernel orchestration belong to claims #116–118
and #120 in docs/rfcs/proof41-contracts.md.

The actual CLI tests require success schema vesper.format-proof/1 and explicit
formatter_document_model scope. Source/output hashes must describe current real
bytes; node/comment counts come from the actual captured document. Nested audit
must actually replay comments_preserved and the generic projection lemma, accept
under empty axioms, and never claim source semantic checking or Rust refinement.
Changing comment content changes both current hashes. An ordinary fixed Expr
macro source may receive a document-model proof while ordinary batch checking
still rejects that unexpanded program.

The emitted AST module must compile independently with the actual bundled model
under Lean4.24.0/trust0, with an actual qualified theorem-name check and two
printed empty-axiom results. Text checks allow canonical quoted Lean names;
independent elaboration establishes the real namespace and declaration. Lean-looking comment
and escaped string payloads remain literal: an independent #check injected probe
must fail, rather than introducing an axiom from source contents. No fake Lean
executable or forged version output is used. Invalid/opaque/lexical/UTF8/oversized
inputs, conflicting/unsupported flags, missing actual Lean, directory and Unix
symlink inputs fail without a certificate or source writes. Failed --emit-lean
writes no witness. Existing proof_audit_cli tests separately cover corrupted
external artifacts; verify has no external artifact input and makes no such claim.

The independent Python harness uses only the standard library, runs actual
kernel reports and independently elaborates the witness/model. Both harnesses
prefer an existing .exe, use finite command timeouts and disposable owned files;
Rust caps retained subprocess output and Python checks output size after capture.
VESPER_FORMAT_PROOF_BINARY selects an actual existing CLI. VESPER_LEAN must name
a real pinned toolchain when configured. VESPER_FORMAT_PROOF_MODEL can select the
actual coordinator model for independent read-only worktree acceptance, otherwise
the model comes from this checkout. This is acceptance input, never a certificate.

The symmetric native three-OS workflow installs the repository-pinned real Lean
using existing CI setup, compiles the bundled model, runs formatter/Lean-AST,
new CLI and old kernel guards, strict Clippy and executable acceptance. Exact
package-name dependency checks retain the pure default CLI engine boundary.

Static checkpoint passed: actual `cargo test --locked -p vesper --test
format_proof_cli --no-run`, strict Clippy for that target, Rustfmt, Python
AST/bytecode, shell syntax, workflow YAML/symmetric filters and diff checks.
Private cache only; no fake production module or Lean toolchain was installed.
The Rust suite has five tests on Unix and four on Windows.

Runtime validation is in progress. No runtime or native CI success is claimed until the
coordinator's actual integrated binary/model is ready. The evidence scope remains
the formatter document algebra and runtime conservation checks: no lexer proof,
Rust renderer refinement, semantic program proof, certified autofix, performance
or RSS guarantee.
