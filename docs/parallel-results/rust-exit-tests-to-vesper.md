# Rust tests to Vesper tests (SH6 lane 19, #1414): part 1

Base: `main` at `ab965f2a`. Prompt:
[19-tests-to-vesper.md](../agent-prompts/self-host-sh6/19-tests-to-vesper.md).

## What changed

- **Runner hook: `vesper test --engines all`** (`src/test_engines.rs`, a few
  lines in `src/test_runner.rs`). A unit test that passes on its usual engine
  must also pass on the evaluator and the JIT, optimized and not; the JIT must
  leave no sequence for teardown, free no sequence twice and free every `Box`
  it allocates; the test and `main` must compile to objects for
  `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`; and where a C
  driver links executables, the executable's exit status must be the low byte
  of the evaluator's `main()`. This is what the Rust `differential` helpers
  check, so a converted case keeps its coverage.
- **`@test(expect_error = ..., message = "...")`**: the diagnostic's message
  must contain the text, as the Rust `rejects(source, code, message)` helpers
  required. Added to the Rust attribute parser (`vesper-syntax`), Anvil's
  `ExpectErrorSpec` and `verify_expect_error` (`vesper-test`, new
  `MessageMismatch` outcome and unit test), the discovery swap point, and the
  Vesper port of discovery (`packages/vesper-test-runner` attrs and plan
  words, decoded in `tests/common/test_runner_words.rs`) so the two
  discoveries still agree. The manuscript does not specify the `@test` keys,
  so no manuscript change.
- **Converter** `scripts/convert-rust-tests.py`: `classify` sorts every Rust
  test file by shape and counts what its helpers make convertible;
  `convert` turns the cases of known helper shapes into `.ves` files, runs
  each with `vesper test --engines all`, and keeps only those that pass.
- **The suite** `tests/vesper/` (README with the layout and the two shapes)
  and its CI hook `tests/vesper_suite.rs`, which runs the suite under
  `cargo test` and `bazel test` (data declared in the root `BUILD.bazel`) and
  checks each family's case count against its `converted.json`.
- **Recipe**: "Moving the tests to Vesper" in
  [porting-a-crate.md](../maintenance/porting-a-crate.md#moving-the-tests-to-vesper),
  with the rule that a PR deleting Rust may not delete a test without a
  replacement.

## The converted family: `tests/continue.rs`

13 cases in `tests/vesper/continue/` (9 values, 4 expected errors). They
cover 6 of the file's 8 `#[test]` functions completely. Kept in Rust for now:

- `continue_formats_and_highlights` calls `vesper_fmt::format_source`
  (a `vesper fmt` golden case is the replacement; the converter has no CLI
  shape yet).
- `continue_drops_loop_local_boxes`: its `differential` case is converted
  (`boxes.ves`, and `--engines all` checks the box counts it read from Rust),
  but the test body has its own JIT loop the converter does not prove covered.

Both suites run in this PR: `tests/continue.rs` unchanged and
`tests/vesper_suite.rs`. The prune of the 6 covered Rust tests is the next
PR, after both are green on all four platforms.

Local (Linux x86_64, release): `vesper test tests/vesper --engines all`
13 passed in 0.8 s. A wrong expected value (100 to 101) fails. Four
platforms: see the PR's required checks.

## How much is left

`python3 scripts/convert-rust-tests.py classify` at this base:

| Where | Shape | Files | Lines | #[test] | Cases of known shapes | #[test] fully of known shapes |
|---|---|--:|--:|--:|--:|--:|
| `crates/*/tests/` | rust-api | 611 | 213,570 | 4,010 | 0 | 0 |
| `tests/` | cli | 26 | 10,264 | 133 | 0 | 0 |
| `tests/` | program | 70 | 26,359 | 520 | 130 | 73 |
| `tests/` | rust-api | 215 | 94,966 | 1,678 | 240 | 23 |
| **Total** | | **922** | **345,159** | **6,341** | **370** | **96** |

Converted: 1 file, 13 cases, 6 `#[test]` functions.

- **program** (70 files): embed programs and check them through
  parse/check/evaluator/JIT only. 485 embedded `main`s. The two helper
  shapes the converter knows reach 130 cases; the rest use other helper
  signatures (`differential(name, source) -> String`, `agree(source,
  expected: &str)`, `rejected(source, code, needle)`). Each is one more entry
  in `helper_shapes`.
- **cli** (26 files): run the binary only. Need a CLI golden shape (the
  command plus expected stdout/status next to the `.ves`).
- **rust-api in `tests/`** (215 files): embed programs (1,357 `main`s) but
  also read a compiler module (IR, optimizer, backends, Lean export). Each
  moves when its module's Vesper port exposes the same check; until then it
  is a differential on the hybrid build.
- **`crates/*/tests/`** (611 files): library API tests. Each flip lane
  rewrites them against the Vesper port as Anvil tests, or keeps them as a
  differential against the last Rust commit.

## Flip-gate verdict

None. This lane deletes no Rust module; part 1 adds the hook, the converter
and the recipe. Rust lines deleted: 0 (the prune in part 2 deletes the 6
covered `continue.rs` tests).

## Census row

`scripts/rust-census.py` is in the plan PR #1415, not yet on `main`. Its
test line ("These move to Vesper tests with each flip (SH6 lane 19)") gets
the converted counts in part 2, after #1415 merges.

## Compiler gaps

- The JIT unit-test batch (`NativeBatch`) falls back to the evaluator for a
  test that calls `main()` ("native batch aggregate/function values
  unsupported"), so plain `vesper test` reports those as `[evaluator]`.
  `--engines all` runs the single-function JIT path, which handles them. Repro:
  any `tests/vesper/continue/*.ves` other than `names.ves`.

## Next

1. Prune the 6 covered tests from `tests/continue.rs`.
2. Teach the converter the next helper shapes (`-> String` differentials,
   `agree`, `rejected(.., needle)`) and convert the `program` files,
   starting with `tests/language.rs` (76 cases of known shapes already).
3. A CLI golden shape for the `cli` files.
