# Original-buffer recovery acceptance — issue #99

Branch `recovery41/acceptance`, base `fc75413`. Owned files are the new
`tests/recovery_cli.rs`, `scripts/test-recovery.sh`, `scripts/check-recovery.py`,
`.github/workflows/recovery.yml` and this report. No production, Cargo/lock,
parser/model/scoped/database/server or shared-target writes belong to this lane.
Later external Warp merges remain coordinator-owned and do not change these
acceptance contracts.

## Frozen response and authority checks

The real binary receives Content-Length-framed JSON-RPC over stdin. The new
`vesper/recoveryStatus` request takes `{textDocument:{uri}}` for an open buffer.
Assertions require the coordinator's frozen `scope:"syntax_only"`, URI, current
version, exact byte length, source hash, actual update kind, declaration ranges/
statuses and site ownership/receiver token ranges. The status response must not
echo source or expose World/types/checked/liveness/proof/verified fields.

Declaration and site metadata must remain within current original UTF-8 source;
body/header extents must belong to their declarations. A `tx.` site records the
actual receiver token and zero-width missing-member cursor owned by the damaged
function. It never supplies a resolved place, type or live obligation. Client
`verified`/`checked`/forged fix metadata cannot grant semantic authority.

Publication stays `vesper.status:"parse_error"` for the original incomplete
source, with `vesper.recovery.scope:"independent_functions"`, original-span
analyzed coverage, and damaged-function unavailability. Full semantic hover,
definition, code actions and structural signature search must fail `-32003`
against that original source. A separate clean independent sibling is actually
checked without semantic errors, yet still cannot authorize whole-source hover,
definition, actions or signature search.
The client negotiates both diagnostic publication flags and literal/versioned
actions normally; no missing client capability is used to hide semantic failures.

## Actual source scenarios

The principal Unicode/CRLF fixture declares a linear Tx, an incomplete damaged
function owning tx, and a complete independent leak function owning other. It
requires all of the following:

* The damaged function remains wholly unavailable (`damaged_function`), rather
  than receiving a guessed Unit body or a filtered successful World.
* The complete sibling retains genuine E0302, its original body range, the
  actual parameter's standard relatedInformation location, and invariant help.
* No E0302 for the damaged function is fabricated during recovery, while the
  independent real leak is not suppressed.
* Removing the incomplete expression without consuming either owner restores
  strict whole-program E0302 for both bindings.
* Repairing both owners with real destructuring restores checked publication,
  clears errors, and allows ordinary checked hover again.

Additional scenarios require incomplete call/missing brace synchronization to
preserve the complete sibling and real leak; comments/string literals/opaque
macro payloads cannot mint declarations from embedded fn text; unterminated
lexical tails remain opaque instead of resurrecting embedded siblings.

Actual nongeneric body edits must report `body_reparse`; header changes,
generic/context uncertainty and unterminated lexical edits must report
`full_reparse`; accepted unchanged text reports `unchanged`. Current metadata is
checked against current text, including a real body edit before a damaged
receiver and independent leak: their syntax/causal byte ranges relocate while
the actual reused-body path remains observable. Stale versions and surrogate-interior edits retain
the last accepted source/version/hash/update result exactly. Close/reopen and the
64-accepted-revision cache boundary must not expose old recovery snapshots,
including for another still-open document.

A bounded flat 20,000-semicolon source exceeds the explicit 16,384-token
recovery profile and exercises resource refusal before semantic checking. If its update
fails, recoveryStatus must fail explicitly with current source identity/version,
never return the old successful syntax snapshot. A subsequent real repair must
produce the new declaration and checked source again. This is not an exponential
inference/checker fixture.

The Rust tests calculate exact BLAKE3 for every expected revision. Python uses
independent UTF-8/UTF-16/CRLF conversion, bounded frame decoding and source-hash
shape/cross-channel freshness checks. Its three actual batch `check --json`
commands independently verify original damaged rejection, strict empty-repair
linear failures and successful genuine repair. Neither harness executes source,
JIT, WASM, macros, proofs, formatting or automatic fixes.

## Native workflow and disposable script

The workflow selects Ubuntu, Windows and macOS, tests actual syntax model/parser/
fragments/incremental reuse plus core/span/renderer boundaries, root scoped and
database recovery, actual new stdio acceptance and existing editor/diagnostic
regressions. Strict all-target Clippy is selected for the consumed compiler and
leaf crates. Both push and PR filters cover all `src/**`, syntax/core/span/
diagnostics/LSP/serde/doc/lint/fmt dependencies and the relevant tests/scripts.
Native CI is future observation, not a claim of local three-platform success.

`scripts/test-recovery.sh` builds the locked actual binary by default. Explicit
`VESPER_RECOVERY_BINARY` selects a coordinator-built read-only binary so this
isolated old-base worktree can run actual acceptance without production copies
or root-target writes. Rust uses the same optional override; ordinary CI uses
Cargo's actual `CARGO_BIN_EXE_vesper`. Every binary selector prefers an existing
`.exe` candidate even when MSYS resolves the extensionless alias.

The Python script uses only the standard library and an owned disposable child
directory; caller work directories are preserved. Stdio/check subprocesses have
45-second timeouts. Captured output and individual frames are bounded and output
JSON duplicate keys are rejected. No recovery example is currently promised by
the model/parser owners; the script exercises the actual end-to-end protocol.

## Checkpoint validation

Private target `/tmp/tooling41-lint-target`, one Cargo job, dev/test debug0 and
incremental off. Static checks passed at this first checkpoint:

```text
cargo test -p vesper --test recovery_cli --no-run
cargo clippy -p vesper --test recovery_cli -- -D warnings
rustfmt --edition 2024 --check tests/recovery_cli.rs
bash -n scripts/test-recovery.sh
Python source compilation and workflow YAML parsing
git diff --check
```

The first checkpoint (`273ec08`) intentionally recorded runtime pending. Final
runtime validation then passed against the new coordinator-built read-only
binary `/workspace/vesper/target/parallel-next/debug/vesper` (reported integrated
tree `4b1fcfa`, including final model/parser/scoped/incremental implementations
and combined incoming main). No production copies or root cache writes were used:

```text
VESPER_RECOVERY_BINARY=ROOT_READONLY_BINARY scripts/test-recovery.sh
  passed: 15 actual framed stdio sessions and 3 actual batch checks
VESPER_RECOVERY_BINARY=ROOT_READONLY_BINARY cargo test -p vesper --test recovery_cli
  all 7 Rust tests passed using the private Cargo target
cargo clippy -p vesper --test recovery_cli -- -D warnings
  passed
```

The final follow-up uses the explicit 16,384-token limit to make resource refusal
unambiguous and adds actual body-reparse relocation before later damaged syntax/
causes. All four semantic feature routes are also rejected for a zero-error
independent subset. No original expectation was weakened to accept missing
metadata, false checked status, full parsing labeled as a body path, or stale
snapshots. No old minimal binary was accepted as recovery evidence.

This is syntax-only recovery plus scoped diagnostics and bounded CST reuse. It
does not certify a recovered whole program or implement semantic PoisonedRecovery,
ownership/liveness completion, per-function PCG, proof lenses, arbitrary macro
execution, automatic fixes, hard RSS/deadlines or a sub-2ms guarantee.
