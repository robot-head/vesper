# Actual recovery CST reuse and Salsa inputs (#98)

Implemented `vesper_syntax::incremental::SyntaxDocument` over a sealed, AST-free
`Arc<RecoverySyntax>`. The provider does not retain an old AST or expose a World,
CheckedSource, semantic signature, ownership state or proof authority.

Frozen API:

- `SyntaxDocument::open(source:&str, recovery::Limits) -> Result<Self,Error>`.
- `source()->&str`, `snapshot()->&Arc<RecoverySyntax>`,
  `last_update_kind()->UpdateKind`.
- `update(&mut self,source:&str)->Result<UpdateKind,Error>`.
- `UpdateKind::{Unchanged,BodyReparse,FullReparse}`, with stable `as_str()` labels
  `unchanged`, `body_reparse`, `full_reparse`. Initial parsing is FullReparse;
  the host may separately label the initial document as opened.

An unchanged input keeps the exact sealed Arc. Failed updates preserve the old
source, snapshot Arc and last successful update kind. The dispatcher must discard
that association for newer accepted document text rather than publish stale data.
The document retains its original configured limits across successful updates;
individual assembled snapshots can record smaller stage-specific work limits.

Updates derive a common-prefix/suffix difference and widen its boundaries to
valid UTF-8 and CRLF boundaries. The fast path requires an edit wholly inside one
known Complete top-level Function whose private parser context marks it eligible
and nongeneric. Header/context text remains unchanged. Delimiter edits are
conservatively sent to full parsing. Existing diagnostics/sites/receiver ranges
inside the target body also prevent the fast path.

The parser's sealed `capture_body_context` and real-full-source
`parse_body_fragment` helpers validate the old source/header context, unchanged
prefix/suffix, exact token range, lexical safety, ordinary function-body depth and
complete fragment consumption. They do not full-parse the file to claim reuse.
The implementation finds the actual BLOCK green subtree and uses Rowan
`replace_with`, rebuilding ancestors while retaining untouched sibling greens.
Only known syntax metadata is relocated with checked arithmetic; declarations,
site owners/receivers, diagnostics and related spans remain current. No AST spans
are reused. Uncertainty uses the actual full recovery parser; resource failures
are explicit and transactional.

Update work is shared across diff scanning, delimiter/metadata scans, metadata
clones, tree traversal/ancestor staging and owned-source construction. Before a
helper runs, its entire work allowance is debited: capture gets a quarter of the
remaining allowance, fragment parsing a third of the remainder, assembly half of
the remaining allowance. A failed attempt therefore leaves only the remaining
budget for actual full fallback. Other source/token/depth/inventory limits remain
unchanged. This conservative composition can refuse an update even when a
standalone full parse fits the original work cap; it never silently resets work.
These are logical size/staging/work profiles, not RSS or wall-clock guarantees.

`database::SyntaxSnapshot::new(Arc<RecoverySyntax>)` privately wraps the sealed Arc
and compares allocation identity. `RecoveryFile` is a new Salsa input with its
`snapshot` field/setter. `recovery_diagnostics(db,file)` returns
`&Result<Arc<tooling::recovery::RecoveryDiagnostics>,RecoveryFailure>` and consumes
that actual snapshot through the scoped provider. A private tracked identity
boundary backdates identical Arc inputs because Salsa input setters invalidate
unconditionally. The diagnostic query then retains its actual result Arc without
rechecking. Different sealed allocations conservatively rerun, even for identical
bytes. The result wrapper uses bounded pointer identity, not deep CST/World
comparison. Legacy SourceFile/parse/check/guarded/signature queries remain intact.

The host must match exact result source/hash to its current document, clear URI
associations on close and reset database IDs/maps periodically. This module does
not turn Salsa IDs into cross-database capabilities. The coordinator owns those
lifecycle rules, module registration and publication. Whole valid source retains
strict normal semantic analysis; this does not make semantic checking incremental.

Validation used `/tmp/vesper-target-tooling41-doctest`, one Cargo job, zero dev/test
debug information and incremental compilation disabled:

- `cargo test -p vesper-syntax --test incremental --offline`: nine passed.
- `cargo test -p vesper --test database_recovery --offline`: three passed.
- `cargo test -p vesper --test database_signatures --offline`: nine unchanged tests
  passed.
- `cargo clippy -p vesper-syntax --all-targets -p vesper --lib --test database_recovery --offline -- -D warnings`: passed.
- Owned Rust files pass rustfmt and the patch passes `git diff --check`.

Tests compare actual full green-tree structure, all diagnostics, all declaration
metadata/context eligibility/raw body ranges and all sites/owners/receiver ranges
against the real full recovery oracle. Separate pointer comparisons prove actual
untouched ITEM green reuse. Cases cover length changes/repeated edits, damaged
siblings on either side, parameter-sensitive indexing context, Unicode/CRLF,
header/generic/delimiter/comment/string fallbacks, shared failed-fast/fallback work,
ordinary depth limits and failure rollback. Salsa tests observe actual execution
and equality cutoff, independent current sibling linear-leak diagnostics, fresh
hash/ranges, conservative same-text/new-Arc invalidation and stale-source mismatch.
No synthetic source wrapper/padding or host/JIT/WASM/proof execution is used.

The checkpoint used model commit `81d6d80556dfdd45f5894cbdc7a00f98634d0f91` and actual
concurrent parser/scoped sources through temporary private registration. Exact
TESTED source blobs were parser.rs `2a9e9bc160974be4b85528d9ca73e7035798e3e9`,
parser/recovery_driver.rs `855551ab0b69012cfc45ee4646e994d5bccb28df`,
tooling/frontend.rs `fe651c5fc28e42118db8ce9152fa6799c4bc629c`, and
tooling/recovery.rs `890d9ae2c84b50422a17989a42229137d6174cd7`. These identify tested
in-progress dependencies, not a claim that later dependency checkpoints were
already validated. Temporary registration/dependency copies/lock changes are
removed from the owned commit; final dependency integration is coordinator-owned.

Remaining full semantic recovery, PoisonedRecovery/completion, per-function PCG,
macro execution, proof lenses and latency targets remain outside this delivery.
