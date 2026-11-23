# Self-hosting SH5 lane 13: the other CLI commands, part 1: routing (#1336)

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
| `routing_runs_in_vesper` | the stats line: help, version, an unknown option, an unknown command and a missing tool print from Vesper; `fmt --help`, `check --fuel` and a non-ASCII command do not | ok |
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
| `fmt` | src/fmt_commands.rs (544 lines) | `vesper-fmt` (Vesper port in crates/vesper-fmt/vesper) | this lane, part 2: the layer over the Vesper formatter |
| `highlight` | src/highlight_commands.rs (681) | `vesper-highlight` (port) plus the compiler's check | this lane, later part; needs the checked world (`check`, lowering) from the hybrid's Rust |
| `lint` | src/lint_commands.rs (509) | `vesper::tooling::lint` (compiler) and `vesper-lint` (port) | this lane, later part; needs the checked world |
| `forge` | src/forge_commands.rs (380) | `vesper-forge` (port) over the parser | this lane, later part |
| `ebpf` | src/ebpf_commands.rs (189) | `vesper-ebpf` (port) and `vesper::stack_bound` | this lane, later part |
| `verify --refute` | src/main.rs (runs the `vesper-refute` tool) | `vesper-crucible` | this lane, later part: the same spawn as tool dispatch |
| `shot` | src/shot_commands.rs (415) | `vesper-highlight`'s `shot` (seed, PNG/SVG) | this lane, later part |
| `graph` | src/package_graph.rs | the package graph | this lane, later part |
| `doc` | src/doc_commands.rs | `vesper-doc` | lane 15 (#1338) |
| `test` (and `--doc`) | src/doctest_commands.rs | test runner, doctests | lane 14 (#1337) |
| `lsp` | `vesper::lsp` | LSP | lanes 16 and 17 (#1339, #1340) |
| `verify`, `proof-audit` | src/verify_commands.rs, src/proof_commands.rs | Lean | lane 18 (#1341) |
| top-level `--help` text | `HELP` in src/main.rs | | printed by the host (`hc_help`), shared with the driver's `hd_help`; the routing to it is Vesper |

The tool commands (`attest`, `audit`, `bench`, `cas`, `fuzz`, `grammar`,
`json`, `migrate`, `publish`, `replay`, `resolve`, `sbom`, `semver`,
`simd`, `top`, `traceparent`) are separate `vesper-<tool>` programs since
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

- Part 2: `fmt` over the Vesper formatter, then the layers that need the
  checked world (`highlight`, `lint`, `forge`, `ebpf`, `shot`, `graph`),
  and `verify --refute`'s spawn.
