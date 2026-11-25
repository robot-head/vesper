# Self-hosting SH5 lane 14: the test runner (part 1, discovery)

Claim: #1337. Prompt: `docs/agent-prompts/self-host-sh5/14-test-runner.md`.
Base: `main` at `bbda89e`.

Rust stays the default runner. The Vesper code runs only in the hybrid
Stage 2 build (`crates/vesper-hybrid`) and in the differential test.

## What changed

**The swap point (`src/test_discovery.rs`, new lib module).** Test
discovery moved out of `src/test_runner.rs` (a binary module, which no test
or hybrid could call) into `vesper::test_discovery`, unchanged, behind
`TestDiscoveryTable` with `install_test_discovery`, the pattern of
`vesper_syntax::parser::ParseTable`. It has two functions:

- `case_rows(source, module)`: the `// @case name(arguments)` rows before
  each function, their validation errors, and the source with a
  `__vesper_test_case_<n>` wrapper per row appended.
- `discover(source, items, cases)`: which functions are tests (`@test`,
  `@test_case`, `// @test`, the `test_` prefix, case wrappers), the errors
  for generic, bodiless or argument-taking tests, `syntax_attr_to_anvil`,
  and each test's Anvil `HarnessPlan` (`vesper_test::classify`), in source
  order.

`src/test_runner.rs` calls both through the table. The reparse of the
expanded source and the check that each wrapper parses as one call stay in
the runner (they need the parser), as does everything after discovery.

**`@vesper/test-runner` (`packages/vesper-test-runner`).** The same two
functions in Vesper, on @vesper/front's lexer:

| File | Ports |
|---|---|
| `attrs.ves` | `extract_test_attrs` of `crates/vesper-syntax/src/test_attrs.rs`: the attribute stream, `parse_attr_value`, `unescape_string`, `parse_int`, every `@test(...)` key |
| `big.ves` | the `i128` of integer attribute values (sign and two words), its parse and `to_string` |
| `discover.ves` | `rust_case_rows`, `rust_discover`, `syntax_attr_to_anvil`, `attr_value_to_string` and `classify` |
| `util.ves` | Unicode `trim`/`is_whitespace`, `location`, the input and result words |

The input and result words are written and read by
`tests/common/test_runner_words.rs`, which the hybrid and the test share.

**The hybrid module `test_runner`.** `crates/vesper-hybrid/modules/test_runner`
re-exports both entries at export ABI 4 (`in Slice[u8]` to `ChunkSeq[u64]`);
`port/test_runner.rs` is the generated binding; `hybrid.rs` installs the
table before `main`, and `VESPER_HYBRID_STATS=1` reports its calls.

## Differential

`tests/self_host_test_runner.rs` compiles the package with the JIT and runs
both implementations on:

- every `.ves` file under `examples`, `packages`, `crates`, `src` and `tests`;
- the inline sources (raw strings) of `tests/test_runner_cli.rs`,
  `tests/anvil_cli.rs`, `tests/imported_source_tests.rs`,
  `crates/vesper-syntax/src/test_attrs.rs` and `src/test_runner.rs`;
- two mutants of each: one of 43 attribute rows (every `@test` key with good
  and bad values, out-of-range and 128-bit integers, escapes, raw values,
  named pins, malformed lists, `// @test` markers and good, malformed and
  duplicate `// @case` rows) inserted before one of its functions.

For each source that parses, the case rows must be equal (`CaseRows`, with
the expanded source, or the error), and then discovery must be equal
(`Discovered`: item, name, span, the whole `HarnessPlan`, the raw pins, or
the error).

**0 differences.** With four mutants per source (4,372 sources; 3,737
parse, 3,288 reach discovery, 405 with `@case` rows): 6,835 tests and 683
errors, all equal, under both the optimized and the unoptimized JIT build.
The committed test takes two mutants per source to fit the debug CI run,
and runs the unoptimized build on every thirteenth source.

**End to end.** `vesper test FILE` and `vesper test FILE --json` with the
default compiler and with `vesper-hybrid` (release builds) on every `.ves`
file with tests and every `@vesper-std` package, plus
`vesper test --mutate --max-mutants 20` on `@vesper-std/sort` and
`@vesper-std/bytes`: 116 runs, identical stdout, stderr and exit codes.
`VESPER_HYBRID_STATS=1` shows the hybrid answered discovery from Vesper.

The package joins the other ports on the exclusion list of
`tests/fixtures/self_host/driver.rs`: the Stage 1 parser that
`tests/self_host_front_end.rs` drives has no `use` (#880). In Bazel the
differential runs with the `long` timeout (5-10 min in a debug build).

The runner's own suites pass on the default build unchanged:
`tests/test_runner_cli.rs`, `tests/anvil_cli.rs`,
`tests/imported_source_tests.rs` and the unit tests of `src/test_runner.rs`.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changes, and the
verified backend's pipeline does not go through the new swap point (it is
only called by `vesper test`). The bootstrap-stage1 workflow is dispatched on the branch (see the PR).

