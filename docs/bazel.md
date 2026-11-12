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

On Windows, the hermetic toolchain uses the MSVC runtime and asks you to accept
the [Visual Studio license](https://visualstudio.microsoft.com/license-terms):
if you may use it, add `common --repo_env=BAZEL_MSVC_RUNTIME_VISUAL_STUDIO_EULA=1`
to `user.bazelrc`. CI does this on GitHub's Windows runners.

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
| GitHub Actions (`.github/workflows/platforms.yml`) | macOS and Windows: `bazel test //...`, Lean models, acceptance scripts. BuildBuddy's hosted workflows are Linux-only |

On BuildBuddy, compile actions run on remote executors (`--config=rbe_build`)
and tests run in the workflow VM, which has Lean and QEMU installed. BuildBuddy
adds its API key to workflow invocations itself.

The GitHub lanes use BuildBuddy's remote cache when the repository secret
`BUILDBUDDY_API_KEY` is set (`.github/actions/bazel`), and fall back to a
GitHub Actions disk cache otherwise, for example on pull requests from forks.

`.bazelrc` configurations:

| Config | Effect |
| --- | --- |
| `ci` | CI output settings and `--keep_going` |
| `buildbuddy` | BuildBuddy remote cache and build event upload |
| `rbe` | also execute every action on BuildBuddy's Linux executors |
| `rbe_build` | remote execution for builds, tests on the local machine |

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
