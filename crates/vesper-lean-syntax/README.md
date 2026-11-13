# Structured Lean syntax

`vesper-lean-syntax` is a small Rust syntax tree and one Lean 4 source printer.
It provides the replacement boundary for Vesper's scattered source-formatting
code. Emitter-side rollout is coordinated separately from this library's API.

```rust
use vesper_lean_syntax::*;
let name = |s: &str| Name::single(s).unwrap();
let nat = || Term::Ref(name("Nat"));
let module = Module {
    imports: vec![ModuleName::new("Std")?],
    declarations: vec![Declaration::Definition(Definition {
        name: name("double"),
        binders: vec![Binder::explicit(Ident::new("n")?, nat())],
        result: nat(),
        body: Term::binary(BinaryOp::Add, Term::Ref(name("n")), Term::Ref(name("n"))),
        kind: DefinitionKind::Def,
    })],
};
let source = module.render()?;
assert!(source.contains("def «double»"));
assert!(source.contains("«n» + «n»"));
# Ok::<(), Box<dyn std::error::Error>>(())
```

All identifier segments are quoted independently. `Name::single("a.b")` denotes
one component, while `Name::qualified([Ident::new("a")?, Ident::new("b")?])`
denotes namespace qualification. Empty names, quotation delimiters, control and
selected directional-format characters are rejected rather than renamed.
The reserved `_root_` component is also rejected: Lean treats it as a global
lookup prefix even when quoted. Use `Name::rooted()` for explicit global lookup.
Keywords, generated `$` names, Unicode and spaces inside quoted identifiers retain
their exact names. Module imports use a deliberately narrower ASCII dotted path
with alphabetic component starts. No raw-source, raw-operator or raw-tactic string
node bypasses these boundaries.

String/character escaping follows Lean 4 rather than JSON. In particular backspace
and form-feed become `\x08` and `\x0c`; Lean does not accept JSON's `\b` and `\f`.
Control characters, quotes, backslashes and Unicode roundtrip through real Lean
checks. Decimal numeral syntax is canonical nonnegative ASCII with arbitrary
precision. The printer never truncates it. Lean's `OfNat` interpretation still
depends on the expected type: callers must perform their own fixed-width range
checks and select checked or wrapping operations explicitly. Negative syntax is a
separate unary node; floats can be built through explicit function applications,
without this library choosing float/NaN serialization or arithmetic semantics.

The implemented subset includes applications/named arguments, explicit/implicit/
instance binders, lambdas, forall/arrows, let/if/match, lists, tuples/product types,
projections, records/updates, record/signed literal patterns, `do` lets/binds/local
assignment, typed monadic binds, local instances, scalar operators,
ordinary/noncomputable definitions, abbreviations/partial definitions, structures/inductives/classes,
instances, mutual definition/type groups, namespaces, theorems/examples and
evaluation/check commands. Empty records are supported for empty effect dictionaries.
`Term::Infer` explicitly requests Lean elaboration (`_`); unresolved metavariables
remain Lean errors. `Term::Eliminate` emits `nomatch` for an empty-type value.
Fixed proof nodes include `rfl` and `by decide`, never caller-provided tactic text.
Ordinary composite expressions are parenthesized so operator precedence does not
depend on the surrounding expression. Monadic `do` actions retain their dedicated
Lean grammar context instead: bind/statement/tail `if`, `match` and `do` nodes are
printed as do elements with braced branches. Parenthesizing them as ordinary
terms would prevent Lean from threading surrounding mutable locals through them.

Statement-level `do` conditionals and matches use explicit braced sequences. This
preserves updates to surrounding mutable variables, which can be lost or rejected
if such statements are replaced by nested term-level monadic expressions. An empty
statement branch is the fixed no-op `pure ()`. Fixed `set_option` nodes cover only
`linter.unusedVariables` (Bool), `maxRecDepth` and `maxHeartbeats` (Nat); mismatched
value kinds are rejected before printing.

This is **structured syntax, not a typed Lean kernel AST**. Lean remains responsible
for name resolution, binder scopes, universe/type checking, positivity, termination
and proof checking. The caller remains responsible for hygiene, resource limits,
module trust and semantic correspondence with Vesper. `partial def` is an executable
translation and does not establish termination. Recursive caller-built syntax trees
have no hostile-depth or memory-exhaustion guarantee.

Unsupported forms include general tactics/macros, quotations, universe-polymorphic
declaration parameters, pattern guards, generic empty matches, one-element tuples,
mutable pattern destructuring and arbitrary attributes/options. Unsupported syntax
requires a new explicit node/printer implementation; it cannot be smuggled through
a raw fragment. This first milestone does not implement a Lean server protocol,
kernel replay, proof certificates, or compiler-correctness proofs.

Tests invoke the pinned Lean 4.24.0 (`VESPER_LEAN` or `lean` on PATH). An explicitly
configured missing/wrong toolchain fails tests; absent default Lean prints a skip.
They compile and execute pure expressions, ADTs/records, generic/named calls,
instance binders, mutual partial CFGs, recursive inductives, HandlerCPS/Effect_Fail
dictionaries/lifting and state-passing `do` code. Literal/name adversarial cases
are checked for actual Lean meanings.

The inspected `conao3/lean-rs` revision `90bdd9c59fd110b1ba997ee56a0defaff8b7c0ce`
provides Lean-to-Rust generation and Charon/Aeneas-based Rust verification; its
`lean!` interface still consumes Lean text. It does not provide the syntax-builder
API needed here and currently pins Lean 4.35.0. It may be useful for separately
verifying pure Rust helpers after a tested toolchain/dependency decision.
