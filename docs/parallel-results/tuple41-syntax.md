# Unit and tuple source syntax — claim #167

The source expression recognizer now accepts unit `()`, grouping `(x)`, singleton
tuples `(x,)`, multiple ordered values `(x,y)` and optional tuple trailing commas.
Nested tuples and tuple-valued quotation slots retain their actual original byte
ranges. No new macro category or public syntax API was needed.

This recognizer streams values through the existing bounded expression traversal;
it allocates no tuple AST or element vector. Source/token/depth/work guards and
per-token bump charges remain in force. Tuple arity is independent of macro slot
and function parameter counts. Typed shapes, semantic tuple vectors and expanded
checking belong to the host/lowering lanes.

Tuple commas do not change the existing rejection of ordinary-call or
macro-invocation trailing commas. Nested macro invocations inside macro arguments
or quotations still refuse. Commas and delimiters inside actual strings/comments
do not create argument boundaries. Function parameter inventories and ordinary
support-item parsing are unchanged.

Validation on Linux, Rust1.99.1:

- `cargo test --locked -p vesper-syntax`:80tests in8suites, zero failures and zero
  ignored tests, including both private-construction compile-fail doctests.
- Nine new source_macro_tuples tests cover unit/group/singleton/multiple/nested
  syntax; real quote/argument spans and invocation order; Unicode/comments/string
  punctuation; tuple arity beyond32 under max_parameters0/1; retained call comma
  and nested-macro restrictions; malformed punctuation; unchanged actual support
  AST and parameter inventories; finite resource refusals; every UTF8 prefix.
- Existing12 macro syntax and8 parameter syntax tests still pass.
- Strict all-target syntax Clippy, rustfmt and whitespace checks pass.

The lane used its dedicated existing cache without installations or Wasmtime
builds. It changed no workflows, Bazel/build files or test wrappers reserved by
#158, and grants no checked source, authority, execution or performance claim.
Full real source compilation and native CLI/stdio acceptance remain integration
requirements for the other claimed lanes.
