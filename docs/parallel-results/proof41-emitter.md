# Structured formatter proof emission — issue #118

The emitter consumes the sealed actual formatter DocumentModel and builds a
`vesper-lean-syntax` Module from typed terms and declarations. It imports only
the fixed StylusAndLintSafety module, defines the complete projected document,
retains the source/output hashes as string metadata, and instantiates the fixed
`comments_render_invariant` theorem with Nat width/column binders. No Lean code
template, raw syntax node, caller theorem/import name, sorry or dynamic tactic is
used. All Text/Comment payloads enter through Term::String and the central printer;
concat children use the new structured Term::List node.

Emission first traverses the actual private projection read-only. It verifies
expanded node/text/depth/comment statistics, charges scans before payload work,
checks tighter caller caps, and computes conservative AST storage, escaped output
and recursive-printer envelopes before any payload clone or AST construction.
The fixed syntax envelope is 4096 bytes plus at most128 constructor bytes per
projected occurrence and the exact central-printer quoted payload lengths.
Metadata includes both owned AST storage and the escaped printer temporary
envelope, bounded by rendered-envelope times AST-depth-plus-eight. The work
envelope includes those allocations and recursive printing. Actual returned
Lean bytes are checked against both the preflight envelope and configured cap.
Maxima need not all fit together; larger valid documents can be refused. These
are logical resource guards, not a measured RSS or CPU bound.

Read-only review caught an initial metadata gap:16KiB of U+0001 has an escaped
printer temporary larger than a64KiB metadata cap even though its raw payload
fits. The final preflight accounts for that temporary before construction and a
regression requires rejection. No cap semantics were weakened.

The public source association comes only from root's private FormatVerification;
hash definitions are metadata, not a semantic theorem about those source bytes.
The proved property concerns the instantiated document algebra. Runtime lexer
token/comment checks provide the separate source association. This emission does
not establish Rust-renderer refinement, program type/linearity correctness or a
certified autofix. Artifact kernel replay with the empty axiom policy belongs to
the root verification command and acceptance lane.

## Actual validation

Using actual projection d069742, AST/list+manifest3287fa2, source capture675b70,
and checked model73a187d plus39323c0:

- Seven emitter integration tests passed, zero ignored. They check actual source
  and formatted hashes, ordered macro comments without expansion, payload escaping,
  damaged/unknown source rejection, semantic-scope boundaries, each tightened
  emission cap, and the escaped-temporary metadata regression.
- With VESPER_LEAN=/home/agent/.elan/bin/lean, the real Lean4.24.0 toolchain compiled
  the static model and three actual generated modules (ordinary, hostile
  quote/backslash/newline/U+0001/U+2028/Unicode comments, and a source macro).
  Three independently corrupted theorem references all failed elaboration.
- The combined formatter suite passed48tests in7suites, zero ignored, including
  all three private-construction compile-fail cases. Strict all-target formatter
  Clippy, owned rustfmt and diff checks pass on the installed Rust1.99.1.

Pure AST/budget tests always run. The real elaboration test explicitly reports
pure-only scope when VESPER_LEAN is absent; when configured it must run the actual
4.24.0 toolchain and fail closed on any failure. Proof/workspace CI configures it,
and the explicit verification CLI always requires a real toolchain. No fake Lean
executables or ignored tests were introduced.

All builds used the existing private /tmp/stylus41-macro-target cache, one job,
debug0/incremental0. The coordinator cache was not written. Owned commits are
25ad710 (initial emitter),362942a (temporary bounds and tests), and this report;
other lane checkpoints were dependencies only. No unrelated production paths
changed.
