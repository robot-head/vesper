# Structured Lean effect extraction handoff

Replaced Lean source-fragment construction throughout `src/lean.rs` with
`vesper-lean-syntax` nodes. The only serialization call is the public
`extract(world) -> Result<String, String>` module boundary. New
`extract_module(world) -> Result<Module, String>` exposes the complete typed
syntax tree for tooling and tests; it does not render or parse generated text.

## Implemented migration

- Imports, namespace and linter option are structured module declarations.
- HandlerCPS, its Monad instance, liftAction and the checked Int64 range helper
  are structured definitions/instances. Arithmetic semantics version metadata
  is retained as an AST String definition rather than a source comment.
- ADT dependency groups, structures/inductives, effect classes, operation
  signatures, evidence lifters and lifting instances are structured declarations.
- Function binders, result/state tuples, explicit type/evidence arguments and
  mutual partial definitions are typed syntax nodes. Pure CFG functions consume
  the IR lane's `extract_function_nodes` API directly, without rendering them.
- Generator expressions, patterns, mutable locals, statements and monadic
  branches are AST terms/statements. Identifier identities are `Ident`, while
  handler state/output/base/child/frame types and capability evidence are Term.
- Handlers preserve explicit frame snapshots, operation lenses, mutable argument
  restoration, direct non-tail resumptions, first-class continuation restrictions
  and closure capture/evidence semantics. LetI installation, typed action binds,
  alias refresh matches and primitive failure guards use dedicated do nodes.
- Shared `lean_nodes` helpers lower source types, identifiers, literals,
  arithmetic checks/operations and tuple/unit representations. Floating literals
  retain bit-exact Float.ofBits behavior, including captured subnormal values.
- Formatting remains only for generated identifier names and diagnostics.
  Numeric source projection labels are decoded as indices; no Lean source is
  reparsed, stored as fragments or interpolated into syntax.

The migration exposed an important printer-context requirement: a parenthesized
term-level if/match used as a do-bind action loses Lean's outer mutable-variable
threading. The syntax lane fixed this centrally with context-sensitive structured
`do_action` serialization (`9b7ccdb`). Existing mutable handler tests and a new
combined conditional/match frame test validate the distinction with real Lean.

Partial definitions remain partial executable translations. This migration does
not prove termination, preservation, linear exhaustion or compiler correctness.
Existing unsupported borrowed-alias/continuation diagnostics remain explicit.
Floating-point literal patterns return an unsupported diagnostic rather than an
invalid generated pattern.

## Dependencies and integration

The effects commit changes only `src/lean.rs`, new `tests/lean_effect_ast.rs` and
this report. It consumes these separately coordinated changes:

- Root shared-node baseline `0d76198` and qualified Decidable helper `f044c1f`.
- Syntax API `0ec9040` / root `f9791f3`, plus do-control printer fix `9b7ccdb`.
- Pure IR node API `181a3af` and removal of its old render wrapper `fbfda75`.

The old exact-text prelude assertion in
`tests/lean.rs::effect_places_lemmas_are_axiom_free` requires the root-owned
semantic update: quoted identifiers and explicit AST binders intentionally
change formatting. No raw compatibility strings/comments were added to satisfy
that assertion. The actual CPS operations and all executable examples remain
unchanged.

## Validation

Use a unique target for diverging compiler worktrees; sharing the root target
can reuse artifacts from a different source worktree.

```sh
CARGO_TARGET_DIR=/workspace/vesper-targets/lean-effects-ast \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
VESPER_LEAN=/home/agent/.elan/bin/lean \
cargo test -p vesper --test lean_effect_ast --test lean --test lean_ir_ast \
  --test lean_literal_syntax --test checked_arithmetic --locked --jobs 2 \
  -- --skip effect_places_lemmas_are_axiom_free
```

81 focused tests pass using actual pinned Lean 4.24.0: 18 checked-arithmetic,
51 existing Lean, seven new effect-AST, four pure-IR-AST and one bit-exact literal
test. The one old exact-text assertion is filtered as described above. No Lean
execution was skipped for missing tooling. The arithmetic suite also checks the
reference/native/optimized paths as part of its existing differential contract.

The seven new tests inspect structured class/instance/ADT/function declarations,
then elaborate and execute stateful projected lenses after non-tail resumption,
closure snapshots with outer lexical evidence, checked-failure frame preservation
and noncommitted assignment, binary-exact subnormal captures, and mutable
conditional/match action frames. Unsupported alias diagnostics are asserted too.

Formatting, diff whitespace checks and strict all-target root-package Clippy pass
with the same isolated target and debug/incremental settings. Broader workspace
and final shared-main integration checks remain coordinator-owned.
