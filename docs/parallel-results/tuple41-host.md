# Typed Expr unit and tuple host support

Claim #168 owns typed_quote.rs, the new typed_tuple tests and this report under
docs/rfcs/tuple41-contracts.md. No public Term/ResolvedFragment wire structure,
legacy flat parser, parameter record, core AST/IR/native ABI, guest transport,
workflow/build file or test wrapper was changed.

The sealed typed parser now retains actual unit, singleton and multiple tuple
values as ExprShape::Tuple with original delimiter token indices and ordered
values. Unit has no values; grouping retains its original child shape. Nested
tuples and optional tuple trailing commas work, while malformed punctuation and
the existing ordinary-call trailing-comma restriction still refuse.

Every recursive visitor includes tuple children: owned shape conversion,
canonical actual/template structure, definition-site global pre-resolution,
lexical term traversal, height, binder candidates and resource work. Structural
tags13/14/15 add disjoint tuple start, per-element and end fences; existing
tags1–12 remain unchanged. Current host/source/slot association and parenthesis
wrapping keep a tuple-valued input one expression, including when repeated in
another call's argument list.

Tuple lexical traversal uses the existing Term::Block only to visit every value
in order; semantic structure remains in the sealed ExprShape. Element-local
binding scopes do not escape into sibling elements, and current source-ranked
parameter contexts are preserved. Generated free globals in every nested tuple
element receive real definition-site claims. Actual generated helper calls keep
expansion origins, while a caller-supplied helper call inside a slot retains only
its source origin. Unit introduces no lexical operations, without dropping its
actual expansion records or provenance.

Tuple vector growth prepays parser-node, shape, lexical-vector and structural
fence storage plus shallow growth copies before reservation. Repeated slots
retain the existing full output token/byte/scope and occurrence preflight and
pay tuple parsing/vector costs again for actual expanded materialization. Tuple
arity does not consume macro slot count;40elements pass with max_slots1. Existing
depth/metadata/work/output caps remain unchanged, and no allowance is reset.
These are logical resource guards, without latency or resident-memory claims.

Local Linux Rust1.99.1 validation:

- All59 pure Forge tests in8suites passed, zero ignored, using
  cargo test --locked -p vesper-forge --no-default-features --all-targets.
- The10 new tests cover actual unit/group/single/multiple/nested structures;
  call-argument slot atomicity and repeated source anchors; generated versus
  caller-origin nested calls; parameter/template capture and element isolation;
  sibling-block global pre-resolution; ordered calls and empty-unit operations;
  UTF8/string/comment punctuation; arity independent of slot count; malformed
  grammar; preallocation/depth/repeated-slot refusal with expansion retirement;
  real foreign host rejection and unchanged flat lexical tuple refusal.
- Existing10typed quotation and8parameter-host tests passed alongside the new
  tests. Strict pure Forge all-target Clippy, rustfmt and whitespace checks passed.

Owned implementation/tests are90b694c and0d763a6 plus this report. Actual syntax
dependency7d33df6 was cherry-picked locally ascedf8c5; integrate the original
syntax commit separately and do not duplicate the local dependency copy.

This layer establishes category, binding and operation provenance. The compiler
lane remains responsible for existing core tuple AST lowering, actual mode/type/
linear checking, current authority derivation and IR validation. No component,
Lean process or generated program executes here; no native CI result or broader
source Tier2 support is claimed.
