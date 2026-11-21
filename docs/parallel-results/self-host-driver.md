# Self-hosting SH5 lane 12: the driver (#1335)

> Base: `main` at `bbda89e` (2026-11-21, the CI trim, #1322). Prompt:
> `docs/agent-prompts/self-host-sh5/12-driver.md`. The
> [manuscript](../manuscript/README.md) is the sole specification; this lane
> needs no change to it.

The compile path of the `vesper` CLI (`check`, `run`, `jit`, `build`,
`emit`) now runs in Vesper in the hybrid compiler: the hybrid binary's
`main` is the Vesper driver. Rust stays the default build. The root
package's `src/main.rs` is unchanged; the default `vesper` binary does not
link any of this.

## What changed

| Path | What |
|---|---|
| `packages/vesper-driver/` | `@vesper/driver` (pure, `Fail` and `Alloc` only): the argument loop and the cross-option checks of `cli()`, every message byte for byte, in the Rust CLI's order (each argument in turn, then the checks after the loop); the checks that need to know the input is a package (`--emit cst`, `--target bpf`); `LibFlags::job`'s checks; `Path::with_extension` for the object path; the options record the Rust passes read. Test entries `parse_text` and `with_extension_text`. |
| `crates/vesper-hybrid/modules/driver/` | `@vesper/hybrid-driver`, the hybrid's `main_code`: reads the arguments with `@vesper-std/env`, parses them with `@vesper/driver`, then runs `cli()`'s pass sequence: load, eBPF build, parse, `--emit cst`, syntax errors, the `.vsys` path, check, the export path, `check`'s output, interface, optimize, run, jit, the verified and wasm builds, the native object, the link and every `emit` format. Each pass is one call of `hd_step` on the `HybridDriver` import. The native link runs `<linker> <object> -o <path> -lm` through `@vesper-std/process`, and the object is removed through `@vesper-std/fs`. Every other command, and arguments that are not UTF-8 (the Rust CLI's `env::args()` panics on them), go back to the Rust CLI. |
| `crates/vesper-hybrid/src/driver.rs` | The host side: `hd_print`, `hd_help`, `hd_session`/`hd_free` (a linear `Session` holding what `cli()` keeps in locals), `hd_step` (27 passes, each the Rust CLI's own code calling the included `src/main.rs` helpers; panics are caught and become exit 101 as in Rust), `hd_text` (copy-out of a pass's message or result). The C entry spawns the CLI thread with the Rust CLI's name and stack. |
| `crates/vesper-hybrid/src/main.rs` | `#![no_main]` and an `export_name = "main"` entry that calls the driver; the included CLI's `fn main` is an ordinary function the driver calls for the commands it hands back. SIGPIPE is set to ignore by hand, since the Rust start-up that did it no longer runs. |
| `crates/vesper-hybrid/src/hybrid.rs` | `VESPER_HYBRID_STATS=1` also reports how many passes the driver ran. |
| `crates/vesper-hybrid/build.rs`, `port/driver.rs` | The `hybrid_driver` module and its committed binding. |
| `tests/self_host_driver.rs` | The parser against the Rust CLI (below). |
| `crates/vesper-hybrid/tests/driver.rs` | The hybrid against the default compiler end to end (below). |
| `crates/vesper-hybrid/cli-suites.sh` | Runs every `tests/*cli*.rs` suite of the root package with the hybrid binary in place of `target/debug/vesper` (the suites' `CARGO_BIN_EXE_vesper` is fixed at compile time). |
| `.github/workflows/hybrid.yml` | Triggers on `packages/vesper-driver/**`; a new step runs `cli-suites.sh` on main, nightly and manual runs (Linux, macOS and Windows), not on pull requests (it builds every CLI suite, #1321). |
| `tools/bazel/vesper_port.bzl`, `BUILD.bazel`, `crates/vesper-hybrid/BUILD.bazel` | `vesper_package` gains a `vsys` attribute (the `.vsys` files a manifest names, #1150); packages for `@vesper-std/fs`, `process`, `env`, `fmt` and `@vesper/driver`; the `hybrid_driver` module and its dependencies; test data for `self_host_driver` and the hybrid's `driver` test. Flagged: these are shared files, and the edits are additive. |

## Design

- **Why `no_main`.** The hybrid includes `src/main.rs` unchanged, and that
  file defines `fn main`. Making the Vesper driver the entry without
  touching it means the hybrid's crate is `#![no_main]` and exports its
  own C `main`; Rust's `fn main` is then a plain function, called for every
  command lane 13 has not ported yet. The standard library still finds the
  arguments (`args_os` on glibc, `_NSGetArgv` on macOS, `GetCommandLineW` on
  Windows).
- **Passes through an import, not callbacks.** The export ABI has no Rust
  callbacks into Vesper, but a library may import C functions that its
  linking crate provides (`// @export vsys direct rust=<path>`). The
  `HybridDriver` library is declared inline in the module's root, so it
  needs no `.vsys` file. One generic `hd_step(session, pass, arg)` keeps
  the import small: a pass returns a count or 0, `-1` with its message in
  the session, or `-2` when it panicked.
- **What is Vesper and what is Rust.** Vesper decides: which command, every
  option and its error, the order of the passes, which pass ends the
  command, the messages built from counts (`N syntax error(s)`,
  `N semantic error(s)`, `N package error(s)`), `check`'s output, the
  default output names, the object path, the linker command line, its
  failure messages, the object's removal and `built <path>`. Rust does the
  compiler work on Rust values (the assembled package, the parse, the
  checked world), which stay in the session.
- **`with_extension`.** Rust's `Path::with_extension` drops the old
  extension's length from the end of the whole path, then runs
  `set_extension` on the rest, so `"..x"` gives `".."` and `"dir.d/"`
  gives `"dir.vesper.o"`. The port does the same.

## Tests and evidence

| Test | What | Result (Linux, this branch) |
|---|---|---|
| `tests/self_host_driver.rs` `argument_errors_match_the_rust_cli` | 5,516 argument vectors: each compile command with each of 67 options alone, before and after the input, every ordered pair of options, and the CLI suites' option sets. A Vesper error must be the Rust CLI's whole stderr (`vesper: <message>`) with exit 1; `--help` its stdout; an accepted vector must get the Rust CLI as far as reading the input. | 5,516 agree |
| `package_checks_match_the_rust_cli` | `--emit cst` and `--target bpf` on a real package directory | agree |
| `records_carry_the_options` | the record's command, fuel (`+7`, `u64::MAX`, the last of two) and input | ok |
| `object_paths_match_path_with_extension` | 34 paths against `Path::with_extension` | agree |
| `crates/vesper-hybrid/tests/driver.rs` `driver_matches_the_default_compiler` | 19 inputs (programs for each error and build path, `examples/`, three package directories, a missing file) x 35 option sets (every compile command and `emit` format, errors, `build` to a missing directory, a missing linker, `false` as the linker, `--lib`, verifiers): exit status, stdout, stderr and every file written, and the object files a failed link leaves behind | 665 runs, 0 differ |
| `compile_commands_run_in_the_vesper_driver` | the stats line: `check` and `emit` run their passes in the driver, an argument error runs none, `fmt` none | ok |
| `help_and_version_match` | `--help`, `--version`, no arguments, `check --help`, an unknown command | ok |
| `crates/vesper-hybrid/cli-suites.sh` | all 52 `tests/*cli*.rs` suites with the hybrid as `vesper` | 52 suites pass (10 need the `vesper-<tool>` binaries; the script builds them) |
| `crates/vesper-hybrid/tests/differential.rs` | the existing corpus differential (`check --json`, `emit ir`, `emit object` over the bootstrap corpus, `examples/` and `tests/`), now through the driver | 538 files, 1,614 runs, 0 differ |

macOS and Windows: the Hybrid compiler workflow runs the crate's tests and
`cli-suites.sh` there on main, nightly and manual runs.

**Stage 1 guard.** No file under `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt`, `lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`
changed, and the default binary is unchanged (`src/**` untouched). The
`bootstrap-stage1` workflow run on the branch is linked from the PR.

## Numbers

The driver's own time, as process wall time (debug builds, Linux x86_64,
60 runs per binary interleaved ABBA, medians):

| Command | default | hybrid | ratio |
|---|---|---|---|
| `check --fuel` (argument error: the driver only) | 5.37 ms | 5.29 ms | 0.985x |
| `check --verify=bogus x.ves` (argument error) | 6.10 ms | 5.79 ms | 0.950x |
| `check examples/counter.ves` | 11.71 ms | 12.42 ms | 1.061x |
| `emit --emit ir examples/counter.ves` | 32.21 ms | 33.76 ms | 1.048x |
| `fmt --help` (handed back to the Rust CLI) | 4.90 ms | 5.14 ms | 1.049x |

The argument paths, which run nothing but the driver, are within noise.
`check` and `emit` also run the parse, layout and IR-check swaps of
earlier lanes, so their few percent is not the driver's. The end-to-end
differential above took 16.38 s on the default compiler and 16.44 s on the
hybrid (1.004x) over 665 runs.

## Compiler gaps

1. **No callbacks across the export ABI.** Rust cannot hand Vesper a
   function to call, so the passes are an import the hybrid implements
   (`HybridDriver`), and their state is a Rust `Session` the driver only
   passes on. Each pass that later moves to Vesper becomes a Vesper call in
   `drive` instead of a `hd_step`. SH5 lane 7 (callback follow-ups) may make
   a typed pass table possible.
2. **A method on a call's result.** `line.extend(in value(in o, k).as_slice());`
   is E0202 "unknown field" (and E0206); the value must be bound first:
   `let v = value(in o, k); line.extend(in v.as_slice());`.
3. **No byte-string comparison.** Matching an argument against `"check"`
   is a byte loop (`eq` in lib.ves). `Slice[u8] == Str`, or `match` on byte
   strings, would make the option table one `match`. (Same need as the
   std I/O lane's `cmp_bytes` gap.)
4. **Windows path prefixes.** `with_extension` follows Rust for `/` and
   `\` separators but not for `C:` or `\\?\` prefixes (`C:a.out`). The CLI
   suites do not reach them.
5. **Kind-only I/O errors.** A spawn or remove error with no OS error code
   (only from inputs the CLI cannot receive, such as a NUL byte) prints the
   kind's generic text, not Rust's custom message.
6. **JIT call cost in tests.** Through `check_harness::Entry::words`, one
   call of `parse_text` costs about 2.5 ms in the debug JIT, so the
   5,516-vector test spends ~14 s there. The AOT driver in the hybrid costs
   nothing measurable (above). Not investigated.

## What is left

- Lane 13 (#1336) ports the other commands onto this driver's dispatch; the
  top-level help, version and command routing are still the Rust CLI's.
- The pass bodies in `driver.rs` repeat `cli()`'s leaves (about 300 lines)
  because `src/main.rs` has no swap point for them. The differentials
  catch drift; a swap point in `src/main.rs` (or moving the leaves into the
  library) would remove the copy.
- As SH4 and SH5 move passes to Vesper (parse, check, lowering), `drive`
  calls them directly and the `Session` shrinks.
