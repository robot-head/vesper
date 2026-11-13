# Parameter-aware source compiler — claim #161

This lane owns the parameter lowering, header-token rendering and narrow private
expanded-header authority association in forge_frontend, plus real compiler
regressions. It follows params41-contracts.md and its source-bound SelfKw
clarification. Syntax and typed-host implementations were imported as actual
committed dependencies; no substitute API or fabricated checked artifact was used.

LoweredBody retains a source-bound FunctionId and private-construction
ParameterRename records. The builder reserves existing spellings, derives fresh
names from actual BinderIds, and seeds its active/seen inventory with the sealed
parameter binders. Its final active inventory must equal that exact parameter
set. The SelfKw exception applies only to an actual sealed header parameter;
ordinary body-local keyword binder rules remain unchanged.

Header identifier replacements and body references share those fresh names.
Sorted edits include actual header tokens, body replacements and macro removals;
counts, sizes, nonoverlap, output capacity, source-map storage and work are
preflighted. Header comments, whitespace and non-name semantic tokens remain
original bytes. Header renames are Source provenance anchors, consistent with
existing alpha-renamed locals rather than a lexeme-equality promise.

Public authority::derive retains strict original header equality and its API.
A private compiler-only path consumes the real MacroSource and actual sealed
lowering records. It checks source/function identity, complete parameter index,
name/span coverage and original source association, then constructs a borrowed
expected header with only the intended parameter names replaced. Function name,
generics, order, pass, type, projections, result and effects remain exact. No
public override, untrusted JSON patch or deep World/header clone is introduced.
The full expanded checker, identity-body comparison, actual body authority graph,
macro/caller gate and normal IR validation remain success requirements.

Local Linux validation on Rust1.99.1 passed:

- Actual root library plus new source compiler tests:43+8 tests, zero ignored.
  The private header test first checks real candidates, then rejects altered
  parameter name, pass, type, result, generics and effect rows through the new
  private association path. Integration cases cover global-name hijacking,
  capture/initializer scopes, in/inout/sink, self, comments, real linear
  consumption/E0303 duplication/E0302 leakage, unsupported/duplicate headers,
  current helper-body authority and strict public derivation.
- Existing authority/lowering regressions:9+10 tests, zero ignored. Total70tests
  in4suites pass without filters.
- Strict targeted Clippy for root library and new/old compiler tests, workspace
  formatting and whitespace checks passed. A test-only op_ref warning was fixed
  without changing the provenance assertion.
- Exact expanded byte capacity succeeds and one byte below refuses, including
  when original macro source is longer than its compacted checked expansion.
  Source/map/work refusals return no accepted artifact.

Actual CLI execution, editor preview, formatting acceptance and native hosted
coverage belong to root/#162 and external Bazel integration #158. This lane did
not edit workflows, build files or scripts/test-*.sh and does not claim remote
native execution, latency/RSS guarantees, source Tier2, generic/projected macro
parameters, or completion of the broader design.