## Numbers

Release build, ABBA, best of six rounds, discovery (`case_rows` then
`discover`) of the 3,288 corpus sources that reach it:

| | Time | Ratio |
|---|---|---|
| Rust (`rust_case_rows` + `rust_discover`) | 349 ms | 1.00x |
| Vesper, first version (`discover` lexes the source twice) | 741 ms | 2.12x |
| Vesper, attributes read from the shared token stream | 589 ms | 1.69x |
| of which encoding the input words (Rust) | 51 ms | |

Without the crossing the Vesper code is about 1.54x of Rust. Rust stays the
default runner. Discovery is a small part of a `vesper test` run (checking
and the JIT dominate), so the hybrid's end-to-end runs above take the same
time within noise.

`vesper build --lib` of the hybrid module with the debug compiler takes
4 min 10 s (SH5 lane 9's optimizer build time).

## Compiler gaps

None blocked the port. Worth knowing:

- **`ChunkSeq::set` carries `Fail`** (out of range), so a function without
  `Fail` in its row writes with `let ok = s.try_set(i, v);`. Repro:
  `fn f(inout s: ChunkSeq[u64]) { s.set(0, 1); }` gives E0401.
- **The formatter splits comparisons inside `||` chains** at the operator:
  `c == 32 || c == 133` inside a longer chain becomes `c\n == 32`. Repro: a
  function body `(c >= 9 && c <= 13) || c == 32 || c == 133 || c == 160 ||
  c == 5760 || c == 8232` formatted with `vesper fmt`. The port uses `match`
  and named locals instead.
- **Rust's `char::is_printable` table** (which `{:?}` uses to escape a
  string) is not ported: `push_debug_str` escapes ASCII and C1 controls,
  U+00AD and combining marks U+0300-U+036F, which covers every string pin
  in the corpus and the mutants. A `@test_case("..")` pin holding another
  unprintable character (a format character such as U+200B, a private-use
  character) would be quoted differently in `pinned_cases`.

## What is left

- Expect-error matching (`verify_expect_error` and its outcome text), the
  verdict cache keys and records, the obligation ledger and the reports
  (text and `--json`) of `src/test_runner.rs` and `crates/vesper-test`.
- The mutation report (sites are part 3).
- Doctest `generate` (the wrapper source of each example; it needs a
  parse of the snippet) and `nominal_expansion` (an AST walk).
- An end-to-end differential of `vesper test` and `vesper test --mutate`
  (reports and exit codes) between the default and hybrid builds, in
  `crates/vesper-hybrid/tests`.

## Part 2: doctest fence extraction and the token preflight

`src/doctest_extract.rs` is the new swap point: `rust_extract` (the
`vesper` fences of `///` and `//!` blocks, their `compile_fail`, `entry=`
and `sim(...)` attributes, `SimSpec::parse` included) and `rust_preflight`
(the token, declaration, nesting and expression-segment limits), moved
unchanged from `src/doctest.rs`. `packages/vesper-test-runner/src/doctest.ves`
implements both over `@vesper/front`'s `lex_raw`; the hybrid build installs
it (`VESPER_DOCTEST_EXTRACT` in crates/vesper-hybrid/src/hybrid.rs). Words:
tests/common/doctest_extract_words.rs.

- **Differential** (tests/self_host_doctest_extract.rs): every `.ves` file of
  the repository, 58 doc blocks on their own and three mutants per source
  that insert them (good and bad fences, attributes, sim seeds, devices,
  unterminated, nested and tilde fences, `//!` and `////`), run with the
  default limits and with small ones; then the preflight of every extracted
  example's code. 4,935 sources, 1,402 examples, 28 distinct error
  messages: **0 diffs** (optimized), 0 diffs unoptimized (every 13th).
- **Speed**: both functions over the corpus, best of six ABBA rounds:
  Rust 474 ms, Vesper 533 ms, **1.12x** (under the 1.5x bar).
- **End to end**: `vesper test --doc --json` against `vesper-hybrid` on the
  repository's files with examples and hand-made files (examples, a bad
  `sim` attribute, an unterminated fence): identical output.
- Rust stays the default in the Rust build; the Rust copy goes at the
  compiler retirement with the rest of `src/`.

The preflight keeps a running sum of the open groups' outer segments
instead of Rust's per-token fold over the stack (same value; the depth is
bounded by `max_nesting` anyway).

## Part 3: mutation sites

