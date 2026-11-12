# Structured Lean syntax handoff

Implemented the independent `vesper-lean-syntax` library in worktree
`/workspace/vesper-worktrees/lean-syntax`, branch `coordinator/lean-syntax`.
Only this new crate and report were edited. Root workspace registration,
dependency wiring and the active arithmetic lane's emitter files remain untouched.

## Why this boundary

The user proposed `conao3/lean-rs` as an alternative to string-formatted Lean
emission. The inspected revision `90bdd9c59fd110b1ba997ee56a0defaff8b7c0ce`
generates Rust from compiled Lean and uses Charon/Aeneas for Rust verification;
its `lean!` macro still takes Lean source text. It does not expose the syntax
construction API required by Vesper's emitter. It also pins Lean 4.35.0 while
Vesper currently pins 4.24.0. It remains a possible separate helper-verification
tool, rather than a dependency introduced by this milestone.

## Implemented API

The std-only Rust API models identifiers, qualified/rooted names, ASCII module
paths, decimal numeral tokens, terms, patterns, binders and declarations.
Terms include named applications, lambdas/forall/arrows, let/if/match,
tuples/products/projections, records/updates, scalar operators and state-passing
`do` blocks. Declarations include definitions/abbreviations/partial definitions,
structures/inductives, mutual definitions/types, namespaces, theorems/examples
and check/eval commands. `Module::render` and `Term::render` use one private
printer. There is no `Raw(String)` fallback, arbitrary operator or tactic text.

Identifier segments preserve their exact names and namespace distinctions:
one `a.b` segment differs from two `a`/`b` segments. Quotation delimiters and
controls reject instead of undergoing the current emitter's lossy replacement.
`_root_` is explicitly reserved because Lean recognizes that prefix even when
quoted; callers use `Name::rooted()` to request global lookup. Keywords,
Unicode, spaces and generated `$` names are quoted consistently.

Strings/chars use Lean-native escaping. Source inspection identified a concrete
future emitter fix: `serde_json` can emit `\b` and `\f`, but Lean 4.24 accepts
neither. This printer emits `\x08` and `\x0c` and validates actual resulting
character values through Lean. No reserved emitter file was patched.

Decimal token validation never truncates or makes a checked/wrapping arithmetic
choice. Lean's expected type/`OfNat` instance determines its interpretation;
callers remain responsible for fixed-width range checks and semantic lowering.
All composite expressions are parenthesized to preserve their structure.

## Validation

```sh
cargo fmt --manifest-path crates/vesper-lean-syntax/Cargo.toml -- --check
VESPER_LEAN=/home/agent/.elan/bin/lean CARGO_TARGET_DIR=/workspace/vesper/target cargo test --manifest-path crates/vesper-lean-syntax/Cargo.toml --locked --jobs 2
CARGO_TARGET_DIR=/workspace/vesper/target cargo clippy --manifest-path crates/vesper-lean-syntax/Cargo.toml --all-targets --locked --jobs 2 -- -D warnings
```

Passed **12 integration tests and one compiled README example**, with actual
Lean 4.24.0 available; formatting and strict Clippy passed. Tests compile and
execute pure/ADT/generic/record/match/do examples, mutual partial CFGs and recursive
inductives. The tests verify every C0/C1 control, Unicode, quotes/backslashes,
hostile identifier/literal text, arbitrary precision beyond u128, operator
precedence, explicit global lookup, implicit/instance binders and named arguments.
Literal/proof examples are elaborated and checked by Lean; partial definitions
are evaluated without a termination-proof claim.

## Integration and limits

Remove the temporary `[workspace]` and child lockfile, register in root members
and default-members, then add the dependency when migrating emitter consumers.
This first handoff does not change the existing extraction route. Migrate a
coherent backend subset after the arithmetic lane hands off, retaining executable
Lean differential tests. Unsupported syntax must acquire an explicit AST node
and printer branch; do not recover a raw text escape hatch during migration.

This is structured syntax, not a Lean kernel-typed AST, hygienic elaborator,
proof certificate or compiler-correctness theorem. Lean checks names, scopes,
types, universes, positivity, termination and proofs. Callers handle hygiene,
module trust, input resource limits and Vesper semantic correspondence.
General tactics/macros, effect-class/instance declarations, universe-polymorphic
declaration parameters, attributes/options, guarded/empty matches and some
specialized syntax remain unsupported. Recursive caller-built trees have no
hostile-depth or bounded-memory guarantee. The README gives the exact boundary.

## Coordinator integration

Registered the library in both workspace lists and removed its standalone
workspace/lockfile. All 12 real-Lean tests and the README example passed again.
Test artifacts now use an exclusively created private scratch directory, rather
than a predictable directly writable file in the shared temporary directory.
Strict workspace Clippy passed, and all library/test targets cross-compiled for
Windows GNU. The emitter and pinned toolchain remain unchanged; migration still
waits for the checked-arithmetic lane's handoff.

## Effect-emitter API extension

After the arithmetic lane landed, the user explicitly requested actual emitter
migration. Added AST/printer forms required by that migration: class declarations,
named/anonymous instance declarations with structured record/lambda bodies,
fixed typed options, explicit inference placeholders, `nomatch` elimination,
signed i128 literal patterns, record patterns, empty dictionaries, local `letI`,
typed monadic binds and statement-level mutable `if`/`match`. No raw source,
operator, command or tactic text is accepted by these new forms.

The printer uses explicit braced `do` sequences, preserving the distinction between
do statements that update surrounding mutable locals and nested term-level monadic
expressions. Empty statement branches have the fixed `pure ()` no-op semantics.
Only three option names are represented: `linter.unusedVariables` takes Bool;
`maxRecDepth` and `maxHeartbeats` take Nat. Other names/type mismatches cannot be
printed through this API.

**15 real Lean 4.24 integration tests plus the compiled README example pass.** The
new fixtures construct and execute the actual HandlerCPS Monad instance shape,
`liftAction`, `Effect_Fail`, `lift_Fail`, and a lifted effect instance, checking
both an `Except.error 35` failure and `Except.ok 42` success. A separate fixture
verifies that statement-level branches, a match, local mutation and typed tuple
binds yield the expected final value. Anonymous/named local dictionaries, empty
effect dictionaries, signed Int64 patterns and record destructuring compile.

Emitter consumers remain independently owned during this handoff. The AST is
structured syntax, not a Lean kernel/typechecking API or compiler proof. The
original unsupported effect-class/options list above describes the first
milestone; this extension supersedes those specific limitations.

## Monadic action context correction

Actual emitter integration found that a typed monadic bind of an ordinary
parenthesized `if`/`match` term lost Lean's do-control threading for outer mutable
locals. Added context-aware action serialization: bind, expression-statement and
tail positions print structured `if`/`match`/`do` as do elements, with braced
branch sequences. Ordinary expression contexts still use parentheses. Nested
control and branch `Do` nodes retain the same distinction recursively.

All **16 real Lean integration tests and the doctest pass**, including eight
new typed/untyped bind, conditional, match, nested-do, statement and tail cases
that mutate an outer local and independently check its final value. Strict
Clippy and formatting pass. These checks use the isolated target directory
`/workspace/vesper-targets/lean-syntax` with debug info disabled, avoiding shared
source-divergent compiler-worktree artifacts.
