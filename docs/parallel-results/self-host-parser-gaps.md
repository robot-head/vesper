# SH5 lane 4: parser gaps the backend ports hit (#1327)

Base: #1264's head (`claude/project-thread-3p659p`, `00855b8`), stacked on it as
the lane prompt asks; origin/main was `bbda89e` when the work started. Rust
stays the default build; no default build changed and no Rust code was
deleted.

## What changed

Each change is in the Rust parser (`crates/vesper-syntax/src/parser.rs`), front
end v2 (`packages/vesper-front/src/parse.ves`) and the AST port
(`packages/vesper-ast/src/lower.ves`) together.

1. **`if` and `match` are operands.** `PrimaryExpr` takes `IfExpr` and
   `MatchExpr`, so `a && if b { x } else { y }`, `a || match v { .. }` and
   `n == if c { 1 } else { 2 }` parse (SH4 lanes 15 and 16). At a statement
   start `expr_inner` still takes them first, so `if c { } -1` is still two
   expressions, as before.
2. **A statement-position `handle` (and `region`, which is a handle in the
   AST) ending in `}` needs no `;`** (SH4 lane 15). It is block-like, like
   `loop`, `for`, `if` and `match`.
3. **`copy` and `affine` are contextual names**, like `linear` (SH4 lanes 9
   and 13). They are modes only where a mode is parsed: after `mod`, `@` and
   `@@`, and at an item start (`copy struct`, `pub affine enum`). Everywhere
   else a name is expected, they are names: `fn copy()`, `let copy = ..`,
   `struct S { copy: u64, affine: u8 }`, `s.copy()`, `match x { copy => .. }`,
   `for copy in ..`, and `g(sink copy)`.
4. **A keyword that cannot be a name gives one diagnostic** (SH4 lanes 9 and
   11). A pattern at a reserved keyword (`let in = ..`, `let (a, mod) = ..`)
   now reports "expected pattern, found the keyword `in`, which cannot be a
   name", where it used to say only "expected pattern". A parameter or other
   name at a keyword (`fn f(match: u64)`) used to give two diagnostics: the
   named one, then "expected identifier" on the same token. It now gives only
   the named one. The keyword stands in for the name in both cases, so the
   parse goes on with no cascade. The reserved words are `fn`, `let`,
   `struct`, `enum`, `impl`, `mod`, `in`, `return`, `match`, `if` and `else`.
5. **fmt** (`crates/vesper-fmt` and its Vesper port): `copy` and `affine`
   used as names space like names: `copy(1)`, `copy[0]`, `copy - 1`,
   `v.copy[0]`. The mode forms keep their spacing: `fn @copy (u64)`,
   `T: mod copy + Ord`, `p: P @@copy` and `copy struct`.

### Not changed: `while`

The lane prompt lists "`while k < n {` parses as a struct literal" (SH4
lane 13). That diagnosis was wrong. Vesper has no core `while`: the
manuscript says `while` is not a core keyword, so a library can define it as
`sugar`. `while` is read as a name, and the parse fails at the next token
("expected RBrace, found Lower"). The struct-literal restriction already holds
in every header (`if`, `match`, `for`). The owner chose (2026-11-21) to leave
`while` to std sugar rather than add a core form. The manuscript now says so
and gives the workaround, `loop { if !(c) { break; } .. }`.

### Routed: method calls on call results