`src/test_mutants.rs` is the swap point: `rust_collect` (the
`collect_source_mutants` / `collect_mutants_in_fn` / `find_operator_span`
of `src/test_runner.rs`, moved unchanged) gives every mutation site of a
source outside its tests: arithmetic, comparison, `&&`/`||`, `+=`/`-=`,
boolean literals and `!`, in top-level functions, impl methods and
conformance methods. `packages/vesper-test-runner/src/mutants.ves`
implements it over `@vesper/ast` (`lower_source`) and `@vesper/front`'s
tokens; the hybrid build installs it (`VESPER_TEST_MUTANTS`). Words:
tests/common/test_mutants_words.rs.

- **Differential** (tests/self_host_test_mutants.rs): every `.ves` file, the
  inline sources of the runner's suites, 16 rows (test markers, `test_`
  names, impls, every operator, comments between operands, a broken
  function) alone and as three mutants per source; each source with no
  test names and with every other function named a test. 4,888 sources,
  9,776 runs, 492,340 sites: **0 diffs**; unoptimized (every 13th) 0 diffs.
- **Speed**: ABBA best of six, Rust 5.31 s, Vesper 2.56 s, **0.48x** (the
  Vesper parse to the AST is faster than `parser::parse`).
- **End to end**: `vesper test FILE --mutate --json` and with
  `--max-mutants 3` are byte-identical between `vesper` and `vesper-hybrid`.

The sort of the sites (by start, end, replacement) is a stable insertion
sort; the walk is already close to source order.

## Part 4: the Anvil call graph and hashes

`packages/vesper-test-runner/src/graph.ves` ports
`FunctionGraph::from_module` (src/anvil_hash.rs) over `@vesper/ast`'s AST of
a source: every top-level function and impl method, keyed `name` or
`Owner::name`, with its canonical span-insensitive BLAKE3 digest and its
direct callees. The canonical bytes of a function go to one buffer and are
hashed once with `@vesper-std/hash`'s pure-Vesper BLAKE3; each effect
instance is hashed on its own and the digests sorted, as in Rust. Entries
`anvil_graph_words` and `anvil_graph_str`; words in
tests/common/anvil_graph_words.rs, whose decoder rebuilds the
`FunctionGraph` maps with `from_module`'s insert order (a later function of
the same key replaces the digest and callees; callers gather every edge).

- **Differential** (tests/self_host_anvil_graph.rs): every `.ves` file, the
  inline sources of src/anvil_hash.rs, tests/anvil_cli.rs and
  tests/test_runner_cli.rs, and 3 mutants per source from 15 rows that add
  every literal kind (including i128 extremes), operators, patterns,
  effects, generics with modes, `fip`, closures, handlers, impls on named
  and applied types, tuple and fn impl owners with effect rows (`Type::Row`
  displays as `eff {:?}`), duplicate keys and a parse error. Graphs equal against
  `FunctionGraph::from_module(&parser::parse(src).module)`.
- **Numbers:** 0 diffs over 4,948 sources (120,092 functions, 176,575 call
  edges); unoptimized build 0 diffs on every 13th source. ABBA best of six
  over the corpus: Rust 5.51 s, Vesper 4.06 s, **0.74x**.
- **No hybrid swap yet.** The call site (test_runner.rs) passes the
  assembled package module (renamed names, case wrappers), not a source, so
  there is nothing for `@vesper/ast` to parse there. The Stage 2 runner
  calls the graph on the source; until then Rust stays, and the Rust is
  deleted with src/ at retirement.

## Part 5: the doctest source guard

`packages/vesper-test-runner/src/guard.ves` ports the fail-closed source
profile that `vesper test --doc` applies to a doctest module after it
parses without diagnostics and before the checker runs: the item limit,
the AST volume (16,384 nodes) and depth (64) limits, the constructs whose
checking grows inferred composites (generic functions, ADTs, effects and
impls, traits and models, nonempty tuples and tuple types and patterns,
generic type applications and calls, builtin generic constructors,
closures, handlers, inferred signatures), and `nominal_expansion` (the
global unfolding budget, depth 48, recursive nominal types). The walk keeps
Rust's explicit last-first stack, so the first error found is the same.
The Rust moved unchanged from src/doctest.rs into src/doctest_guard.rs,
now a swap point (`DocTestGuardTable`, given the source and the parsed
module); `DocTestLimits::validate` stays in `checked` before the swap.
Entries `doctest_guard_words` and `doctest_guard_str`; words in
tests/common/doctest_guard_words.rs.

- **Differential** (tests/self_host_doctest_guard.rs): every `.ves` file
  and each of its top-level functions on its own (so an early generic
  function does not hide the rest of a file), the inline sources of
  src/doctest.rs and tests/doctest_cli.rs, 35 rows and 24 generated rows
  (depth, volume, nominal width and depth at and around each limit), and 3
  mutants per whole source, keeping only sources that parse without
  diagnostics, under four limit sets (256/32768, the defaults 128/8192,
  4/6 and 1/1). The test requires every one of the 20 reachable errors to
  occur.
