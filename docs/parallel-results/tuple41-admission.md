# Tuple41 bounded checker admission (#172)

Claim and contract were read before writes. Owned changes are limited to the
frontend admission integration, private `frontend/tuple_growth.rs`, actual
`tuple_analysis` tests and this report. Core checker/AST/IR/native code, authority
policy, limit fields/caps, workflow/build/test wrappers and CLI registration are
unchanged.

Dependency-only cherry-picks: original lowerer commits `2b80a43` and `78eae54`
are `69089f4` and `c0aa392` in this worktree. They are not admission-owned work.

The existing structural/generic/nominal/closure gates run first. A module with
nonempty tuple construction then receives a numeric size/depth/metadata envelope
before the actual checker. The guard never constructs semantic Type trees,
clones a World or manufactures a checked artifact. Borrowed lexical bindings and
Copy bounds handle parameters, initializer-before-shadowing, blocks, closed
function results/function aliases, tuples, destructuring, fields, assignments,
sequential inout calls and control-flow joins. Function-value bounds include
parameter, result and effect argument types. Ill-typed wrapping calls retain an
argument upper bound. Joins sum bounds rather than assuming equal valid types.

The guard checks vector storage before reserve_exact; tracks environment-copy
and retained expression metadata envelopes; caps type size/depth with existing
limits; and bounds recursive visits and borrowed scans. Unknown calls,
higher-order/latent arrow refinement, handlers, dynamic indexing and unsafe
generic projections explicitly refuse. Trait/conformance elaboration and
implicit inherent self replacement also refuse because their generated headers
require substitution outside this envelope. Referenced duplicate functions or
effect operations refuse, including semantically invalid declarations whose
last checker header differs from an earlier declaration. Existing programs without nonempty tuple
construction retain their previous admission behavior. This conservative subset
can refuse larger or uncertain tuple programs even when the ordinary checker
would accept them. Post-check type/metadata guards remain in place.

Validation (real cache `/tmp/tooling41-lint-target`, one Cargo job, debug0,
incremental0):

- `cargo test --test tuple_analysis --test forge_tuples --quiet`: 9 admission
  integration tests plus all 11 real source/lowering/authority tuple tests pass.
- The original 25-step doubling regression now asserts a Resource refusal with
  an explicit before-checking reason; a direct private AST admission assertion
  verifies it without checker entry.
- New tests compare real tuple error diagnostics byte-for-byte with the unchanged
  ordinary checker, and verify real source/hash/AST/type/World identity.
- `cargo test -p vesper --lib tooling::frontend::tests --quiet`: all 5 existing
  frontend unit tests pass.
- `cargo clippy -p vesper --lib --test tuple_analysis --test forge_tuples -- -D
  warnings`: passes after fixing three style-only collapsible-if findings.
- Follow-up hardening and regressions for substituted/duplicate headers, inferred
  depth from flat source, and latent-effect function values retain all 9+11
  passing actual tuple integration tests.
- `cargo test -p vesper --test recovery_analysis --quiet`: all 8 existing
  recovery tests pass.
- Final strict targeted Clippy, `cargo fmt --all -- --check` and
  `git diff --check` pass. No checks were bypassed or limits raised.


Independent review follow-up (2026-11-13):

- Reject duplicate ADT names and builtin ADT overrides before resolving fields.
  Reject duplicate variant/field inventories as well. The real World retains
  the last ADT declaration; its field lookup uses the first matching field.
  No earlier source declaration can now supply a smaller projection envelope.
- Validate nominal type-argument arity before expression admission. This closes
  the nullary-constructor path that clones its entire expected nominal type even
  after malformed-arity diagnostics. Malformed integer/float/String type argument
  contexts also refuse. Actual integer literal inference returns `kind.ty()`;
  string/bool literal inference returns a fixed scalar type. A long UTF-8 string
  tuple remains accepted through the real checker.
- Prepay two environment snapshots during short-circuit RHS traversal for both
  `&&` and `||`, including dead RHSs. Small metadata tests isolate this cost:
  both the same boolean expression without the wide parameter and the same wide
  parameter without nested short-circuiting remain accepted under that limit.
- Constructor matching now follows actual nominal prefix/final-segment matching
  and checks constructor values before assuming a scalar envelope. The current
  parser separately rejects multiple `::` segments; this is recorded as a parse
  boundary, not claimed as an additional reachable checker bug. Actual ordinary
  `Mark::Ready` tuple construction remains accepted.

Final follow-up validation: all 12 admission integration tests, all 11 actual
lowering/authority tuple tests and all 8 recovery tests pass. Strict targeted
Clippy, all 5 existing frontend unit tests, workspace formatting and diff checks
pass. No core checker or limits changed.

Closed-header review follow-up: `ty` explicitly refuses `Type::Infer` and the
current parser's `Type::Named("_", [])` source placeholder in analyzed parameters, returns, nominal arguments, fields and partial let annotations.
This is a conservative clarification of the closed source profile, not an
additional demonstrated resource exploit: the actual parser retains explicit
`_` as `Named("_", [])`, and the current non-function `refine_annotation` fallback
keeps the expected type rather than filling that placeholder from the body.
`Type::IntLiteral` remains separately bounded.
Unannotated lets still use their actual initializer envelope; tuple and function
alias initializer inference remain supported. New small-limit tests cover
inferred-result function values and nested header/annotation holes. A direct
private admission assertion uses the real parser without entering the checker.
Closed-profile clarification validation: 13 admission integration tests, 11 tuple
lowering/authority tests, 8 recovery tests and 6 frontend unit tests pass. Strict
targeted Clippy, workspace formatting and diff checks pass.
