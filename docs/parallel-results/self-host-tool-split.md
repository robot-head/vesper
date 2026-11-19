# Self-hosting SH0 lane 2: tool subcommands out of the compiler binary (#1124)

Eight subcommands that the compiler does not need for its own job now run
as separate executables. `vesper <tool> ARGS...` runs `vesper-<tool>
ARGS...` with the same arguments, standard streams and exit status, the way
Cargo runs external subcommands. Six crates leave the compiler's normal
closure, five of them seed copies. That shrinks the code Stage 2 has to port
or keep behind `extern`. No Rust code was deleted, no seed crate was removed,
and every tool still runs the same Rust code as before.

Base: `main` at `b71ac81`. PR #1133.

## What changed

| Before (in `vesper`) | After |
|---|---|
| `src/bench_commands.rs` | `crates/vesper-tools/src/bench.rs`, binary `vesper-bench` |
| `src/grammar_commands.rs` | `grammar.rs`, `vesper-grammar` |
| `src/json_commands.rs` | `json.rs`, `vesper-json` |
| `src/audit_commands.rs` | `audit.rs`, `vesper-audit` |
| `src/publish_commands.rs` | `publish.rs`, `vesper-publish` |
| `src/sbom_commands.rs` | `sbom.rs`, `vesper-sbom` (`publish` uses it too) |
| `package_commands::resolve` | `resolve.rs`, `vesper-resolve` |
| `src/tee_commands.rs` (`build --tee`) | `tee.rs`, `vesper-tee measure <exe>`, run by `vesper build --tee` |
| `src/proof_commands.rs` (binary module) | the library module `vesper::proof_commands` (used by `verify`, `proof-audit` and `publish`) |
| `package_commands::atomic_write` | `vesper::atomic_file::atomic_write` (re-exported from `package_commands`) |

Each module moved with `git mv`. The one code change is in `vesper-bench`:
its randomized-layout runs used to re-spawn `vesper bench`, and now they
re-spawn the `vesper-bench` executable directly. Every tool runs on the same
64 MiB stack thread as the compiler CLI and prints errors as `vesper:
<error>` with exit status 1 (`vesper_tools::main`).

**Dispatch** (`src/tool_commands.rs`). The tool commands listed in
`TOOLS`, and any other command the compiler does not know, are dispatched
before the CLI thread starts. The lookup order for `vesper-<tool>` is:

1. The directory of the running `vesper`, and the directory of its
   canonical path when `vesper` is a symlink.
2. `crates/vesper-tools/bin/` next to `bin/vesper`. This is where Bazel
   puts the tools, both in `bazel-bin` and in runfiles.
3. `PATH`. On Unix, files without an execute bit are skipped.

If a known tool's binary is missing, the error names the binary and how to
build it. If an unknown command has no `vesper-<name>` executable, the error
is the same `unknown command '...'; see --help` as before. Exit codes pass
through unchanged; a child killed by a signal exits with 128 plus the
signal number. `vesper --help` is unchanged and still lists every tool.

**Installing the tools.**

- *Cargo:* `crates/vesper-tools` is in both `members` and
  `default-members`, and it has an integration test (`tests/binaries.rs`).
  As a result, `cargo build` and `cargo test` at the root put
  `vesper-<tool>` beside `target/<profile>/vesper`.
  `cargo install --path crates/vesper-tools` installs the tools next to an
  installed `vesper`.
- *Bazel:* `//:bin/vesper` has the eight tool binaries as `data`
  (`vesper_crate(binary_data = ...)`), so building it also builds them, and
  the dispatcher finds them. Root integration tests also get them on `PATH`
  (`vesper_crate(extra_test_path_binaries = ...)`).

**Which libraries the tools link.** The lane prompt says tools link the
normal crates. Every normal package involved here except `vesper-bench`
always builds its Vesper port (`build.rs` calls `Port::build`
unconditionally). Linking those would make Vesper the tools' default build,
which the owner's standing parity rule forbids until each port matches Rust.
`vesper-bench` is Rust by default, but `vesper-bench-pmu` implements the
seed's `WindowObserver` traits, so bench needs the seed too. So the tools
link the same seed copies the compiler linked before, and every tool's
behavior is unchanged. When a port reaches parity, its tool can switch to
the normal crate by editing one line in `crates/vesper-tools/Cargo.toml`.

**Allocator.** `bench_commands` installed a counting `#[global_allocator]`
in the whole compiler binary, which `--expect-zero-alloc` reads. Only
`vesper-bench` and the other tool binaries (which link the same library)
have it now. The compiler uses the system allocator directly.

## Closure before and after

Measured with `cargo tree -p vesper -e normal,build` and with lines counted
in each package's library and binary source directories (tests
excluded). Release builds on a 4-core host,
Rust 1.97, `CARGO_INCREMENTAL=0`.

| | before (`b71ac81`) | after |
|---|--:|--:|
| crates in the closure | 338 | **301** |
| first-party crates | 42 (391.7k lines) | **36** (359.5k lines) |
| third-party crates | 296 | **265** |
| frozen seed copies | 18 (114.7k lines) | **13** (90.7k lines) |
| release `vesper` size | 65,360,472 B | **59,935,896 B** (-8.3%) |
| rebuild + link of the `vesper` binary crate after touching `src/main.rs` (2 runs each, alternating) | 175 s, 170 s | **158 s, 154 s** (-10%) |

