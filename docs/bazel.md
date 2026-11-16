# Building with Bazel

All CI builds, tests, Clippy and rustfmt checks run through Bazel. Cargo still
works locally, and `Cargo.toml`/`Cargo.lock` remain the single source of truth
for dependencies: Bazel reads them directly through
[rules_rs](https://github.com/hermeticbuild/rules_rs), so there is no second
dependency list to keep in sync.

## Everyday commands

Install [Bazelisk](https://github.com/bazelbuild/bazelisk) as `bazel`; it
downloads the Bazel version pinned in `.bazelversion`. The Rust toolchain
(pinned in `MODULE.bazel`) and the C/C++ toolchain are downloaded by Bazel, so
no local `rustup` or system compiler is needed.

```sh
bazel test //...                                  # everything CI checks
bazel test //crates/vesper-core/...               # one crate, with its lints
bazel test //:test/lint_cli                       # one integration test
bazel test --test_tag_filters=rustfmt //...       # formatting only
bazel test --test_tag_filters=clippy //...        # Clippy only
bazel build //:bin/vesper && bazel-bin/bin/vesper --help
bazel run @rules_rust//:rustfmt                   # reformat every Rust target
```

Native Windows builds do not link yet: the hermetic toolchain links MSVC
binaries through `clang-cl`, and rustc passes it `link.exe`-style arguments
(rules_rs 0.2.4 with llvm 0.8.26). Use Cargo on Windows for now; CI does the
same. Bazel first asks you to accept the
[Visual Studio license](https://visualstudio.microsoft.com/license-terms) with
`common --repo_env=BAZEL_MSVC_RUNTIME_VISUAL_STUDIO_EULA=1` in `user.bazelrc`.

Tests that run Lean use `VESPER_LEAN` when it is set; give it the real binary
(`VESPER_LEAN=$(elan which lean) bazel test //...`), because Bazel runs tests
with a temporary `HOME` where the elan proxy cannot find its toolchains.

## How targets are derived from Cargo

Every Cargo workspace member has a `BUILD.bazel` that calls `vesper_crate()`
(`tools/bazel/defs.bzl`). A module extension (`tools/bazel/cargo_workspace.bzl`)
runs `cargo metadata --no-deps` and records each member's targets: library,
binaries, integration tests, examples and build script, with their entry
points, editions and required features. rules_rs supplies the resolved
dependencies and features of each member, including path dependencies on
other members. `vesper_crate()` declares, per package:

| Target | Cargo equivalent |
| --- | --- |
| `:<dir>` | the library (`<dir>` is the member's directory name) |
| `:<dir>_unit_test`, `:<dir>_doc_test` | `cargo test --lib`, `cargo test --doc` |
| `:bin/<name>`, `:bin/<name>_unit_test` | each binary and its unit tests |
| `:test/<name>` | each file in `tests/` |
| `:example/<name>` | each example (built, as `cargo test` does) |
| `:clippy`, `:rustfmt` | `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` |

So adding a test file, an example, a dependency or a feature to a crate needs no
Bazel change.

### Adding a new crate

Add it to `[workspace].members` as usual, then run

```sh
python3 tools/bazel/sync_build_files.py
```

which writes the crate's one-line `BUILD.bazel`. Bazel stops with the same
instruction if a member has none, and CI checks it too.

### When a crate needs more than the default

Edit the crate's `BUILD.bazel` (see existing ones for examples):

- `extra_compile_data` / `extra_test_data`: files outside the crate directory
  that `include_str!`/`include_bytes!` or tests read, for example
  `["//crates/vesper-loom-trust:files"]` or `["//:lean"]`. Every package
  exports its files as `:files`; the root package also has `//:examples` and
  `//:lean`.
- `build_script_data` / `build_script_env`: inputs of `build.rs`.
- `copy_test_cwd`: tests that must see regular files instead of runfiles
  symlinks (Lean's `--root`, code that rejects symlinked inputs).
- `test_tags`: extra tags per test, for example `no-remote-exec`.
- `test_target_compatible_with`: platform constraints for one test, for
  example to skip `//:test/proof_audit_cli` on macOS, where it is too slow.
- `test_timeouts`: a Bazel timeout per target, for example `"long"` (900s)
  for `//:test/format_proof_cli`, whose real Lean audits run serially. Unlisted
  tests keep their size's default; this setting does not skip any test.

### How tests see their environment

Tests run through a small launcher (`tools/bazel/test_launcher`) that does
what `cargo test` does: it starts the test in its package directory and puts
the package's binaries on `PATH`. Consequently, in integration tests:

- `env!("CARGO_MANIFEST_DIR")` is `.` (the package directory, which is the
  working directory), so paths relative to it work as under Cargo.
- `env!("CARGO_BIN_EXE_<name>")` is the bare binary name, found on `PATH`,
  even after `Command::current_dir`.
- Bazel's sandbox only contains declared inputs, so a test that reads files
  outside its crate must list them in `extra_test_data`.

rules_rs also rejects compiled outputs that embed the absolute build
directory, so library code must not keep `env!("CARGO_MANIFEST_DIR")` or
`env!("OUT_DIR")` values at run time.

## CI and BuildBuddy

| Where | What |
| --- | --- |
| [BuildBuddy Workflows](https://www.buildbuddy.io/docs/workflows-config) (`buildbuddy.yaml`) | Linux: `bazel test //...` (all tests, Clippy, rustfmt), Lean models, AArch64 QEMU checks, native executables, every acceptance script |
| GitHub Actions (`.github/workflows/platforms.yml`) | macOS: `bazel test //...`, Lean models, acceptance scripts. Windows: `cargo clippy`, `cargo test`, Lean models (see above). BuildBuddy's hosted workflows are Linux-only |

On BuildBuddy, compile actions and audited hermetic tests run on remote
executors (`--config=rbe_build`). Host-dependent tests run in the workflow VM,
which has Lean and QEMU installed. BuildBuddy adds its API key to workflow
invocations itself.

The GitHub lanes use BuildBuddy's remote cache when the repository secret
`BUILDBUDDY_API_KEY` is set (`.github/actions/bazel`), and fall back to a
GitHub Actions disk cache otherwise, for example on pull requests from forks.

`.bazelrc` configurations:

| Config | Effect |
| --- | --- |
| `ci` | CI output settings and `--keep_going` |
| `buildbuddy` | BuildBuddy remote cache and build event upload |
| `rbe` | also execute every action on BuildBuddy's Linux executors |
| `rbe_build` | remote builds and audited tests, other Rust tests local |
| `ci_tests` | resource-based local test scheduling for the 8 CPU / 22GB workflow |

The `buildbuddy` config also writes remote RPC diagnostics to
`bazel-remote-grpc.log` and compact execution diagnostics to
`execution_log.binpb.zst`. Git ignores both files, and package input globs
exclude them. The pinned Bazel 9.3.1
supports the enabled remote cache chunking flag; BuildBuddy's
[content-defined chunking support](https://www.buildbuddy.io/blog/content-defined-chunking/)
uses SplitBlob/SpliceBlob for large artifacts. These settings apply when
`buildbuddy` is selected, including through `rbe` and `rbe_build`.

To use BuildBuddy locally, put your personal API key in `user.bazelrc` (git
ignores it):

```
common --config=buildbuddy
common --remote_header=x-buildbuddy-api-key=<your key>
```

## Not committed

`MODULE.bazel.lock` is git-ignored: rules_rs records facts about `Cargo.lock`
there, and concurrent lanes change `Cargo.lock` often, so a committed copy
would conflict constantly. Bazel regenerates it locally.

## Test throughput and cache keys

`Test all targets` uses `--config=ci_tests`: local tests share a six-CPU,
18,432 MiB scheduling budget on the 8 CPU / 22GB VM. The remaining capacity is
for Bazel, downloads and other workflow processes. `--local_test_jobs=0` lets
Bazel use test resource estimates; a positive test-count limit bypasses those
estimates. Unclassified tests reserve 4,096 MiB, while the large Lean replays
and CLI audits reserve 9,216 MiB. These are scheduling estimates, not enforced
process memory limits. The first hosted runs must confirm VM peak memory and
suite duration before reducing these reservations.

`rbe_build` allows remote test execution with a local fallback. Rust test
wrappers and doctests default to `no-remote-exec`. `remote_tests` in
`vesper_crate()` opts audited tests into remote execution; currently these are
the core, syntax and semver crates and the self-hosted frontend corpus. Lean,
QEMU, system-tool and other unaudited tests retain local execution. Lints use
their hermetic toolchains and can execute remotely.

The frontend corpus uses four Bazel shards. Each shard requests four CPUs and
5 GiB from BuildBuddy. Its four workers take the next available case as they
finish. Four-source blocks distribute JIT samples across shards. The global
corpus index still determines JIT sampling, preserving the exact set of
comparisons. Every shard checks the original whole-corpus coverage floors
using the seed and verifies the exact totals assigned to that shard. The fixed
edge-case suite runs on shard zero. Without Bazel's shard variables, Cargo
runs all cases and the fixed suite. Bazel requires a shard status file; the
test acknowledges it and rejects invalid shard configurations. For per-case
timings, use:

```sh
SELF_HOST_TRACE=1 bazel test --test_output=all //:test/self_host_front_end
```

Avoid global `--test_env` entries for inputs used by one test. In particular,
`test_env_inherit` passes the freshly prepared `VESPER_LOGOS42_ROOT` only to
`logos42_acceptance`. That target continues to replay its actual external
checker on every CI run; its random directory no longer invalidates every
other test's cache key. Do not replace it with a fixed path unless the checker
contents and tools also become declared inputs.

Library compilation defaults exclude integration tests, examples and benches;
integration compilation excludes other Cargo test entrypoints. Shared nested
Rust modules stay declared. Root compiler compilation lists only embedded
assets and models. CLI checker source identities use additional
`binary_compile_data`, without becoming library inputs. Integration-only
embedded fixtures use `integration_compile_data` and
`extra_integration_compile_data`. Runtime data remains separate;
`test_data_by_target` supplies a complete replacement for one target's runtime
inputs. `test_path_binaries` similarly narrows its runtime executables. The
frontend corpus uses these to avoid depending on unrelated docs and website
pages. The thirteen fixed proof packets also use a restricted runtime
filegroup containing their Lean sources, provenance, snapshot tool and
negative controls. Generated CI path files and the Bazel module lockfile are
excluded from package input globs.

Regression checks for environment scope and compiler inputs run with:

```sh
bazel test //:bazel_ci_tests //tools/bazel/tests:all
```

When evaluating changes, compare BuildBuddy test cache hits, total local test
time, frontend shard durations, peak VM memory and end-to-end check time.
Compilation cache hits alone do not measure test-cache reuse. Keep `//...`
coverage, proof budgets and negative controls enabled.
