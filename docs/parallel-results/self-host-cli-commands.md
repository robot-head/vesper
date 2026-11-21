# Self-hosting SH5 lane 13: the other CLI commands, part 1 (#1336)

> Stacks on lane 12's driver, PR #1367 (`claude/project-thread-9cpzfd` at
> `e8199f55`, itself `main` at `439f768` plus the driver). Prompt:
> `docs/agent-prompts/self-host-sh5/13-cli-commands.md`. The
> [manuscript](../manuscript/README.md) is the sole specification; this part
> needs no change to it.

The hybrid compiler's driver now hands every command off the compile path
to `@vesper/driver-commands`, a Vesper package that runs the commands it has
ported and hands the rest back to the Rust CLI. Part 1 ports the top-level
paths (no arguments, `--help`/`-h`, `--version`/`-V`), `vesper traceparent`
over the Vesper port of its engine (`@vesper/beacon`), and `vesper simd`
over the Rust JIT scanner. **Rust stays the default build**: `src/**` is
unchanged and the default `vesper` binary links none of this.

## What changed

| Path | What |
|---|---|
| `packages/vesper-driver-commands/` | `@vesper/driver-commands`: `run(args)` returns the exit code or `delegate()` (-1). The top-level paths; `traceparent` (src/trace_commands.rs): the argument loop, every message, the `--json` reports, the canonical form, all over `@vesper/beacon`'s parser and formatter; `simd` (src/simd_commands.rs): the argument loop with every check in the Rust order, the 64 KiB read loop over `@vesper-std/fs`'s `StdFs` import, the text and JSON outputs. A small JSON writer reproduces serde_json's `to_string_pretty` (sorted `json!` keys, two-space indent, serde_json's string escapes). |
| `packages/vesper-driver-commands/commands.vsys` | The `HybridCommands` import: print, the CLI's help, a few host texts and counts (version, arch, OS, the scanner's metadata), `io::Error`'s `Display`, and the scanner (`hc_simd_compile`, `hc_simd_scan`). |
| `crates/vesper-hybrid/src/commands.rs` | The host side of `HybridCommands`. The scanner and the last error live in a thread-local. |
| `crates/vesper-hybrid/modules/driver/` | The dispatch rows: no arguments, and every command that is not a compile command, go to `run` instead of straight back to Rust. The module imports `HybridCommands` (`// @export vsys HybridCommands rust=crate::commands::host`) and depends on the package. |
| `crates/vesper-hybrid/port/driver.rs` | Regenerated binding: it now keeps the `HybridCommands` symbols linked. |
| `crates/vesper-hybrid/src/main.rs`, `src/hybrid.rs` | `mod commands`; `VESPER_HYBRID_STATS=1` also reports how many times the command layers printed. |
| `crates/vesper-hybrid/tests/self_host_cli_commands.rs` | The differential (below). |
| `crates/vesper-beacon/vesper/src/lib.ves` | **Flagged, shared:** `TraceError` and `SpanContext` are now `pub`, so another package can import the parser. The generated binding is byte-identical (checked with `vesper build --lib --rust-types crate::port --rust-binding` before and after). |
| `BUILD.bazel`, `crates/vesper-beacon/BUILD.bazel`, `crates/vesper-hybrid/BUILD.bazel`, `.github/workflows/hybrid.yml` | **Flagged, shared, additive:** the `driver_commands_package` (with its `.vsys`), beacon's `port_package` made visible, the driver module's new dependency, the test's path binaries, and the workflow's path triggers. |

## Design

- **One entry for every other command.** The driver already reads the
  arguments; it calls `run` with them. `run` matches the command name and
  either runs the command or returns `delegate()`, so each later part adds
  one row to `run` and nothing to the driver.
- **Engines.** Where a Vesper port of the engine can be imported, the layer
  calls it (`traceparent` parses and formats with `@vesper/beacon`, not the
  Rust seed the default CLI links). Otherwise the layer calls the Rust
  engine through `HybridCommands`, as the driver calls its passes.
- **Output order.** Every print goes through Rust's `stdout`/`stderr`
  (`hc_print`), the same streams the delegated Rust CLI uses, and the driver
  flushes stdout before it exits.
- **Before the CLI.** The Rust `main` runs tool dispatch
  (`tool_commands::dispatch`) before `cli()`. It does nothing for an empty
  command line, for a first argument starting with `-`, or for a command the
  compiler knows, which covers every path ported here, so no tool lookup is
  skipped.

## Tests and evidence

