# `eff` as a field, method and binding name (SH2 lane 3)

Claim: #1175. Census gap C1 (`docs/parallel-results/self-host-census.md`).
Base: `c9f228b` (origin/main, 2026-11-20).

## What changed

`eff` is now a contextual keyword, like `state`, `linear` and `with`. It opens
an effect row only where a signature allows one:

- after a `fn` signature (`fn f() -> T eff [E]`),
- after a function type (`fn(u64) -> u64 eff [e]`),
- after a closure head, and there only when the next token is `[`
  (`|x| eff [E] body`),
- as the row bound of a generic (`[e: eff]`).

Everywhere a lower-case name is expected it is a name: struct fields, struct
literal fields and shorthand, field access, methods and method calls,
parameters, `let` bindings, patterns, expression names and qualified path
segments. The census repro now checks and runs on the evaluator and the JIT:

```text
struct Row { eff: u32 }
fn main() -> u32 { let r = Row { eff: 1 }; r.eff }
// before: [E0002] expected a lower-case identifier, found the keyword `eff`
// after:  1
```

The lexer is unchanged: `eff` is still the `Eff` token, so CST token kinds
and the highlighter's classes are unchanged. Only the parser's name positions
accept it, and the formatter glues `eff (` (it never opens a row).

Files:

- `crates/vesper-syntax/src/parser.rs`: `Eff` joins `at_contextual_name` and
  the explicit name lists (qualified path segments, expression names, binding
  patterns); it leaves the "keyword cannot be a name" list. A closure takes an
  effect row only at `eff [`, so `|x| eff` and `|x| eff.n` are bodies.
- `packages/vesper-front/src/parse.ves`: the same change (parser mirror rule):
  `contextual_name`, `reserved_word`, `closure`.
- `packages/vesper-ast/src/lower.ves`: the same change in the Vesper AST
  lowering, so `tests/self_host_ast.rs` keeps 0 differences on the new
  fixture. This package is outside the lane's listed paths; the hunk is the
  mirror of the parser hunk and nothing else.
- `crates/vesper-fmt/src/lib.rs` and its port `crates/vesper-fmt/vesper/src/lib.ves`
  (`gap`): `eff` followed by `(` gets no space, so `fn eff(in self)` and
  `r.eff()` stay canonical instead of becoming `fn eff (in self)` and
  `r.eff ()`. `eff [` keeps its space. The seed `vesper-fmt-seed` builds the
  same `src/`, so port, seed and Rust still agree.
- `src/tooling/lint.rs`: the explicit-effect-row finder took the last
  depth-0 `eff` token of a function header as the row; with `fn eff(..)`
  allowed it now requires `eff [`.
- `docs/manuscript/content.typ`, "Contextual names" (flagged in the PR): adds
  `eff` to the contextual keywords and states the closure rule.
- `tests/contextual_eff.rs`: census repro first (committed failing), then
  bindings, parameters, methods, patterns, rows in signatures, function types,
  closures and row bounds, and the error for `eff` without a row.
- `tests/fixtures/contextual_eff.ves`: every `eff`-as-name position, so the
  front end v2 and AST differentials (which parse every repository `.ves`
  file) cover the change. `tests/contextual_eff.rs` also runs it.

## The one ambiguity

After a closure head, `eff [` could start an effect row or index a value named
`eff`. It always starts the row, as before. A closure whose body indexes such
a value is written `|i| { eff[i] }` or `|i| (eff)[i]`. No repository source
hits this, and the manuscript states the rule.

## Stage 1 guard

No guarded path is edited (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**`, `packages/vesper-compiler/**`).
The `bootstrap-stage1` workflow on the branch reproduces `bootstrap/stage1.lock`;
see the PR for the run.

## Numbers

| | before | after |
|---|---|---|
| census C1 repro | E0002 (3 errors) | checks, runs, `1` |
| front end v2 CST differential (repository + fixture) | 0 differences | 0 differences |
| AST differential | 0 differences | 0 differences |
| renames an SH2 port needs for `eff` | about 100 (census) | 0 |

Parsing cost is unchanged: the change adds one token kind to existing
name-position checks and one lookahead in `closure`.

## Other reserved words the compiler's Rust code uses as names

Rough counts of lines in `src/` and `crates/vesper-core` that use the word as
an identifier outside strings and comments (`grep -w`, so some lines are
comments or type names; they size the problem, not exact uses):

| word | lines | why it is not as contained as `eff` |
|---|---|---|
| `effect` | ~520 | starts an item (`effect E {`, `linear effect E`); contextual only by lookahead `effect Upper`, and at statement start an expression `effect` could collide with sugar skeleton entry (§2.2 `LowerIdent` followed by an identifier) |
| `handle` | ~180 | starts an expression (`handle E in`), so `handle` as a name collides in expression position; needs `handle Upper` lookahead in both parsers and the AST |
| `copy` | ~120 | mode keyword in types, generics and `copy struct`; names collide with `@copy`/`mod copy` positions |
| `sink` | ~50 | pass mode in parameters and arguments: `f(sink x)` versus `f(sink)` needs lookahead in every argument list |
| `inout` | ~25 | pass mode; same as `sink`, plus `inout x.{a, b}` projections |
| `affine` | ~15 | mode keyword, like `copy` |

None is fixed here: each changes an expression or argument position, not just
name positions, and each needs its own manuscript decision. `eff` was the only
one whose keyword uses all sit at signature ends.

## Compiler gaps

1. **`effect` and `handle` as names.** `fn handle(x: u64) -> u64 { x }` and
   `let effect = 1;` are still E0002. A port renames them (census: the sample
   used `effect_at`, `handler`).
2. **Pass-mode words as names.** `struct S { sink: u64 }` is E0002.
3. **Closure body `eff[i]`.** `|i| eff[i]` parses as an effect row (by
   design above); `|i| { eff[i] }` works.
4. **Highlighting.** `crates/vesper-highlight` tier 0 still classes every
   `eff` token as a keyword (as it already does for `state` and `with` used as
   names), and its row scan treats `eff[` after an `eff` binding as a row.
   Semantic highlighting from the CST would fix both; left alone because the
   tier 0 port mirrors it.

## What is left

The gaps above, each a separate lane if a port needs it. SH2 ports may now use
`eff` as a name directly.
