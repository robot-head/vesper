# Stylus document-algebra proof model — issue #116

Added `lean/StylusAndLintSafety.lean` with the exact frozen
`Vesper.StylusSafety.Doc` constructors and public `render`, `commentsOf`,
`comments_render_invariant` and projection-disjointness theorem names. The
ordered comment result contains complete string payloads, retaining repeated
occurrences; it is not a count-only witness. The model has no external imports.

The Doc algebra covers nil, text, comment, hard line, both soft line variants,
nest, list concat and group. Rendering produces tagged text/comment/indented-line
pieces and threads the actual column and a group ordinal through nested
concatenations. Column advancement handles Unicode scalars and embedded LF;
flat width rejects LF, CR and line comments as in the Rust arena algebra.
Natural numbers model successful checked arithmetic, without a machine-overflow
or resource theorem.

The stronger `comments_renderWith` theorem proves exact ordered payload
conservation for every ordinal-indexed group-choice function, inherited mode,
width, column and indentation. `comments_renderWith_invariant` compares two
different schedules and modes. The frozen `comments_render_invariant` follows
for the representative local-width chooser. This permits arbitrary layout
choices while keeping the proof independent of any particular width heuristic.

The local chooser intentionally abstracts Rust's bounded suffix lookahead.
This is a document-algebra theorem instantiated by the actual Doc projection,
not a proof that the Rust implementation refines Lean, a lexer/comment-token
classification proof, source semantics or a certified compiler/autofix theorem.
Comment-looking text leaves remain text; the source bridge separately checks
actual lexer comment payloads against projected Comment leaves.

`projection_narrowing_preserves_disjointness` proves the design's generic
subset/disjointness implication. Its subset premise is explicit. The
`narrowing_requires_subset` counterexample proves that deleting that premise
would be unsound; this does not establish that a production edit meets it.
`payload_order_matters` also rejects a reordered pair of equally many comments.

Actual Lean 4.24.0 checked the module and produced an `.olean` file. Closed
examples cover flat/broken whitespace, nesting, Unicode columns, embedded LF/CR,
group width boundaries, line comments, comment-looking Text and duplicate
payloads. Printed axiom dependencies are empty for both required theorems,
the stronger schedule theorems, helper conservation lemmas and both
counterexamples. No sorry, custom axioms, partial definitions or native_decide
are used. Explicit structural termination keeps mutual Doc/List definitions
and proofs total.

Lean's optimized String.contains/startsWith/splitOn carry propext in this pinned
toolchain. Equivalent primitive scalar-list helpers avoid importing that axiom
into the theorem dependencies. Exact source text and order remain unchanged by
this implementation choice; all required dependency lists print empty.

Verification used `/home/agent/.elan/bin/lean -o /tmp/Proof41StylusAndLintSafety.olean
lean/StylusAndLintSafety.lean`, plus `git diff --check`. No toolchain install,
Cargo build, coordinator-cache write or existing proof edit was needed.
The emitter/root owners perform independent actual witness elaboration and
kernel artifact replay under the empty axiom policy.