| Test | What | Result (Linux, this branch) |
|---|---|---|
| `self_host_cli_commands.rs` `command_layers_match_the_default_compiler` | 183 argument vectors: every vector of `tests/foundation_cli.rs`'s traceparent tests and `tests/simd_cli.rs`, each argument-loop edge (options after errors, `--help` late, `--`, a lone `-`, non-ASCII and control bytes in an echoed option, leading zeros and overflow in `--delimiter`, a missing profile value), 37 headers covering every parser error at each offset (each alone, before and after `--json`), every profile, missing and directory inputs, and two commands handed back. `simd scan input` runs on 9 inputs (0 to 131,089 bytes, across the 64 KiB chunk edge). Each run is in a fresh directory: exit status, stdout, stderr and every file afterwards | 399 runs, 0 differ |
| `ported_commands_run_in_vesper` | the stats line: each ported path prints from Vesper; `fmt --help` and `check --fuel` do not | ok |
| `driver.rs` (lane 12's) | the driver's own differential, rerun with the new dispatch | 3 tests pass |

**Stage 1 guard.** No file under `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt`, `lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`
changed, and `src/**` is untouched, so the default binary is unchanged. The
`bootstrap-stage1` workflow runs on the branch once #1367 has merged and
the branch can merge (the CI rule).

## Numbers

Process wall time (debug builds, Linux x86_64, 60 runs per binary
interleaved ABBA, medians; `big.bin` is 1 MB):

| Command | default | hybrid | ratio |
|---|---|---|---|
| `--version` | 6.27 ms | 6.32 ms | 1.008x |
| `traceparent <valid> --json` | 6.72 ms | 6.35 ms | 0.946x |
| `traceparent bad --json` | 6.33 ms | 6.18 ms | 0.976x |
| `simd scan big.bin --delimiter 10 --json` | 22.13 ms | 21.70 ms | 0.981x |
| `simd scan big.bin --delimiter 10` | 22.44 ms | 22.18 ms | 0.989x |
| `fmt --help` (handed back to the Rust CLI) | 6.33 ms | 6.62 ms | 1.046x |

All within noise of the default CLI. The differential's 399 runs took
3.93 s on the default compiler and 3.82 s on the hybrid (0.970x).
`cargo +1.100.0 clippy -p vesper-hybrid --all-targets -- -D warnings` is
clean.

## Commands still in Rust

| Command | Rust layer | Engine | Owner or blocker |
|---|---|---|---|
| `fmt` | src/fmt_commands.rs (544 lines) | `vesper-fmt` (Vesper port in crates/vesper-fmt/vesper) | this lane, part 2: the layer over the Vesper formatter |
| `highlight` | src/highlight_commands.rs (681) | `vesper-highlight` (port) plus the compiler's check | this lane, later part; needs the checked world (`check`, lowering) from the hybrid's Rust |
| `lint` | src/lint_commands.rs (509) | `vesper::tooling::lint` (compiler) and `vesper-lint` (port) | this lane, later part; needs the checked world |
| `forge` | src/forge_commands.rs (380) | `vesper-forge` (port) over the parser | this lane, later part |
| `ebpf` | src/ebpf_commands.rs (189) | `vesper-ebpf` (port) and `vesper::stack_bound` | this lane, later part |
| `fuzz`, `verify --refute` | src/crucible_commands.rs (1,117) | `vesper-crucible` (port) over the evaluator | this lane, later part; needs the evaluator through the host |
| `migrate` | src/migrate_commands.rs (173) | `vesper-migrate` (Rust only) | this lane, later part: layer in Vesper, engine through the host |
| `replay`, `top` | src/replay_commands.rs, src/top_commands.rs (1,327) | `vesper-chrono`, `vesper-pulse` (Rust only) | this lane, later part: engine through the host |
| `attest`, `cas` | src/attestation_commands.rs, src/cas_commands.rs (450) | `vesper-loom-trust`, `vesper-loom-admission`, `vesper-loom-cas` (Rust only) | this lane, later part: engine through the host |
| `shot` | src/shot_commands.rs (415) | `vesper-highlight`'s `shot` (seed, PNG/SVG) | this lane, later part |
| `graph`, `semver` | src/package_graph.rs, src/package_commands.rs | the package graph; `vesper-semver` (port) | the plan lists src/package_commands.rs under lane 12 (#1335); taken here if lane 12 leaves it |
| tool dispatch (`audit`, `bench`, `grammar`, `json`, `publish`, `resolve`, `sbom`, any `vesper-<name>`) | src/tool_commands.rs | process spawn | this lane, later part: needs `current_exe`, the executable bit and the signal exit code in `@vesper-std/process` |
| `doc` | src/doc_commands.rs | `vesper-doc` | lane 15 (#1338) |
| `test` (and `--doc`) | src/doctest_commands.rs | test runner, doctests | lane 14 (#1337) |
| `lsp` | `vesper::lsp` | LSP | lanes 16 and 17 (#1339, #1340) |
| `verify`, `proof-audit` | src/verify_commands.rs, src/proof_commands.rs | Lean | lane 18 (#1341) |
| top-level `--help` text | `HELP` in src/main.rs | | printed by the host (`hc_help`), shared with the driver's `hd_help`; the routing to it is Vesper |

## Compiler gaps

1. **A `handle` whose result is an enum has no native layout.** Building the
   library fails with `Codegen: native layout is not yet defined for _`:

   ```vesper
   let parsed: Result[SpanContext, TraceError] = handle Fail[TraceError] in {
       Result::Ok(parse_traceparent_55(in raw))
   } with {
       return v => v,
       raise(e, _) => Result::Err(e),
   };
   ```

   `vesper check` accepts it. Workaround: assign the value inside the body
   and return the error as a word (`trace_error_word`).
2. **A handler clause cannot capture an owned buffer** (E0404, "handler
   clause capture 'm' must be owned copy"), so a message cannot be built in
   the `raise` clause. Same workaround.
3. **`while i < n {`** does not parse (SH5 lane 4, #1327): `loop { if i >= n
   { break; } ... }`.
4. **A view blocks a move to the end of its block** (E1531): `header = a;`
   while `let s = a.as_slice();` is in scope; copy instead.
5. **Port types are private.** `@vesper/beacon` kept `TraceError` and
   `SpanContext` private (E0112 from a dependent package); a port meant to
   be imported needs `pub` on the types of its public functions. Fixed here
   for beacon (binding unchanged).
6. Lane 12's gap 3 (no byte-string comparison or `match`) applies to every
   command table: `run` is a chain of `eq` calls.

## What is left

- Part 2: `fmt` over the Vesper formatter, then the layers whose engines
  stay Rust (`migrate`, `replay`, `top`, `attest`, `cas`) through
  `HybridCommands`, then the commands that need the checked world
  (`highlight`, `lint`, `forge`, `ebpf`, `fuzz`), and tool dispatch.
- `simd` reads in 64 KiB chunks like Rust, but does not check the
  `u64` overflow of the byte and match counts (`input length exceeds u64`),
  which no real file reaches.
