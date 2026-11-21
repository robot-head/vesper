# `effect`, `handle` and pass-mode words as names; literals past `u64::MAX`; fmt on long chains (SH3 lane 5)

Claim: #1244. Stacks on #1239 (value enums), head `08a12a0`.
Base: `origin/main` `ec19a4c` at the time of writing (#1239 itself is based
on `736d756`).

## What changed

### Contextual `effect`, `handle`, `sink` and `inout`

They join `state`, `linear`, `with` and `eff` as contextual keywords. The
lexer is unchanged (they are still their own tokens, so CST kinds are
unchanged); only the parser's name positions accept them, with at most two
tokens of lookahead:

| word | keyword where | name everywhere else |
|---|---|---|
| `effect` | at the start of an item (`effect E {`, `linear effect E`) | `fn effect()`, `let effect`, fields, methods, patterns |
| `handle` | `handle` followed by an upper-case name, `self::`, `@`, or a name and `:` (`handle E in`, `handle l: E in`, `handle self::m::E in`) | `fn handle(x: u64)`, `handle(1)`, `let handle = ..`, `s.handle` |
| `sink`, `inout` (parameters) | before the parameter's name (`sink x: T`, `inout sink: u64`) | `fn f(sink: u64, inout: u8)` |
| `sink`, `inout` (arguments) | before a token that starts an operand and cannot continue a value: a name other than `as`, a literal, `{`, `!`, `~`, a closure's `|` or `||`, `match`, `if`, `@`, `self`, or another contextual keyword | before `,` `)` `.` `(` `[` `?`, an operator other than `|`/`||`, or `as`: `g(sink)`, `g(sink.a)`, `g(sink(x))`, `g(sink[i])`, `g(sink - 1)` |
| `sink`, `inout` (function types) | always (`fn(sink T)`) | - |

The one change of meaning: an argument passed by `sink` or `inout` whose
expression starts with `(`, `[` or `-` now reads as a value named
`sink`/`inout`. No repository source writes one (checked with `grep` over
every `.ves`, `.vsys` and `.typ` file); the manuscript says to write it in a
block, `sink { (a, b) }`. Before a closure (`sink |x| e`, `sink || e`) the
word stays a mode, so a value named `sink` is or-ed as `(sink) | x`.

