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

## Local validation and final integration

The initial lane checks on Linux/Rust1.99.1 passed:

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

At the initial lane checkpoint, the complete new target had3passing and8failing
end-to-end tests, none ignored. All eight failures reached the existing frontend
admission refusal, `inferred tuple aggregate construction is unsupported before
checking`, with the actual expanded diagnostic candidate attached. The
coordinator was notified immediately; those tests were kept strict rather than
bypassing the guard. Admission required a separately claimed bounded pre-checker
growth envelope preserving refusal of exponential inferred types, such as
25repeated `let x=(x,x)` assignments.

The coordinator's final root integration at2d32fb8 includes that bounded
frontend admission and the subsequent closed-header/annotation safeguards. The
coordinator reports132passing tests in12integrated suites, including all11tests
in forge_tuples and all6tuple CLI tests. The formerly blocked cases now exercise
actual compiler acceptance/refusal for helper-body authority drift, caller
origin, linear tuple errors, field typing, repeated provenance, arity and output
bounds. Strict whole-workspace checks also passed.

These final results are attributed to the coordinator's actual root integration;
this report update did not run another build or write any build cache. The early
admission refusal is historical evidence, not a remaining integration blocker.
Native hosted and executable acceptance details remain in the coordinator/#170/
#158 reports; this lane does not infer hosted results from local Linux checks.
