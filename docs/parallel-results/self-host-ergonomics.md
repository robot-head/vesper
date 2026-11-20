# Self-hosting SH1 lane 5: front-end ergonomics

Claim: #1148. Prompt: `docs/agent-prompts/self-host-sh1/05-ergonomics.md`.
Base: `main` at `e29e63d`.

Compiler-shaped Vesper (the front end v2, the std I/O driver, the hybrid
pilot) kept running into six small gaps. This lane closes each one with the
smallest manuscript change and support in the checker, evaluator, JIT and
objects.

## Reproductions on the base

All six gaps still failed on `e29e63d`. The first commit of the PR adds the
repros as tests, and they fail there.

| Gap | Source | On `e29e63d` |
|---|---|---|
| Or-patterns | front end v2 gap 1 | `1 \| 2 => 5` is `E0002: expected FatArrow, found Pipe` |
| `-> Str` for literals | front end v2 gap 6 | `expected Str, found String`, then `E1530` |
| Bound literal view | std-io gap 3 | `let w = ".ves".as_bytes();` is `E1531` |
| Byte-slice comparison | std-io gap 7 | no builtin; `<` on `Slice[u8]` is refused |
| Total checked arithmetic | hybrid gap 2, front end v2 gap 5 | `checked_add_u64` is `E0201 unknown name` |
| Long `else if` chains in `vesper fmt` | front end v2 gap 2 | 21 or more arms: `cannot format source: DepthLimit` |

## What changed

Each item is a spec call. All six are in `docs/manuscript/content.typ` and
listed in the PR body.

1. **Or-patterns.** The grammar now reads `MatchExpr ::= 'match' HeaderExpr '{' ( Pat ( '|' Pat )* '=>' Expr ','? )* '}'`.
   An arm `p1 | p2 => e` means the arms `p1 => e`, `p2 => e` in order. The
   parser (`crates/vesper-syntax`) emits one AST arm per alternative, all
   sharing one body expression with the same span. The CST keeps the source
   shape: the alternatives are sibling `PATTERN` nodes with `|` tokens
   between them. The checker groups consecutive arms that share a body and
   requires every alternative to bind the same names at the same types (new
   code `E0214`). It reports a diagnostic in the shared body only once.
   Exhaustiveness and every backend see ordinary arms, so nothing below the
   checker changed. Nested or-patterns (`Some(1 | 2)`), a leading `|` and
   guards are out of scope. The parser change is mirrored in
   `packages/vesper-front/src/parse.ves`.
2. **Static text.**
   - A function whose result type is `Str` returns static text: every value
     it returns must be a string literal, a call of another `-> Str`
     function, or an `if`, `match` or block made of those (`E1530`
     otherwise).
   - Inside such a function, a literal the result expects is typed `Str`.
     Natively a literal is already `{len, 0, &static}`, so the value needs no
     new lowering.
   - A bound view (`let w = ".ves".as_bytes();`, `let n = name(k);`) may now
     take a string literal or a `-> Str` call as its source, since static
     bytes cannot change or dangle.
   - `Option[Str]` and other types that only contain `Str` stay refused.
3. **`a.cmp(in b) -> i32`** on `ChunkSeq[u8]` and `Slice[u8]`. The owner
   chose this form on 2026-11-19, over a free `cmp_bytes` and over `<` on
   byte slices. It returns the three-way lexicographic order as `-1`, `0` or
   `1`, with a proper prefix first. It is total and read-only, so both
   operands may view one sequence. It is `SeqOp::BytesCmp` beside
   `SeqOp::BytesEq`:
   - the evaluator uses `<[u8]>::cmp`;
   - native code takes 16-byte SSE/NEON steps while the lanes agree (the
     same vector gating as `eq`), then compares bytes up to the first
     difference, then the lengths.
4. **`checked_add_k`, `checked_sub_k`, `checked_mul_k`** for all ten integer
   kinds, returning `Option[k]`. They are reserved prelude functions like
   the bit builtins (`src/integer_widths.rs`), and their bodies use only
   total operations:
   - add and sub use wrapping arithmetic and sign or carry tests;
   - mul uses the widened exact product for kinds up to 32 bits, and the
     high word of `mul_wide` over the magnitudes for 64-bit kinds.

   A function with no `Fail` in its row may call them. Defining one is
   `E0100`.