`in`, `copy` and `affine` stay reserved: `in` ends every `for` header and
`handle` head, and `copy`/`affine` start modal items and types, so none of
them is a name with two tokens of lookahead in every position. Lane 3
(closure modes, #1242) adds pass modes to closure parameters; those use the
parameter rule above.

Files:

- `crates/vesper-syntax/src/parser.rs`: `contextual_kind`, `handle_starts`,
  `pass_word_is_mode`, `pass_in`; the name lists (qualified path segments,
  expression names, binding patterns, `for` headers, statement anchors) use
  `contextual_kind`. The four words leave the "cannot be a name" list.
- `packages/vesper-front/src/parse.ves` and `packages/vesper-ast/src/lower.ves`:
  the same rules (parser mirror rule).
- `crates/vesper-fmt/src/lib.rs` and its port `crates/vesper-fmt/vesper/src/lib.ves`:
  `effect (`, `handle (`, `sink (` and `inout (` are glued like `eff (`, so
  `fn effect()` stays canonical, and after `.`, `fn` or `let`, or before an
  operator or `?`, the four words space as names (`v.sink[0]`, `sink - 1`).
- `crates/vesper-highlight/src/tier0.rs` and its port: one of the four words
  right after `fn`, `.` or `let`, or right before `:`, `(`, `,`, `)`, `.`,
  `=`, `;` or `}`, is classed as the identifier it is (`fn effect()` is a
  function name, `fn f(sink: u64)` is not a pass mode). As keywords they keep
  their classes.

### Integer literals up to `u128::MAX`

- `crates/vesper-core/src/ast.rs`: `Literal::WideInt(u128)` holds a literal
  above `i128::MAX`; every other integer literal stays `Literal::Int(i128)`,
  and `Literal::int(u128)` picks the variant. A new variant (rather than
  widening `Int`) leaves the Stage 1 verified backend untouched: its two
  matches on `Literal` already end in a wildcard that refuses an unknown
  literal.
- Parser: literals parse as `u128` ("integer literal exceeds u128 range"
  past it). `-` of the literal 2^127 folds to `Int(i128::MIN)`; `-` of
  `i128::MIN` stays a unary minus (it used to be impossible, and would have
  overflowed `-n`). A negated pattern past 2^127 is an error.
- Checker: `WideInt` has type `u128`. `IntegerKind::default_for` gives an
  unconstrained literal past `u64::MAX` (or below `i64::MIN`) the kind
  `i128`, after `i64` and `u64`.
- Evaluator: a 128-bit constant takes the kind of its instruction's type;
  patterns compare `WideInt` exactly. Native code builds `WideInt` constants
  and patterns as `I128` bit patterns. The anvil JIT, wasm backend and Lean
  extraction refuse `WideInt` with a message, as they refuse other 128-bit
  values.
- `packages/vesper-front`: the literal check accepts up to `u128::MAX` and
  mirrors the negated-pattern error. `packages/vesper-ast`: `Lit` gains a
  high word `hi`, kind 5 is `WideInt`, and the dump prints 128-bit values;
  constant and enum-member folding follow the Rust rules on the 128-bit
  magnitude. The hybrid parse module (`crates/vesper-hybrid`) carries the
  new word.

### `vesper fmt` on long chains

The formatter's document is flat in a chain (one `Concat` per line), so the
only limit a chain met was the lexer gate's preflight, which counted every
token of a chain at a level (`f(x) ||` was three) against 128. The parser
itself counts expression height. The preflight now estimates height the same
way: per delimiter level, one per binary operator (and `!`, `~`, `else`,
ranges, assignments, arrows) plus the postfix steps of the current operand
(`.`, a call or index group, `?`, `as`, `with`), so a homogeneous chain
counts one per operator. The ceiling is 160 (it also counts the enclosing
levels' structural steps); the parser's own height limit refuses chains past
128. The Vesper port mirrors it, and its divergent "gate" check now lowers
the ceiling to 16 (the old check edited a line that no longer exists).

## Manuscript (flagged for the owner)

- "Contextual names" (§2.2): adds `effect`, `handle`, `sink` and `inout`,
  their rules, and the block form for a `sink (..)` argument.
- "Integer kinds": a literal is at most `u128::MAX`, a negated one at least
  `i128::MIN`, and one above `i128::MAX` is a `u128`; an unconstrained
  literal that does not fit `i64` takes the first of `u64` and `i128` that
  holds it.

## Stage 1 guard

No guarded path is edited: `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` and `packages/vesper-compiler/**`
are untouched. The verified backend's `Literal` matches end in wildcards, so
the new variant compiles there unchanged and is refused as an unknown
literal. The `bootstrap-stage1` workflow run on the branch is linked from the
PR.

## Numbers

| | before | after |
|---|---|---|
| `fn effect() {}`, `fn handle(x: u64) -> u64 { x }`, `let effect = 1;`, `struct S { sink: u64, inout: u8 }` | E0002 | check and run |
| `fn f() -> u128 { 340282366920938463463374607431768211455 }` | E0002 "exceeds u64 range" | runs (evaluator, JIT, native object) |
| longest `f(x) \|\| ..` chain `vesper fmt` formats (body of a `-> bool` fn) | 41 terms | the parser's limit (about 115) |
| longest `x == k \|\| ..` chain in an `else if` arm | 31 terms | 75 terms |
| front end v2 CST differential (repository, corpus, hand cases incl. 10 new) | 0 differences | 0 differences |
| AST differential (repository, corpus, hand cases incl. 12 new) | 0 differences | 0 differences |
| fmt port vs seed (repository, scrambled, fuzzed, limit sweeps) | identical | identical |
| highlight port vs seed (repository, snippets and prefixes, fuzzed) | identical | identical |

Parsing cost is unchanged within noise: `vesper fmt --check` of
`packages/vesper-ast/src/lower.ves` (4,300 lines) takes 429-448 ms before and
442-463 ms after in a debug build on a loaded machine. The changes add a
kind test to existing name checks and at most two tokens of lookahead at a
`handle`, `sink` or `inout` token.

## Tests

- `tests/contextual_names.rs` (8): the lane repros, pass words as parameter
  names and modes, as argument values, `handle` as a name next to handle
  expressions, `effect` as a field, method and pattern, `for` patterns and
  paths, reserved words keep their message, and the fixture runs on the
  evaluator and the JIT.
- `tests/wide_literals.rs` (7): the repro, `Int`/`WideInt` selection,
  `i128::MIN` in expressions and patterns, context typing between `u64::MAX`
  and `i128::MAX`, out-of-range errors, a native object, and the fixture.
- `tests/fixtures/contextual_names.ves` and `tests/fixtures/wide_literals.ves`:
  every new position, so the front end v2 and AST differentials (which parse
  every repository `.ves` file) cover them.
- New hand cases in `tests/self_host_front_v2.rs` and `tests/self_host_ast.rs`.
- `crates/vesper-fmt/tests/binary_chains.rs` (5): the 24-term repro inside an
  `else if` arm and a `match`, homogeneous chains up to the parser's limit,
  chains past it still refused, the glued call parentheses, and name spacing
  of the four words.
- `crates/vesper-highlight/tests/chroma.rs`: classes of the four words as
  names and as keywords.

## Compiler gaps

1. **`in`, `copy` and `affine` stay reserved.** `fn copy()` and `let in = 1`
   are E0002. `in` would need three tokens of lookahead after `for p`, and
   `copy`/`affine` start modal items and types.
2. **Mixed-precedence chains count every operator.** `x == 1 || x == 2 || ..`
   counts two per term, so `vesper fmt` stops at about 75 terms while the
   parser accepts about 115. A precedence-aware estimate would close it.
3. **Chains between the parser's limit and the preflight ceiling** now fail
   as `Syntax` (the parser's depth diagnostic) instead of `DepthLimit`.
4. **`sink (a, b)` and `inout [x]` as arguments** read as values named
   `sink`/`inout`; write `sink { (a, b) }`.
5. **`fmt` spacing of a bare `sink[i]`.** After `.`, `fn` or `let`, or
   before an operator or `?`, the four words space as names (`v.sink[0]`,
   `sink - 1`), but a bare `sink[i]` formats as `sink [i]` (same tokens, same
   parse), since `inout [u8; 4]` in a function type keeps its space. `eff`
   has the same gap.
6. **Highlighting is lexical.** Tier 0 classes the four words by their
   neighbours; `inout [u8; 4]` in a type stays a mode, but `sink[i]` used as a
   value is still classed as a pass mode. `eff`, `state` and `with` used as
   names are still keywords to the highlighter (SH2 lane 3 gap 4).
7. **128-bit literals on the anvil JIT, wasm and Lean.** They refuse
   `WideInt` (and other 128-bit values) as before.
8. **128-bit constant tables are not read-only data.** A constant table of
   `i128`/`u128` is not encoded as read-only data, because that encoding
   wrote one word per element and dropped the high word; it is built like
   any other aggregate. Native reads of 128-bit elements were fixed by
   #1297 (#1270), and `wide_tables_keep_both_words` checks the evaluator and
   the JIT.
9. **The Lean arithmetic oracle model still says "parse range".**
   `lean/ArithmeticOracle.lean` classes a literal past `u64::MAX` as
   `literalParseRange` (E0002). The parser now accepts it and the checker
   rejects it as out of range for its kind (E0209), since no kind of the
   oracle's 64-bit profile holds it. `lean/` is under the Stage 1 guard, so
   the harness (`src/bin/arithmetic_oracle42/process.rs` and
   `tests/oracle42_acceptance.rs`) maps that class to E0209. The Lean
   class can be renamed in a guarded follow-up.

## CI notes

- The lane changes the parser and the AST, so `Test all targets` misses the
  remote cache for nearly every test, and the root `BUILD.bazel` change also
  runs the Lean replay suite. A cold run passed 1,518 of 1,559 tests with no
  failure before it reached BuildBuddy's one-hour limit (invocation
  3f403911). A second run reuses the cached results.
- `//:test/self_host_ir_inline` (from #1274) ran past the 900 s `long`
  timeout, like the differentials #1362 moved, so it is `eternal` too.

## What is left

The gaps above. SH2 and SH3 ports may now use `effect`, `handle`, `sink` and
`inout` as names and write 128-bit constants as literals.