- **Numbers:** 0 diffs over 14,728 sources (58,912 runs, about 20,700
  passing). Unoptimized build 0 diffs on every 13th source. ABBA best of
  six under the default limits, Rust parse and guard against Vesper
  lowering and guard: Rust 5.77 s, Vesper 2.22 s, **0.39x**.
- **Swapped in the hybrid** (`doctest_guard`, registered in swaps.rs). The
  Vesper side parses the source again; the Rust side reuses the module
  `checked` already parsed. That parse is inside the measured 0.39x.

## Part 6: the generated module of a doctest example

`packages/vesper-test-runner/src/generate.ves` ports `generate` of
src/doctest.rs, moved into the src/doctest_generate.rs swap point
(`DocTestGenerateTable`): the snippet appended to the file's base source
and, unless it declares items, wrapped in the entry function with the
`sim` fence's device effect row; the entry (`entry=`, `main` for a
declaration, the wrapper for a body, none for `compile_fail`); and each
code line's range. A snippet declares items when it parses (`@vesper/ast`,
diagnostics from `@vesper/front`) without diagnostics into a module with
items or model uses, or when its first token is `fn`, a mode, `struct`,
`enum`, `effect`, `impl` or a contextual `trait`/`model`/`pub`. The
check-byte budget and the token preflight (already swapped,
`doctest_extract`) stay in `doctest::generate` before the swap. Entries
`doctest_generate_words` and `doctest_generate_str`; words in
tests/common/doctest_generate_words.rs.

- **Differential** (tests/self_host_doctest_generate.rs): every `.ves` file
  whole (up to 16 KiB), each top-level function (on every seventh file
  with the whole file as the base), each function's body lines,
  three-line windows (malformed and partial prefixes), the inline sources
  of the doctest suites, and 34 rows (each also indented) for every
  declaration start, contextual-keyword lookalikes and non-ASCII text,
  under a cycle of 3 wrappers, `entry=` absent/`run`/empty,
  `compile_fail`, and no sim, an empty, one-device and three-device sim
  row. Modules equal against `rust_generate`.
- **Numbers:** 0 diffs over 39,275 snippets (14,574 declarations, 24,701
  bodies); unoptimized build 0 diffs on every 13th. ABBA best of six:
  Rust 2.36 s, Vesper 2.06 s, **0.87x**.
- **Swapped in the hybrid** (`doctest_generate`).

## Part 7: doctest wrapper names and verdicts

`packages/vesper-test-runner/src/verdict.ves` ports the decisions
`prepare` of src/doctest.rs makes without running an example, moved into
the src/doctest_verdict.rs swap point (`DocTestVerdictTable`):

- `wrappers`: each example's private wrapper `__vesper_doctest_{index}`,
  lengthened with `_` until it collides with no base function, no
  `entry=`, no identifier token of any example and no earlier wrapper
  (error past 128 bytes). The reserved set is a `@vesper-std/intern`
  interner probed with `lookup_view` on the input slice, so a probe
  allocates only for a new name.
- `verdict`: the example's line and column, each checker diagnostic placed
  in the documented file (base diagnostics as they are, snippet
  diagnostics through the code-line segments), and whether checking alone
  settles it: a `compile_fail` fence matched or not, a positive example
  that failed checking, or one whose entry the snippet does not define.
  Line and column placement counts newlines eight bytes at a time and
  answers all of one example's offsets in a single pass.

Entries `doctest_wrappers_words`/`_str` and `doctest_verdict_words`/`_str`;
words in tests/common/doctest_verdict_words.rs.

- **Differential** (tests/self_host_doctest_verdict.rs): every `.ves` file's
  extracted examples plus four-line stand-in windows (up to 64 per file),
  a file of colliding names and a file that hits the name limit; each
  example under synthetic diagnostics before, inside and after the
  snippet, with and without a `compile_fail` code and an own entry.
- **Numbers:** 0 diffs over 19,383 examples (35,800 diagnostics, 16,187
  decided, 1,380 wrapper sets, 1 limit error); unoptimized build 0 diffs
  on every 13th. ABBA: wrappers **1.15x**, verdicts **1.16x**, together
  **1.07x** (first cut was 6.8x from per-offset rescans of the file).
- **Swapped in the hybrid** (`doctest_verdict`).

## Left after part 7

- Reports.
- A hybrid end-to-end test of `vesper test`.
- Execution (anvil_jit, the evaluator, anvil_sim) stays Rust until the
  compiler is retired.