5. **`vesper fmt` and `else if` chains.** The formatter's preflight counted
   every token of an `else if` chain as one expression chain, so it refused
   21 arms (`MAX_CHAIN` 128). Now `} else` goes back to the count where its
   `if` started, so a chain counts like a single `if`, and chains used as
   operands (`if .. else .. + if ..`) still count. Chains now format up to
   the parser's own depth limit (about 124 arms), the same source `vesper
   check` accepts.
6. **Front end v2 rewrites.** `kind_name` is `-> Str`, so a diagnostic no
   longer allocates its kind names. `precedence` uses or-patterns (25 arms
   down to 12). The differential test `tests/self_host_front_v2.rs` stays
   green: all six tests pass, with or-pattern edge cases added to its edge
   list.

## Tests

| Test | What |
|---|---|
| `tests/or_patterns.rs` (6) | literal, constructor (shared binding) and tuple alternatives on every backend against a Rust model; exhaustiveness through alternatives; `E0214` for different names, a missing name, different types; one diagnostic for a shared body; moves in a shared body; malformed alternatives |
| `tests/str_literals.rs` (4) | `-> Str` tables, `return` of static text, bound literal views, `push_str`, `==`/`<` on `-> Str` results on every backend; `E1530` for non-static results and `E1531` for a mutable local's view; `cmp` on 1,000+ pairs (every length 0..40, equal, prefix, one byte changed at every position) against Rust's `Ord`; `cmp` on views of one sequence and on literal views |
| `tests/checked_arith_builtins.rs` (3) | every kind and op over 13 to 20 boundary values per kind (min, max, 0, 1, 2, 3, the square-root magnitudes and their neighbours, -1, -2) against exact `i128` arithmetic; run-time operands from a loop; reserved names |
| `crates/vesper-fmt/tests/else_if_chains.rs` (2) | chains of 20 to 120 arms format idempotently; a chain inside a call and a block; `if` operands still hit `DepthLimit` |

"Every backend" (`tests/ergonomics_support`) means:
- the evaluator, optimized and not;
- the JIT, optimized and not;
- x86-64 and AArch64 object emission;
- an AOT executable's exit status, where a C driver links.

Lean extraction is not exercised. Strings are refused there, as before, and
the checked builtins are ordinary prelude source.

`tests/fixtures/self_host/driver.rs` skips `tests/fixtures/ergonomics`, because
the frozen Stage 1 parser (`packages/vesper-compiler`) has no or-patterns.

## Numbers

Instructions measured with callgrind on optimized AOT executables, Linux
x86-64:

| Kernel (100k or 1M calls) | Before (workaround) | After | Ratio |
|---|--:|--:|--:|
| Compare two equal 64-byte slices: `try_get` loop vs `a.cmp(in b)` | 2,974 Ir/call | 89 Ir/call | 33x fewer |
| `u32` add with overflow check: widen + `wrapping_add` + range test vs `checked_add_u32` | 25.3 Ir/iter | 23.3 Ir/iter | 1.09x fewer |

The checked builtins inline like any prelude function, so they cost no more
than the hand-written workaround. A native `uadd_overflow` lowering could
save a few more instructions (see gaps).

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed. The
`bootstrap-stage1` workflow run on the branch is linked from the PR.

## Compiler gaps

1. **An or-pattern body is emitted once per alternative.** `1 | 2 | ... | 50
   => { big }` duplicates `big` fifty times in the IR. A shared join block
   would need an or-pattern in Place IR (`src/ir.rs`, outside this lane).
   ```text
   fn f(k: u8) -> u64 { match k { 1 | 2 | 3 | 4 => { long_body() }, _ => 0 } }
   ```
2. **No nested or-patterns or guards.** `Some(1 | 2) =>` is a syntax error.
3. **`else if` chains stop at about 124 arms.** The parser's expression
   height limit (`HARD_PARSE_DEPTH` 128) counts each `else if` as one level,
   because the AST nests them. Removing the limit needs iterative `if`
   handling in every recursive pass.
   ```text
   fn f(k: u64) -> u64 { if k == 0 { 0 } else if k == 1 { 1 } ... (125 arms) else { 9 } }
   // E0003: Depth limit 128; observed 129
   ```
4. **Checked builtins are prelude source, not single instructions.**
   Cranelift's `uadd_overflow`/`sadd_overflow`/`umul_overflow` would make
   `checked_add_k` an add and a flag test once Option results can be built
   directly in native lowering.
5. **Static text has no `Option[Str]` or `(Str, u8)`.** A table that returns
   a name and a flag still needs two functions.
6. **The Stage 1 front end lacks all of these.** `packages/vesper-compiler`
   is frozen by `bootstrap/stage1.lock`, so sources that use them stay out of
   the Stage 1 corpora.

## Left for later

- Nested or-patterns and match guards.
- Shared bodies in Place IR (gap 1).
- `checked_div_k`, `checked_neg_k`, `checked_shl_k`.
- Adopting `cmp` in `@vesper-std/fs` sort and the std-io driver's `is_ves`.
  Those are other lanes' files.
