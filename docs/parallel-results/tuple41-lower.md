# Unit and tuple lowering — claim #169

The lowerer handles the actual sealed ExprShape::Tuple using the existing core
ExprKind::Tuple. Unit remains zero elements, grouping retains its former shape,
singletons emit the required comma, and multiple/nested values retain order.
All child reference/binder inventories, parameter alpha-renaming and exact
expected-body comparison remain active. Header and authority policy code was
not changed in this lane.

Both actual delimiter indices are validated. Closing punctuation uses its real
closing-token provenance; commas use the tuple node's structural opening anchor.
Mappings remain provenance anchors rather than promises of equal lexemes.
Tuple AST vectors, recursive depth/node work and closing-anchor storage are
prepaid before allocation. Rendering uses explicit bounded geometric growth,
prepaying storage and old-payload copies before source/map reserves; punctuation
cannot bypass existing output, map or work limits.

Actual committed syntax7d33df6 and host90b694c were imported as dependencies.
No substitute API, guest identity, manufactured CheckedSource or fake executable
was used. Changes stay in lower.rs and the new forge_tuples tests/report, leaving
external158 workflows, build files and test wrappers untouched.

## Validation and remaining integration dependency

On Linux/Rust1.99.1:

- Existing root library, authority, lowering and parameter suites passed70tests
  in4suites with zero ignored. This includes real parameter/header protections
  and the lowerer's exact reported-work regression after rendering prepayment.
- Three new structural/sealed-lowering/resource tests passed. One exercises eight
  actual source/host/lower/parser/tiny-batch-checker cases: unit, grouping,
  singleton, multiple, nested Unicode string values, slots, tuple parameters and
  element scopes. It compares the generated core AST with a real reparse without
  constructing an accepted CheckedSource. Another rejects group/singleton,
  value-order and unit/block mismatches; the third checks tight node/depth/output/
  map/work limits using real sealed inputs.
- Strict targeted Clippy for the root library and old/new compiler tests,
  workspace formatting and whitespace checks passed.

The complete new target currently has3passing and8failing tests, none ignored.
All eight end-to-end cases reach the existing frontend admission refusal:
`inferred tuple aggregate construction is unsupported before checking`, with the
actual expanded diagnostic candidate attached. The lowerer's tuple output is
therefore not yet an accepted compiler artifact. Those tests remain strict for
current helper-body authority drift, caller origin, actual linear tuple errors,
field typing, repeated provenance, arity and whole-output bounds.

The coordinator was notified immediately. Frontend admission is outside this
lane's ownership and must retain protection against exponential inferred types,
such as25repeated `let x=(x,x)` assignments. Removing that refusal without a
bounded pre-checker growth envelope would be unsafe. This lane did not weaken or
bypass it. Full actual compiler/CLI/stdio validation must run after the separately
claimed safe admission change; native hosted and executable acceptance remain
root/#170/#158 responsibilities.
