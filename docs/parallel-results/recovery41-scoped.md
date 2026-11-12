# Issue #97: scoped diagnostic-only recovery

Implemented from `fc75413` on `recovery41/scoped` after reading issue #97,
`docs/rfcs/recovery41-contracts.md`, the parser/model contracts and current
frontend guards. Owned source is `src/tooling/recovery.rs`, guard reuse in
`src/tooling/frontend.rs`, and `tests/recovery_analysis.rs`. The coordinator
registers `pub mod recovery;` in tooling; syntax/root module wiring and temporary
model/parser/Cargo/lock copies are restored before owned commits.

## Authority and API

`analyze(&RecoverySyntax, RecoveryLimits) -> Result<RecoveryDiagnostics,
RecoveryFailure>` consumes the actual sealed AST-free syntax snapshot. The private
immutable result exposes `source()`, `source_hash()`, `diagnostics()`,
`analyzed_functions()` and `unavailable()`. It has Debug, no deep Clone, no World,
CheckedSource, certificate, checked boolean, types or liveness. Coverage records
contain original span/name; unavailable records contain original span and an enum
with stable snake_case `as_str()` reason. A function in analyzed coverage may have
real errors; coverage never means semantic success.

Source/hash association is checked before publication. Syntax-complete input is
explicitly refused with `syntax_complete_use_check_source`; repairing source uses
the existing full guarded checker and restores strict full-program diagnostics.
For damaged input, original syntax diagnostics stay on RecoverySyntax;
`RecoveryDiagnostics::diagnostics()` contains only actual normal-checker scoped
semantic diagnostics. The publication budget covers syntax and semantic slices
together without cloning them first. No E030x diagnostic is filtered.

Complete ASTs come only from `parser::parse_complete_items(snapshot, indices,
syntax_limits)`. Indices are derived from the sealed actual declaration inventory,
not caller-created flags/ranges. Each item must match its actual descriptor kind,
name and absolute original span. No failed-body AST, padding, wrapper, placeholder,
guessed callee signature or invented source text is introduced. Component checks
clone actual original context items and one eligible actual function only after
the guards. Their temporary checked Worlds are dropped and never exposed.

## Conservative closed components

Recovered functions are unavailable in their entirety. Any opaque declaration,
unowned recovery site, lexical/macro opaque site, non-function damaged context,
unknown header or original nontrivia token gap blocks components conservatively.
Unknown coverage gets explicit unavailable ranges, including otherwise unowned
opaque source. All header identities, reserved names and constructor/member names
must be unique. Imports/use-models/traits/models/impls and ambiguous declarations
are unavailable, never guessed missing-name diagnostics.

The initial context supports complete nongeneric acyclic user ADTs and nongeneric
effects with closed, body-free, non-effectful operation signatures. Unknown field
or operation types, recursive/generic/ambiguous contexts are unavailable. The real
normal checker first validates eligible context once; genuine context errors are
published once and dependent functions become `invalid_context`. This avoids
rechecking functions under invalid context or duplicating its diagnostics.

Eligible functions are nongeneric, body-complete and have closed known types and
effects. Lexical scopes, shadowing and pattern bindings are inspected before normal
checking. Ordinary global-function dependencies, indirect/function-value calls,
closures, handlers, try/residual contexts, higher-order/row/unresolved header types
and generic arguments are unavailable. Calls are restricted to known unique user
constructors and actual known effect operations. This initial profile also
conservatively excludes intrinsic calls such as `wrapping_add`; recognizing
reserved intrinsic names does not authorize a guessed call contract. Builtin
primitive types/unit and concrete builtin error types are known; a closed Fail row
can be declared, while unsupported builtin/generic constructor or operation calls
remain unavailable. Existing frontend aggregate-construction/inference guards also
apply. This is a documented subset of normal checking, not compiler recovery.

## Resource policy and shared guards

`RecoveryLimits` adds no AnalysisLimits fields. It has `analysis` (existing validated
frontend defaults), `max_functions` default64/hard128, `max_context_items`
default64/hard128 and `max_work` default4MiB/hard32MiB. Configurations and actual
source length are checked before source hashing or cloning. The declaration total
is at most function+context limits. Defaults inherit source128KiB, AST/type2048,
depth48, identifier256B, printable type2KiB, diagnostics64/64KiB and AST/type
metadata512KiB. There is an additional 16,384-token scoped ceiling.

Half the work allowance is reserved for the parser's own actual full-source
lex/range/parse budget; the remainder counts source hashing bytes, CST/inventory
visits, serialized global/context/component AST bytes, nominal expansion visits,
pattern/expression work, initial parameter scope entry bytes, every cloned scope
entry and pattern insertion. The normal-check stage is prepaid with a fixed
logical weight of four times observed profile-walk work times bounded nominal
expansion cost; context-only checking is charged separately. Overflow/exhaustion
refuses before cloning/checking the affected component. These are explicit logical
resource charges and bounded source/component profiles, not CPU instruction,
wall-clock, allocator failure, RSS or general checker-termination proofs.

Nominal repeated siblings/DAG visits consume the shared type/work budgets; cycles
are unavailable before World mode expansion. Before owned AST cloning, recovery
reuses the same immutable frontend AST/depth/inference and serialized metadata
checks. The frontend implementation is factored over borrowed item references;
normal `check_source` keeps its original profile. Aggregate diagnostic validation
reuses the rich primary/related/help record/text/span gate across borrowed slices.
Resource exhaustion returns explicit `RecoveryFailure::Resource` rather than
partial or falsely checked diagnostics. Legacy doc/lint independent profiles,
source grammar, checker transfer rules, IR and normal API behavior remain unchanged.

## Validation and integration

Private dependency checks used model `81d6d805` plus SelfKw followup `2a287b27`,
parser checkpoint `406cd0a`, and final parser followup `dbba3a6` for final compilation.
The original helper APIs and frozen scoped API remain unchanged. All temporary
syntax model/driver/parser/Cargo/lib/root-tooling/lock files are restored before
owned commits. No registry versions were changed (the temporary lock difference
was only the already-approved syntax blake3 edge).

Private cache `/tmp/vesper-tooling41-doc-target`, one job, debug0, incremental off:

```sh
cargo test -p vesper --locked --offline --test recovery_analysis
cargo test -p vesper --locked --offline --lib tooling::
cargo clippy -p vesper --locked --offline --all-targets -- -D warnings
rustfmt --edition 2024 --check src/tooling/frontend.rs src/tooling/recovery.rs tests/recovery_analysis.rs
```

Eight scoped integration tests pass: Unicode/CRLF damaged-linear plus independent
real E0302 and real causal parameter span; original repair restoring both/one/no
strict linear errors; actual moved-use errors and lexical shadowing; unavailable
global/unknown/generic/higher-order contexts; cyclic/ambiguous/incomplete context;
opaque string/comment/unknown-header spoofing; real invalid-context diagnostics
once; direct and aggregate resource refusal. Eighteen tooling unit tests pass,
including two new private tests for scope clone/initial collection/pattern
insertion charges before work and overflow refusal. Strict all-target Clippy and
formatting pass. The coordinator additionally reported combined scoped/database/
stdio/source tests passing after integrating the checkpoint.

The batch provides diagnostic-only independent components. Semantic
PoisonedRecovery, recovered typing/hover/actions/search/signatures, per-function
PCG, generic/imported dependency closure, macro/proof execution and latency
promises remain outside this implementation.