These crates left the closure: `vesper-aegis-seed`, `vesper-bench-seed`,
`vesper-grammar-seed`, `vesper-loom-audit-seed`, `vesper-loom-solver-seed`,
`vesper-bench-pmu`, and 31 third-party crates (mostly Aegis's X.509,
`ring`, `webpki` and COSE stack). The aegis and bench seeds remain root
**dev**-dependencies, because `tests/native_verifiers_cli.rs` and
`tests/bench_cli.rs` compare against them. Dev-dependencies are outside the
normal closure.

Each tool binary in release builds is 11-15 MB (`vesper-bench` is 35 MB,
because it links the native batch compiler). The tools crate builds in
7.5 minutes from a warm compiler library.

## Seeds still in the compiler's closure

| Seed | Compiler module that needs it | Leaves when |
|---|---|---|
| `vesper-crucible-seed` | `native_coverage` (`Probe`, `CmpKind`), which the coverage-guided `fuzz` and `verify --refute` share with the native backend | SH4 (coverage hook). `crucible_commands` itself could move to a tool afterwards. |
| `vesper-semver-seed` | `interface.rs` (`vesper emit --emit interface`), `tooling/lint` | SH1 (lint), interface with Stage 2 |
| `vesper-serde-seed` | `vesper-lsp` transport (`vesper lsp`) | SH5 |
| `vesper-loom-calculus-seed` | `packages`, `package_graph`, `interface`, LSP, `forge_frontend` | SH6 |
| `vesper-pact-seed` | `pact_frontend` | SH2 |
| `vesper-alloc-seed` | `regions` | SH2/SH4 |
| `vesper-tensor-seed` | `tensor_types` | SH2 |
| `vesper-beacon-seed` | `trace_effects`, `trace_commands` | SH2 |
| `vesper-ebpf-seed` | `ebpf_backend`, `ebpf_commands`, `stack_bound` | SH4 |
| `vesper-silicon-seed` | `native_verifiers`, `freestanding`, `stack_bound` | SH4 |
| `vesper-kinetic-seed` | `native_verifiers` | SH4 |
| `vesper-resonance-seed` | `native_verifiers` | SH4 |
| `vesper-simpulse-seed` | through chrono, doc and crucible | SH5 |

## Tests

- Every existing CLI test passes with no changes to its expectations. That
  includes `bench_cli`, `grammar_cli`, `audit_cli`, `publish_cli`,
  `sbom_cli`, `package_cli`, `locked_resolution_cli`, `upgrade_cli`,
  `atomic_output_cli`, `codex_rest_cli`, `foundation_cli` and
  `native_verifiers_cli` (`build --tee`), all run locally through dispatch.
  The moved unit tests run as `vesper-tools` unit tests (17).
- `tests/tool_dispatch.rs` (8 tests):
  - each tool's output and status are byte-identical through `vesper` and
    when run directly;
  - errors keep the `vesper:` format and exit 1;
  - `--help` still lists every tool;
  - unknown commands, including path-like ones, keep the compiler's error;
  - a missing tool binary is named;
  - `build --tee` without `vesper-tee` names it;
  - (Unix) an external subcommand gets its arguments (including empty and
    spaced ones), stdin, stdout, stderr and exit code 7, and a
    non-executable file earlier on `PATH` is skipped;
  - (Unix) a tool killed by `SIGTERM` exits with 143.
- `crates/vesper-tools/tests/binaries.rs`: each binary prints its usage,
  errors use the compiler's format, and `vesper-tee measure` writes the
  expected report.
- `cargo +1.100.0 clippy -p vesper -p vesper-tools --all-targets -D warnings`
  is clean.

## Stage 1 guard

No guarded path changed (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**`, `packages/vesper-compiler/**`).
The bootstrap-stage1 workflow ran on this branch:
[run 44458246976](https://github.com/robot-head/vesper/actions/runs/44458246976),
on `8d3a5ee`, which contains every compiler change in this PR. Both jobs
passed. "Stage 1 reproducibility and cross-backend ratchet" did two clean
builds, checked the fresh lock strictly against `bootstrap/stage1.lock`,
and reported `stage1 cross-backend record ok (11 programs)`. "Stage 0
two-seed build and Path B" also passed. The later commits touch only
dispatch lookup, Bazel wiring and tests.

## Compiler gaps

None. This lane moved Rust code between packages and changed no Vesper
source, so it found no Vesper compiler gap. The only blocker it found was a
Rust-side coupling: `native_coverage` depends on Crucible's probe types,
which keeps `vesper-crucible-seed` in the closure. A minimal reproduction
is `src/native_coverage.rs:66` (`use vesper_crucible::{CmpKind, Probe};`).
Moving those two types into a small shared crate, or into
`vesper-codegen-verified`'s side of the backend, would let
`crucible_commands` become a tool as well.

## What is left

- Move `fuzz` and `verify --refute` (`crucible_commands`) to a tool once
  the coverage types are split from Crucible (SH4).
- Move `semver` and `lint`'s semver report with SH1, and `lsp` with SH5,
  which drops `vesper-serde-seed`.
- Switch each tool to its normal crate when the owner accepts that crate's
  port as at parity. That is a one-line change in
  `crates/vesper-tools/Cargo.toml`.
- Update the seed table in `docs/maintenance/self-hosting-plan.md` (not
  this lane's path) so it shows aegis, bench, grammar, loom-audit and
  loom-solver as out of the compiler.
