# Self-hosting SH5 lane 13: the other CLI commands (#1336)

Part 1 (routing, PR #1372) is below; part 2 (`vesper fmt`) follows it.

## Part 1: routing

> Stacks on lane 12's driver, PR #1367 (`claude/project-thread-9cpzfd` at
> `12fa0969`, which merged `main` with #1371 and #1398's tool split). Prompt:
> `docs/agent-prompts/self-host-sh5/13-cli-commands.md`. The
> [manuscript](../manuscript/README.md) is the sole specification; this part
> needs no change to it.

The hybrid compiler's driver now hands every command off the compile path
to `@vesper/driver-commands`, a Vesper package that runs the routing in
front of the command layers and hands the compiler's own commands back to
the Rust CLI. Part 1 ports that routing: no arguments, `--help`/`-h`,
`--version`/`-V`, the unknown-command error, and the whole tool dispatch of
`src/tool_commands.rs` (`vesper <tool>` finds `vesper-<tool>` beside the
executable, in Bazel's `../crates/vesper-tools/bin`, or on `PATH`, runs it
with the same arguments and streams, and exits with its status or
128 + its signal). **Rust stays the default build**: `src/**` is unchanged
and the default `vesper` binary links none of this.

The first draft of this part ported `traceparent` and `simd`; SH6 lane 3
(#1398) then moved those and most other tool commands out of the compiler
into `vesper-<tool>` binaries (crates/vesper-tools), so they no longer run
in the compiler at all, and the part was rescoped to the dispatch that now
sits in front of them.

## What changed

| Path | What |
|---|---|
| `packages/vesper-driver-commands/` | `@vesper/driver-commands`: `run(args)` returns the exit code or `delegate()` (-1). The top-level paths and messages; `COMPILER_COMMANDS` and `TOOLS` as in src/main.rs and src/tool_commands.rs; tool-name validation; `locate` (the executable's directory, its canonical directory when different, each with the Bazel tree, then every `PATH` entry, split as `env::split_paths` does, including Windows quotes); `missing`; the spawn over `@vesper-std/process` and the exit code. A command with a non-ASCII byte goes back to Rust, which decides (and panics on non-UTF-8, as before). |
| `packages/vesper-driver-commands/commands.vsys` | The `HybridCommands` import: print, the CLI's help, host texts (version, arch, OS, `current_exe()`'s directory and its canonical directory), `io::Error`'s `Display`, and the executable check (a regular file with an execute bit on Unix). |
| `crates/vesper-hybrid/src/commands.rs` | The host side of `HybridCommands`. |
| `crates/vesper-hybrid/modules/driver/` | The dispatch rows: no arguments, and every command that is not a compile command, go to `run` instead of straight back to Rust. The module imports `HybridCommands` (`// @export vsys HybridCommands rust=crate::commands::host`) and depends on the package. |
| `crates/vesper-hybrid/port/driver.rs` | Regenerated binding: it now keeps the `HybridCommands` symbols linked. |
| `crates/vesper-hybrid/src/main.rs`, `src/hybrid.rs` | `mod commands`; `VESPER_HYBRID_STATS=1` also reports how many times the command layers printed. |
| `crates/vesper-hybrid/tests/self_host_cli_commands.rs` | The differential (below). |
| `BUILD.bazel`, `crates/vesper-hybrid/BUILD.bazel`, `.github/workflows/hybrid.yml` | **Flagged, shared, additive:** the `driver_commands_package` (with its `.vsys`), the driver module's new dependency, the test's path binaries, and the workflow's path trigger. |

## Design

- **One entry for every other command.** The driver already reads the
  arguments; it calls `run` with them. `run` either finishes the command or
  returns `delegate()`, so each later part adds one row to `run` and
  nothing to the driver.
- **Same order as Rust.** The Rust `main` runs `tool_commands::dispatch`
  before `cli()`; `run` makes the same decisions in the same order (an
  option or a compiler command skips the tool lookup; a listed tool always
  runs or fails with `missing`; any other valid name runs only if
  `vesper-<name>` is found, else the CLI's unknown-command error).
- **Output.** Every print goes through Rust's `stdout`/`stderr`
  (`hc_print`, `print!`/`eprint!`), the same streams the delegated Rust CLI
  uses; a failed write is std's panic and exit 101, as in Rust.

## Tests and evidence

| Test | What | Result (Linux, this branch) |
|---|---|---|
| `self_host_cli_commands.rs` `routing_matches_the_default_compiler` | Both compilers are hard linked side by side into 3 layouts (every tool beside them plus an external one; no tools; Bazel's `bin/` + `crates/vesper-tools/bin/` tree) and run under 8 `PATH`s (unset, empty, one entry, two entries, a trailing `/`, leading and doubled empty entries meaning the working directory, a relative entry; on Windows quoted entries with `;`). 80 argument vectors: every top-level option, unknown options and names (empty, `$`, space, `/`, upper case, non-ASCII, DEL, non-UTF-8), compiler commands handed back, every `TOOLS` entry, external tools on `PATH`, a non-executable shadow before an executable one, an owner-only executable, a directory, a script killed by SIGTERM and SIGKILL, a tool that writes a file, spawn errors (no `#!`, a missing interpreter), and non-UTF-8 tool arguments. A tool is a hard link of the default compiler (so every host can run one) or, on Unix, a shell script. Each run is in a fresh directory: exit status, signal, stdout, stderr and every file afterwards | 1,920 runs, 0 differ |
| `routing_runs_in_vesper` | the stats line: help, version, an unknown option, an unknown command and a missing tool print from Vesper; `check --fuel` and a non-ASCII command do not (since part 2, `fmt` prints from Vesper too) | ok |
| `full_stdout_fails_as_in_rust` (Linux) | stdout on `/dev/full`: help and version panic with std's `failed printing to stdout` and exit 101, as the Rust CLI does | ok |
| `driver.rs` (lane 12's) | the driver's own differential, rerun with the new dispatch | 3 tests pass |

**Stage 1 guard.** No file under `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt`, `lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`
changed, and `src/**` is untouched, so the default binary is unchanged. The
`bootstrap-stage1` workflow runs on the branch once #1367 has merged and
the branch can merge (the CI rule).

## Numbers

The differential's 1,920 runs took 11.62 s on the default compiler and
12.56 s on the hybrid (1.081x, debug builds, Linux x86_64). The difference
is process start-up of the larger binary (the debug hybrid is 890 MB to the
default's 195 MB, since it links the Vesper modules and their debug info),
not the routing, which makes at most a few `stat` calls and one spawn.
`cargo +1.100.0 clippy -p vesper-hybrid --all-targets -- -D warnings` is
clean (its only output is Cargo's unused-dependency notes on
`crates/vesper-hybrid/Cargo.toml` left by the tool split, which this part
does not touch).

## Commands still in Rust

| Command | Rust layer | Engine | Owner or blocker |
|---|---|---|---|
| `fmt` (one file's read, format and replace) | `fmt_commands::format_file` | `vesper-fmt` (port) | CLI/walk/JSON in Vesper (#1644); bounded no-follow read and checked atomic replace in Vesper over `@vesper-std/fs` (#1996 part 3b); the formatter is @vesper/fmt, the Vesper port of vesper_fmt (#1996 part 3d) |
| `lint` | `HybridCommands::hc_lint_run` | `vesper::tooling::lint` / `vesper-lint` (port) | CLI option parsing, validation, and `--help` in Vesper (`lint.ves`, #1730) |
| `forge` | `HybridCommands::hc_forge_run` | `vesper-forge` (port) | CLI option parsing, validation, and `--help` in Vesper (`forge.ves`, #1660) |
| `ebpf` | `HybridCommands::hc_ebpf_check` | `vesper-ebpf` (port) / `vesper::stack_bound` | CLI option parsing, validation, and `--help` in Vesper (`ebpf.ves`, #1653) |
| `verify --refute` | `run_tool` (`vesper-refute`) | `vesper-crucible` | Routed in Vesper (`lib.ves`, #1648) |
| `doc` | `HybridCommands::hc_doc_run` | `vesper-doc` (port) | CLI option parsing, validation, `store-olean`, and `--help` in Vesper (`doc.ves`, #1738) |
| `test` (and `--doc`) | `HybridCommands::hc_test_run` | test runner, doctests | CLI option parsing, validation, and `--help` in Vesper (`test.ves`, #1745) |
| `lsp` | `HybridCommands::hc_lsp_serve` | `vesper::lsp` (lanes 16 and 17: #1339, #1340) | CLI routing, option validation, and `--help` in Vesper (`lsp.ves`, #1790 / #1809) |
| `proof-audit` | `HybridCommands::hc_proof_audit_run` | Lean | CLI option parsing, validation, and `--help` in Vesper (`proof_audit.ves`, #1764) |
| `verify` | `HybridCommands::hc_verify_run` | Lean | CLI option parsing, validation, and `--help` in Vesper (`verify.ves`, #1784) |
| top-level `--help` text | `HELP` in src/main.rs | | printed by the host (`hc_help`), shared with the driver's `hd_help`; the routing to it is Vesper |

`graph` runs wholly in Vesper since #1996: `graph.ves` parses the options
(#1669) and reads the file over `@vesper-std/fs`, and @vesper/loom-calculus
(`lock_json.ves`, `lock_words.ves`, `lock_graph.ves`) reads the Loom.lock
JSON with serde_json's exact errors, validates it with the ported solver,
projects it and renders the text, JSON or DOT report of
`src/package_graph.rs`. `hc_graph_run` and `hc_graph_err` are gone. The
port's differentials (`tests/lock_json.rs`, 5,625 inputs, and
`tests/lock_graph.rs`, 10,956 runs over every focus, direction and format)
and the CLI cases of `graph_matches_the_default_compiler` show 0 diffs.

The tool commands (`attest`, `audit`, `bench`, `cas`, `fuzz`, `grammar`,
`highlight`, `json`, `migrate`, `publish`, `replay`, `resolve`, `sbom`,
`semver`, `shot`, `simd`, `top`, `traceparent`) are separate `vesper-<tool>` programs since
#1398. The hybrid reaches them through the Vesper dispatch above; porting
each tool is SH6's Rust-exit work, not the compiler CLI's.

## Compiler gaps

1. **`u64` arithmetic raises `Fail`.** `k * 2` and `k * 2 + 1` on `u64`
   are unhandled `Fail` effects (E0401) in a function without `Fail` in its
   row; the free functions `wrapping_mul(k, 2)` and `wrapping_add` work,
   but the method forms (`k.wrapping_mul(2)`) are E0202 "unknown field".
2. **`while i < n {`** does not parse (SH5 lane 4, #1327): `loop { if i >= n
   { break; } ... }`.
3. Lane 12's gap 3 (no byte-string comparison or `match`) applies to every
   command table: `compiler_command` and `tool` are chains of `eq` calls.
4. From the first draft (traceparent, now dropped): a `handle` whose result
   is an enum fails codegen with `native layout is not yet defined for _`
   although `vesper check` accepts it; a handler clause cannot capture an
   owned buffer (E0404); and a port's types must be `pub` to be imported
   (E0112).

## What is left

- Part 2 is done (below). Next, the layers that need the
  checked world (`highlight`, `lint`, `forge`, `ebpf`, `shot`),
  and `verify --refute`'s spawn.

## Part 2: `vesper fmt`

The hybrid now runs `vesper fmt` in `@vesper/driver-commands` (`fmt` in
packages/vesper-driver-commands/src/lib.ves) instead of handing it back to
the Rust CLI. **Rust stays the default build**: the only `src/**` change is
`fmt_commands::format_file` becoming `pub(crate)`, so the host can call it.

**What moved to Vesper.**

- **Options.** `--help`/`-h` and `--json` before `--`, `--check`,
  `--recovery`, `--`, and the `unknown fmt option` error.
- **Paths.** The single-file case and the workspace case.
- **The walk** (`walk`), over @vesper-std/fs's directory reads (`StdFs`), with its own byte-order merge sort:
  - files and directories in byte order, without following symbolic links;
  - hidden, `target` and `bazel-*` directories skipped;
  - `Path::extension() == "ves"` decided the way Rust decides it;
  - the 200,000-entry limit, checked while each directory is read, so an
    oversized directory is never fully loaded (Codex review);
  - on Windows, a listed name holding U+FFFD goes back to the Rust CLI before
    anything is formatted: the host lists Windows names lossily, so the name
    may stand for an unpaired surrogate that Rust's `OsString` keeps (Codex
    review);
  - the `{dir}: {io error}` message.
- **Output.** Every status line and the summary.
- **JSON.** Both JSON reports and the error report, written as serde_json
  writes them: keys sorted, compact, with its string escapes.
- **Exit codes and errors.** Every exit code and error message.
- **Non-UTF-8 arguments.** Any argument that is not UTF-8 goes back to
  Rust, which panics on it as before.

**What stays on the host.** Before #1996 part 3b, `hc_fmt_file` ran all
of `fmt_commands::format_file`. Now the Vesper layer does one file's
bounded read and the checked atomic replace itself, over
`@vesper-std/fs` (`read_nofollow_limit`, `create_staged`, `same_file`,
`rename_path`, `remove_staged`):

- the read: `O_NOFOLLOW | O_NONBLOCK`, regular file, 256 KiB, UTF-8;
- a 0600 stage file;
- the permission copy;
- `sync_all`;
- a re-read that checks dev, inode, length and mtime;
- the rename.

Until #1996 part 3d the formatter stayed on the host: `hc_fmt_source`
ran `fmt_commands::format_text` on the text that was read, and
`hc_fmt_output` copied the output out. Since part 3d, `fmt_run` calls
@vesper/fmt (`format` and `format_recovery` with `Limits::default()`),
whose port matches vesper_fmt on every repository file, damaged copies and
limit sweeps (#2090, #2113, #2130). It also writes the error text
(`cannot format source: <Error as Debug>`), and `hc_fmt_*` is gone.

| Path | What |
|---|---|
| `packages/vesper-driver-commands/src/lib.ves` | `fmt`, the dispatch row, the walk, the JSON writer, the UTF-8 check |
| `packages/vesper-driver-commands/Loom.toml` | depends on @vesper/fmt (crates/vesper-fmt/vesper) |
| `crates/vesper-hybrid/port/driver.rs` | regenerated binding (keeps the two symbols linked) |
| `src/fmt_commands.rs` | `format_text` (the formatter of one file's text) is `pub(crate)` |
| `crates/vesper-hybrid/tests/self_host_cli_commands.rs` | `fmt_matches_the_default_compiler`, `fmt_walk_limit_matches_the_default_compiler` (Linux: 200,000 entries walk, 200,001 give the bounded-walk error, on both compilers); `routing_runs_in_vesper` now expects `fmt` to print from Vesper |

**The differential.** `fmt_matches_the_default_compiler` builds a fresh
fixture for each compiler and each argument vector, then compares exit
status, stdout, stderr and every file left, with its contents and mode
(symbolic links by target). The fixture covers:

- canonical, noncanonical and damaged sources;
- a recovery source;
- a file one byte over 256 KiB, and one exactly at it;
- a file that is not UTF-8;
- `..ves`, `.ves`, `a.ves.bak` and `.txt` names;
- nested and sibling directories;
- hidden, `target`, `bazel-out` and `targets` directories;
- an empty directory;
- on Unix: file, directory and dangling symbolic links, a name with `"`,
  `\`, a tab, U+0001 and `é`, a name that is not UTF-8, and a read-only
  file.

The 68 argument vectors cover every option and its position around `--`,
help precedence, unknown options, `-`, an empty path, a single file, a
repeated file, a mix of directories and files, `sub/` and `sub//`, missing
files and directories, and `--check`, `--json` and `--recovery` together.

**Result (Linux x86_64, debug builds):** 68 vectors, **0 differ**. Default
0.74 to 0.92 s, hybrid 0.69 to 0.70 s, which is 0.75x to 0.94x of Rust's
time. The whole test file passes, and the routing differential still shows
0 differences on 2,040 runs.

**Compiler gap found.** A `handle` that returns `Result[Entries, IoError]`
fails codegen with `native layout is not yet defined for _`. This is gap 4
of part 1, now hit with a generic enum. `try_list` works around it by
writing the listing to an `inout` parameter and returning the error word.
