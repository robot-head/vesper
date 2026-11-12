# #87 compatible causal diagnostic metadata

Delivered on `diagnostics41/model`, fresh worktree from
`b4d1df8846d29afac7133774d00e39362b66deff`. Read issue #87, the diagnostics41
contract and specification §21.4. Exclusive writes are the diagnostic definitions
in core ast.rs, the new core diagnostics tests, this report, and the coordinator-
authorized narrow serde_json dev-dependency addition to the core manifest.

Frozen public API in `vesper_core::ast`:

```rust
pub struct DiagnosticLabel { pub span: Span, pub message: String }
// Diagnostic existing fields remain code:String, message:String, span:Span.
pub related: Vec<DiagnosticLabel>
pub help: Option<String>
Diagnostic::new(code: &str, message: impl Into<String>, span: Span) -> Self
Diagnostic::with_related(self, span: Span, message: impl Into<String>) -> Self
Diagnostic::with_help(self, message: impl Into<String>) -> Self
```

`DiagnosticLabel` derives Debug/Clone/PartialEq/Eq/Serialize/Deserialize, matching
the diagnostic record. `new` initializes empty metadata. The related builder
appends in supplied order; the help builder replaces only help. Serde defaults
missing related/help, skips empty related and None help, and preserves an
explicitly supplied empty help string. Unchanged legacy diagnostics re-emit the
exact old three-field JSON order/shape/bytes; codes/messages/primary spans stay
unchanged. Rich round trips retain exact UTF-8 byte ranges, Unicode messages,
ordered labels and help. Malformed nonoptional related data is rejected rather
than silently discarded.

This is supplied metadata, not source-aware validation, proof, edit authority or
semantic causality. Producers supply genuine event evidence; the separately
owned frontend/renderer validate bounds and source coordinates. No core type,
AST expression, checker acceptance/rejection, IR/interface/native ABI, arithmetic
or Lean extraction changes are introduced. Existing code using `Diagnostic::new`
keeps the same signature; external struct literals must initialize the two new
fields as part of this additive Rust API change.

Validation in reused private `/tmp/vesper-target-editor41-span`, one job,
dev/test debug=0 and incremental=0:

- Actual core test run: 3 existing unit tests and 7 new integration tests PASS;
  doc suite empty. Coverage: exact legacy JSON decode/encode, explicit empty
  normalization, rich Unicode/CRLF byte-span/order round trips, builders,
  independent optional fields, cloning/equality and malformed metadata.
- Strict `cargo clippy --all-targets -- -D warnings`: PASS.
- `rustfmt --edition 2024 --check` for both owned Rust files: PASS.
- `git diff --check`: PASS.

Workspace `--locked` initially refused the new existing serde_json dev-dependency
edge in the core lock entry. No root lock was written. For the actual tests and
Clippy, the core manifest was temporarily made standalone using the same cached
serde1/serde_json1 packages (resolved serde1.0.229/serde_json1.0.154), then restored
to workspace inheritance before commit; the temporary leaf lock was removed.
The committed core manifest only adds `[dev-dependencies] serde_json = {
workspace = true }`. Coordinator updates the existing root lock dependency edge
and reruns normal locked workspace checks. No new registry package/version was
requested or added, and no root/global production files were modified.
