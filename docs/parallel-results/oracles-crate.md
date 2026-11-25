# The `*42` oracles leave `src/bin` (#1825)

The diverse test oracles stay Rust (owner, 2026-11-21: "keep oracles in
rust"), but they lived in the root package's `src/bin`, so `src/` could not be
deleted whole at the compiler's retirement. This change moves them, unchanged,
into their own workspace member, `crates/vesper-oracles`.

## What moved

| From | To |
|---|---|
| `src/bin/{arithmetic_oracle42,resource_oracle42,logos42,lean4lean42}.rs` and their module trees | `crates/vesper-oracles/src/bin/` (same names and layout) |
| `tests/{oracle42_runner,oracle42_corpus,oracle42_acceptance,logos42_acceptance,resource_oracle42}.rs` | `crates/vesper-oracles/tests/` |
| `tests/fixtures/resource_oracle42/` | `crates/vesper-oracles/tests/fixtures/resource_oracle42/` |

12,667 non-test lines; the census now counts them as the `vesper-oracles`
crate (class `diverse`) instead of the root `bin` module, so the `diverse`
total is unchanged and all of `src/` is compiler.

## Edits beyond the move

- `include_str!`/`include_bytes!` paths gained the two extra `../` levels.
- Tests that used `CARGO_MANIFEST_DIR` as the repository root (the resource
  oracle's `lean/` sources, the `lean --print-prefix` working directory for
  `lean-toolchain`) now use `CARGO_MANIFEST_DIR/../..`; fixture paths stay
  crate-relative.
- `oracle42_acceptance` used the `vesper` binary as "a real executable that
  cannot speak the worker protocol". A test can only name its own package's
  binaries, so that control now uses `logos42`. It passes with Lean 4.35.
- The new crate depends on the `vesper` library: the arithmetic oracle's
  worker drives the compiler's parser, checker, optimizer, evaluator and
  native backend as its system under test. At retirement that worker has to
  drive the Vesper compiler binary (or the seed) instead; nothing else in the
  crate reaches into the compiler.

## Build and CI wiring

- Root `Cargo.toml`: one `members` and one `default-members` entry (plain
  `cargo test` keeps running the suites; `cargo build --bin logos42` from the
  root still works).
- Bazel: `crates/vesper-oracles/BUILD.bazel` carries the per-test tags
  (`lean_replay`, resources), timeouts, macOS exclusions, `copy_test_cwd` and
  the `VESPER_LOGOS42_ROOT` inheritance that the root `BUILD.bazel` had for
  these targets. The root keeps a narrow `oracle_sources` filegroup for what
  the oracles embed and drops `binary_compile_data` (only the oracles used it)
  and the oracle-only `integration_compile_data` entries. Labels change from
  `//:bin/logos42` and `//:test/<suite>` to `//crates/vesper-oracles:...`.
- Scripts: `prepare-logos42.sh`, `logos-sweep.sh`, `prebuild-acceptance.sh`,
  `lean_replay_scope.py`, `platform_test_profiles.json` and their tests, the
  Logos sweep workflow, and `rust-census.py` STATUS.

## Verification (Linux x86_64, Rust 1.100.0, Lean 4.35.0)

- `cargo +1.100.0 clippy -p vesper-oracles --all-targets -- -D warnings`: clean.
- `rustfmt +1.100.0 --edition 2024 --check` on every moved file; `sync_build_files.py --check`.
- `python3 -m unittest discover -s scripts/ci/tests -p 'test_platform_*.py'`: 94 passed.
- `cargo test -p vesper-oracles`: all four binaries' unit tests and
  `oracle42_corpus` pass without Lean; with `VESPER_LEAN` set,
  `oracle42_acceptance` (11), `oracle42_runner` (20) and `resource_oracle42`
  (18) pass; with a sealed root from `tools/logos42/setup.py` (driver built by
  `cargo build --bin logos42` at the root), `logos42_acceptance` (17, the full
  three-model Logos replay) passes.
- Bazel 9.3.1 loads the new package: `bazel query` finds the same 18
  `lean_replay` tests (three now under `//crates/vesper-oracles`), and
  `test/resource_oracle42` keeps `timeout = "long"`, `copy_cwd` and its
  resource tags. Bazel builds and the `//:bazel_ci_tests` analysis tests were
  not run here (a toolchain download is blocked in this sandbox); CI runs them.
