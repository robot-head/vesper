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

## What is left (part 2)

- Expect-error matching (`verify_expect_error` and its outcome text), the
  verdict cache keys and records, the obligation ledger and the reports
  (text and `--json`) of `src/test_runner.rs` and `crates/vesper-test`.
- Mutant candidates (`collect_mutants_in_fn`) and the mutation report.
- Doctest extraction (`src/doctest.rs`: fences, attributes, `extract`,
  `generate`).
- An end-to-end differential of `vesper test` and `vesper test --mutate`
  (reports and exit codes) between the default and hybrid builds, in
  `crates/vesper-hybrid/tests`.
