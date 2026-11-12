# Source-expression hygienic lowering (#106)

This lane lowers a sealed, current-host `ResolvedBody` into actual core `Expr`,
`Statement`, and `Pattern` values, serializes that AST as an actual replacement
Vesper body, and retains the expected AST for the coordinator's full-candidate
shape comparison. It establishes syntax identity and shape, not checked-program
or authority evidence. No existing checker, core/IR schema, CLI, or legacy Forge
API is changed by this lane.

## API and ownership

`forge_frontend::lower::lower_body(&ResolvedBody, &HygieneHost, &SourceBindings,
LowerLimits) -> Result<LoweredBody, LowerError>` checks the sealed host owner,
original source hash, supported source/builtin package records, host token origins,
registry records for actual global references, and resolution inventory. A
same-SessionId different host, changed source, or differently bound source package
is rejected. Host reported spans are never accepted as source authority.

`LoweredBody` has private fields and read-only `source()`, `expected()`,
`segments()`, and `work_used()` accessors. Each `LowerSegment.span` is relative to
the emitted body and has either `SegmentOrigin::Source { span }` (an exact actual
original source-token range) or `Generated { expansion }` (the validated host
ExpansionId). Synthesized punctuation/trivia use their contributing syntax anchor;
these segments describe source provenance, not a byte-identical reconstruction.
The coordinator maps generated identities to its actual invocation/definition
records. Repeated copies of one source argument keep that original range while
each emitted occurrence and each core expression/pattern receives a distinct,
real expanded byte position. Guest `ReportedSpan` is discarded.

All supported local BinderIds are alpha-renamed; the whole body is resolved before
lowering. Fresh names reserve every trusted global/type/macro/builtin spelling and
every raw identifier in the expanded body, including names that resemble generated
names. A let binder enters scope after its own initializer. Source global DefIds
retain the exact `SourceBindings::core_name`; they are never recovered by a bare
name search. Call arguments preserve the ordinary implicit `in` default.

`LoweredBody::shape_matches(actual, limits)` and free `shape_matches` perform a
bounded comparison of supported core AST semantics while ignoring expression and
pattern positions. Names, values, statement order, inferred annotations, actual
call pass/default flags, block tail presence and semicolon/unit behavior remain
significant. The coordinator must still parse/check the entire actual candidate,
compare its actual function bodies, derive authority from the checked candidate,
and lower ordinary IR before publishing a checked expansion.

## Supported profile and limits

The initial profile supports resolved names/qualified global paths, u64-range
integer spellings (including hex/underscores), booleans, JSON-escaped strings,
static global calls with implicit arguments, let-bindings, and blocks. Actual
unit/trailing semicolon behavior is preserved. Negative/unary/float literals,
handlers, higher-order/local calls, type annotations, wildcard patterns, arbitrary
patterns, and other expression forms are explicitly unsupported. The original
source frontend and final semantic/authority profiles may refuse additional cases.
There is one associated source package plus compiler builtins; no cross-package
source-copy or registry-provenance claim is made.

`LowerLimits::validate()` is public and validates unused configurations too.
Defaults (HARD ceilings): nodes 8192 (65536), depth 48 (128), emitted bytes 256KiB
(1MiB), map/name records 32768 (65536), identifier bytes 256 (1024), logical work
16MiB (128MiB). Positive finite limits and checked arithmetic reject overflow.
Shape depth/node volume is checked iteratively before recursive AST construction.
Token/text/scope scans, source hashing, name/identity inventory, AST staging,
string decoding/encoding, provenance emission and comparison consume logical work
before the corresponding output growth or copies. All output is owned and only
returned on success. Failed lowering leaves host and bindings unchanged.

`work_used()` reports the actual charged logical units on success, allowing root
to deduct from ONE shared lower-stage budget across all bodies. Supplying exactly
the reported work succeeds; one less fails. Error aborts the whole compile, so its
spent work is not reset/reused. These logical budgets bound this profile's work
and allocation requests; they do not prove process RSS, execution latency, user
code termination, or hardware isolation.

## Verification

Private temporary registration uses pure Forge (`default-features=false`), actual
syntax and authority implementations; none of that shared wiring is committed by
this lane. Consumed syntax commits: `3b14cdb6` + `338c45ae`; typed Forge:
`58f8051` + `4575337`. Authority binding APIs are the owner's actual implementation (tested production
file blob `bd0c1b7542171a61d413ca4fc01d43848e95711e`; its final dependency commit
is coordinated separately).

`tests/forge_lower.rs` exercises the real syntax parser, typed quotation host,
whole-body resolver, lowerer, normal parser and frontend checker. Positive value
fixtures execute using the existing normal interpreter (no JIT/WASI execution is
implicitly introduced). Coverage includes both capture directions, global
shadowing, own-initializer scope, generated-name collisions, empty/trailing-semicolon
unit blocks, numeric/bool/escaped-Unicode strings, exact effect-operation names,
repeated source argument provenance with actual linear E0303 rejection, distinct
expanded expression positions, foreign host/source/package refusal, limit failures,
shape mutation refusal, retry after failure and exact/shared work boundaries.

Final verification against the actual dependencies above:

- `cargo test -p vesper --test forge_lower --offline`: all 10 tests pass.
- `cargo clippy -p vesper --lib --test forge_lower --offline -- -D warnings`: passes,
  with no suppressions after resyncing the authority owner's lint fix.
- `rustfmt --edition 2024 --check src/forge_frontend/lower.rs tests/forge_lower.rs`:
  passes.

All checks used the private cache, one Cargo job, debug information disabled and
incremental compilation disabled. Shared temporary wiring was restored before
both owned-only commits. No native execution, structured-WASI integration,
residual-row purity proof, authority certification, or live ownership proof is
claimed by this lowering stage.