`f(x).g()` parses: the AST is `Call { callee: Field(Call(f), "g") }`. A
user-defined method on a call result works (`mk().get()`). The E0202 that
SH4 lane 12 hit comes from the checker. It refuses built-in sequence and
string methods on a non-place receiver, so `mk().len()` and
`sub(in s, a, e).as_slice()` fail. This is routed to SH5 lane 6 (#1329) with
the repro below.

## ⚠️ Manuscript changes (flagged for owner sign-off)

`docs/manuscript/content.typ`:
- Grammar: `PrimaryExpr` gains `IfExpr | MatchExpr`.
- "Contextual names": `copy` and `affine` join `state`, `linear`, `with` and
  `eff`. The reserved keywords are listed, and one diagnostic names the
  keyword.
- "Loops and early exits": `handle` and `region` statements need no `;`.
  `if` and `match` are operands outside a statement start. There is no core
  `while`.

The LL(2) notes stay accurate. `if` and `match` are keyword tokens, so
reading them as operands needs one token. `copy` and `affine` are modes only
right after `mod`, `@` or `@@`, or at an item start, which is decided by the
current token.

## Numbers

| repro | before | after |
|---|---|---|
| `a && if b { true } else { false }` (lane 16) | E0002 "expected expression" | checks and runs |
| `a \|\| match x { .. }` (lane 15) | E0002 | checks and runs |
| `handle E in { .. } with { .. }` then a statement, no `;` (lane 15) | E0002 | checks and runs |
| `fn copy()`, `let copy = 1; copy` (lanes 9, 13) | E0002 "expected pattern" | checks and runs |
| `fn affine(affine: bool)` (lane 13) | E0002 | checks and runs |
| `fn f(match: u64)` | 2 diagnostics | 1, naming `match` |
| `let in = 1;` | "expected pattern" | 1, naming `in` |
| front v2 differential (repository + corpus + new hand cases) | 0 diffs | 0 diffs |
| AST port differential (repository + corpus + new hand cases) | 0 diffs | 0 diffs |

Parsing speed is unchanged: the new checks are one token comparison in
`primary`, `lower` and `pattern`.

## Tests

- `tests/sh5_parser_gaps.rs` (8 tests): each repro checks and runs on the
  evaluator and the native JIT. The modes still work, and each reserved
  keyword gives one diagnostic.
- Six new hand cases in `tests/self_host_front_v2.rs` and
  `tests/self_host_ast.rs`.
- `crates/vesper-fmt/tests/modal_names.rs` (2 tests).
- `tests/contextual_names.rs`: `copy` and `affine` left its "stay reserved"
  list.

Run locally (Linux) and passing: `sh5_parser_gaps`, `self_host_front_v2`,
`self_host_ast`, `contextual_names`, `contextual_eff`, `closure_modes`,
`vesper-fmt` with and without `vesper-port`, the hybrid parse tests, and
`cargo clippy -D warnings` on the changed crates. `vesper fmt --check` passes
on the changed `.ves` files.

`tests/self_host_front_end.rs` `differential_over_repository_sources` fails
the same way (node count 137 vs 126 on
`crates/vesper-port-pilot/vesper/pilot.ves`) on #1264's head without this
lane's changes. The Rust CST for that file is byte-identical before and after
this lane, so the failure predates this lane.

## Stage 1 guard

These guarded paths are not edited: `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` and `packages/vesper-compiler/**`.
No pipeline is routed through a swap point. The `bootstrap-stage1` run goes on
this branch once #1264 has merged and the branch merges main (CI rule).

## Compiler gaps

1. **No core `while`** (owner: leave it to std sugar). Until the sugar system
   exists, write `loop { if !(k < n) { break; } .. }`.
2. **Built-in methods on a call result** (checker, routed to lane 6 / #1329):

   ```vesper
   fn mk() -> ChunkSeq[u8] { let mut o = ChunkSeq::new[u8](); o.push(1); o }
   fn main() -> u64 { mk().len() }   // E0202 unknown field, E0206
   ```
3. **`in` stays reserved.** It ends every `for` header and `handle` head, so
   `let in = 1` is still an error. It now gives one diagnostic that names it.
4. **Highlighting is lexical.** Tier 0 still classes `copy` and `affine` used
   as names as modal specifiers (as with `eff`, `state` and `with`, #1264's
   gap 6).
5. **The Stage 1 front end differential** (`self_host_front_end`) fails on the
   base, as described above.

## What is left

Nothing in the lane's list, apart from the two items the owner and lane 6
own (`while` and built-in methods on call results).
