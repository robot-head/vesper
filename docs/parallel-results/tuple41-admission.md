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
generic projections explicitly refuse. Existing programs without nonempty tuple
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
- Strict Clippy, focused existing frontend unit tests and formatting checks are
  pending in this initial report.
