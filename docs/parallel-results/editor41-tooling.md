# Issue #81: pure checked-source tooling for editor consumers

Implemented from `593bbc311a1c0693aa08732b40cec813e2cda436` on
`editor41/tooling` after reading issue #81, `docs/rfcs/editor41-contracts.md`,
refreshed specification §21 and current source adapters. This is a bounded
source-tooling host slice, not the full §21 incremental/PCG/recovery design.

## Library and compatibility boundary

`src/tooling/{frontend,doc,lint}.rs` contains pure source-only tooling in the root
compiler library. Dependency direction remains root → leaf; the root-free doc
and lint crates acquire no root dependency. `src/lib.rs` adds only `pub mod tooling`.
There are no Cargo/dependency/lock/schema/core/checker changes. Existing
`doc_commands::document_source` and lint public helper/DTO names remain reexports.
CLI help/JSON/default-policy contracts and legacy source profiles remain intact:
128 KiB documentation source and 256 KiB lint source, with their original tested
guards. Filesystem source/config reads, explicit policy loading, staging guard
ownership, stale-source checks and atomic replacement remain in the binary
adapters. Pure APIs perform no filesystem, macro, JIT, WASM, build or proof I/O.

`tooling::frontend::check_source(&str, AnalysisLimits)` returns only an actual
`CheckedSource` or `AnalysisFailure`. The snapshot has private construction,
Debug, no deep Clone, an owned `Arc<str>`, the actual parser result/checker result,
and actual BLAKE3 bytes. Immutable accessors are `source() -> &str`,
`parsed() -> &parser::Parse`, `checked() -> &check::Checked`,
`world() -> &check::World`, `source_hash() -> [u8;32]`. Consumers can retain
`Arc<CheckedSource>`; arbitrary World/boolean/client metadata cannot mint one.
The successful snapshot never contains parse/semantic diagnostics, nor a
post-validation resource failure. `AnalysisFailure` is Clone/Debug/Eq with
`Parse { diagnostics }`, `Semantic { diagnostics }`, `Resource { reason }`,
`Unsupported { reason }` and `.diagnostics()`. Resource/unsupported outcomes carry
no unchecked World. Parse/check diagnostics preserve actual code/message/span,
subject to count/byte/valid-source-span checks before being returned.

## Explicit new editor profile

`AnalysisLimits` is Copy/Clone/Debug/Eq/Hash and validates direct configurations.
Its fields and default/hard ceilings are:

| Field | Default | Hard ceiling |
| --- | ---: | ---: |
| max_source_bytes | 131072 | 262144 |
| max_ast_nodes | 2048 | 8192 |
| max_type_nodes | 2048 | 32768 |
| max_depth | 48 | 64 |
| max_identifier_bytes | 256 | 1024 |
| max_printed_type_bytes | 2048 | 8192 |
| max_diagnostics | 64 | 256 |
| max_diagnostic_bytes | 65536 | 262144 |
| max_metadata_bytes | 524288 | 2097152 |

Zero/oversize configurations fail before parsing. These are logical conservative
profiles, not wall-clock, allocation-failure, RSS or checker-termination proofs.
The new profile preserves lexical weighted-subtree, iterative AST,
recursive-nominal/DAG and inferred tuple/generic/closure growth guards **before**
checker traversal/cloning. User nominal cycles and unsupported inferred composite
construction fail explicitly. The new generic-value guard includes short and
qualified generic trait/conformance method names as well as free/inherent methods;
method dispatch cannot bypass the same generic inferred-growth refusal. Source lexing/parsing also uses bounded production
parser APIs, 16384 tokens and diagnostic/depth limits. Function parameter/generic
lists and effect-row instances have a fixed 64-entry editor ceiling. AST counts
include empty variants, generic/effect metadata, model-use/trait-bound entries and
selector paths, so flat zero-argument metadata cannot evade node accounting.
Identifiers are bounded before checking. Explicit types have bounded formatting;
AST metadata is counted by a nonallocating serialized-byte writer before checking.
Actual inferred expression/World/result/call/handler type structures are iteratively
node/depth validated before post-check formatting/serialization, with aggregate
metadata bytes also checked. Failures never publish a positive checked snapshot.

The checker decides actual semantics: textual `->_` currently names an unknown
source type and is rejected; the library does not promote it. Bounded complete
arrow inference is supported. The same outer header returning `fn()->i64 eff[Ask]`
with `{|| Ask::ask()}` versus `{|| 0}` produces different actual checked latent
results while retaining identical declared result types. Direct regression tests
retain this distinction for signature consumers. No blind body/span stripping is
used by this lane.

`tooling::doc::document_checked(&CheckedSource, vesper_doc::Limits)` invokes the
same actual World/source metadata builder as the legacy wrapper, without parsing
or rechecking. It preserves inferred full results, effect instances, generated
private helper exclusion, modes/selectors, native eight-byte layout descriptions
and all existing unverified/unsupported proof/search/layout limitations. The
original `document_source` retains its original independent CLI guard profile.

## Selected fixes

Pure `tooling::lint` retains `prepare_source`, `prepare_source_with_policy`,
`SourceReport`, `PreparedLint` and policy/provenance DTOs, reexporting the leaf
policy types. The new entry point is:

```rust
prepare_source_selected_with_policy(
    source: &str,
    selected: ast::Span,
    semver_safe: bool,
    policy: &Policy,
    provenance: ConfigProvenance,
) -> Result<PreparedLint, String>
```

It validates source-range ordering/bounds/UTF-8 boundaries, checks the source with
the explicit editor profile, then recomputes complete checked lint evidence.
Client diagnostics/fix payloads are never inputs. Policy allow suppression precedes
selection. Nonempty selection intersects freshly token-derived finding spans;
a caret uses `start <= caret < end` (zero spans match only exactly). Checked
source/token mapping runs first; actual edit normalization runs only on selected
allowed effect indices. The report retains all fresh policy-visible findings,
including unrelated findings; candidate edits cannot target unrelated functions.
Full-candidate reparse/check/IR lint rerun, no-new-finding/no-new-unsupported checks,
and requested actual supported-interface SemVer comparison remain mandatory.
Selected-effect multiset counts use semantic effect identity across shifted row
indices, so an unrelated remaining effect is not falsely treated as a new finding.
Explicit full-document fix-all remains the existing separate prepare API.

`ConfigProvenance` remains `Default` or `Explicit { path, bytes, blake3 }` as before;
paths are descriptive caller metadata, never filesystem authority in this library.
`PreparedLint` retains public `report` and optional fully validated `candidate`;
caller ownership/source/version checks remain the editor/CLI adapter's job.
No returned candidate is applied to a file by these APIs.

## Verification

Private cache `/tmp/vesper-tooling41-doc-target`, one Cargo job, dev/test debug0,
incremental off, baseline lockfile unchanged. After initial local cache compilation,
checks use `--locked --offline`. Commands:

```sh
cargo test -p vesper --locked --offline --lib tooling::
cargo test -p vesper --locked --offline --lib --test doc_cli --test lint_cli --test interface --test language --test syntax_and_ir
cargo test -p vesper-semver --locked --offline
cargo clippy -p vesper --locked --offline --all-targets -- -D warnings
cargo fmt --all --check
```

Eleven direct pure-tooling tests cover exact source identity, immutable actual
AST/World linkage, document_checked equivalence, structured parse/semantic failure,
source/AST/type/diagnostic/identifier/printed-type/metadata budgets, compact nominal
DAG/cycle, 25-level inferred tuple and generic trait method rejection, zero-argument
row/model-use metadata accounting, actual latent inference, range/
caret/UTF-8 behavior, policy suppression, unchanged unrelated functions, actual
candidate checking and SemVer failure/success. Existing three pure lint unit tests
migrate with their private implementations; four filesystem staging tests remain
in the binary adapter. Unchanged verification passes include rootlib16, doc_cli11,
lint_cli29, interface13, language107 and syntax_and_ir4 (180 total), preserving
actual ownership/effect/PCG and interface checks. Strict all-target Clippy and
formatting pass. The separate SemVer crate suite also passes (26 unit +9 release tests). Matching CLI integration source files required no changes.
