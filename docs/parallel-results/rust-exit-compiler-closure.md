# SH6 lane 3: shrink the compiler's closure (#1398)

Base: `main` at `ab965f2a` (part 1, #1417); part 2 on top.

## What changed (part 1)

Nine tool commands moved out of the compiler binary into `vesper-<tool>`
executables in `crates/vesper-tools` (the SH0 lane 2 pattern, #1124/#1133).
`vesper <tool>` dispatches to them unchanged: same arguments, streams and exit
status.

| Command | Module (was) | Now | Library it linked |
|---|---|---|---|
| `vesper attest` | `src/attestation_commands.rs` | `vesper-attest` (`src/attest.rs`) | loom-trust, loom-admission, loom-cas |
| `vesper cas` | `src/cas_commands.rs` | `vesper-cas` | loom-cas |
| `vesper fuzz` | `src/crucible_commands.rs` | `vesper-fuzz` (`src/fuzz.rs`) | crucible, migrate, fmt |
| `vesper verify --refute` | `src/crucible_commands.rs` | `vesper-refute` (`verify` forwards to it) | crucible |
| `vesper migrate` | `src/migrate_commands.rs` | `vesper-migrate` | migrate, ffi |
| `vesper replay` | `src/replay_commands.rs` | `vesper-replay` | chrono |
| `vesper semver` | `src/package_commands.rs` | `vesper-semver` | semver |
| `vesper simd` | `src/simd_commands.rs` | `vesper-simd` | simd |
| `vesper top` | `src/top_commands.rs` | `vesper-top` | chrono |
| `vesper traceparent` | `src/trace_commands.rs` | `vesper-traceparent` | beacon |

The root manifest drops the direct dependencies on `vesper-simd`,
`vesper-migrate`, `vesper-loom-trust`, `vesper-loom-admission` and
`vesper-serde` (the last had no use in `src/`; it is still linked through
`vesper-lsp`). `vesper-simd` and `vesper-serde` stay as dev-dependencies for
the CLI tests that check results against them.

`tests/compiler_closure.rs` is the guard: it walks the normal path
dependencies of the root manifest (as `cargo tree -p vesper -e normal`
resolves them, all targets at once) and fails when a workspace crate outside
its `ALLOWED` table enters the closure, or when an allowed crate has left it
(so the table only shrinks). Under Bazel only the root manifest is test data,
so it skips there; macOS and Windows (Cargo) run it.

## Part 2

`vesper highlight` and `vesper shot` moved to `vesper-highlight` and
`vesper-shot`, so `vesper-highlight-seed` (with its `shot` rasterizer)
left the closure. The JetBrains Mono font `shot` embeds is the root
`//:fonts` filegroup, declared as the tools' compile data for Bazel.

## Freed libraries (flippable before Stage 2)

| Crate | Was linked for | Now linked by |
|---|---|---|
| `vesper-simd` | `vesper simd` | `vesper-simd` tool only |
| `vesper-migrate` | `vesper migrate`, the `fuzz --differential` limitation text | `vesper-migrate`, `vesper-fuzz` |
| `vesper-loom-trust` | `vesper attest` | `vesper-attest` |
| `vesper-loom-admission` | `vesper attest` | `vesper-attest` |
| `vesper-highlight` (seed) | `vesper highlight`, `vesper shot` | `vesper-highlight`, `vesper-shot` (part 2) |

These crates no longer need the seed pattern: their lanes (13, 18, 17, 16)
can delete their Rust outright at the flip bar.

No crate was flipped here, so there is no flip-gate verdict; Rust lines
deleted: 0 (the moved command modules are 5,000 lines, moved, not deleted).

## What the compiler still links, and why

From `cargo tree -p vesper -e normal` after part 1 (the `ALLOWED` table in
the guard carries the same reasons):

- Checking and code generation proper: core, syntax, span, diagnostics, ffi,
  fiber-abi, pulse, std-host, forge, pact, pact-binding, lean-syntax,
  loom-calculus (manifests), alloc (regions), tensor (tensor types), beacon
  (trace effects), ebpf (backend), silicon (stack bounds, freestanding).
- Capability verifiers that `vesper build` runs without a flag when a program
  carries `eff [ConstTime]`, `eff [Realtime]` or `eff [DetFloat]`: vault,
  resonance, kinetic (and tensor through kinetic). These are part of checking
  a capability program, not optional tools, so they stay until the backend
  checks are ported (SH4 lane 17, `vesper-backend-checks`) or the lanes flip
  them with the seed pattern.
- Commands still in the compiler binary: `vesper test` (test_runner,
  anvil_sim, anvil_hash: crucible, chrono, simpulse, loom-cas), `doc`,
  `verify`, `fmt`, `lint`, `lsp`, `ebpf`, `forge`, `graph`, `proof-audit`.

## What is left (next parts)

1. `vesper test` stays in the compiler (agreed with SH5 lane 14, #1337):
   the hybrid compiler includes `src/main.rs` and installs its Vesper test
   discovery table there, so `vesper-hybrid test` must run in-process.
   crucible, chrono, simpulse and loom-cas (anvil_hash's verdict cache)
   therefore stay and flip with the seed pattern, unless lane 14 or lane 16
   replaces the cache store.
2. `doc` and `verify` stay in the compiler binary: moving them frees no
   crate (vesper-doc is reached through the LSP and doctests, loom-cas through
   anvil_hash, fmt through LSP actions), and `verify_commands` belongs to
   SH5 lane 18. With part 2, every remaining row of the guard's `ALLOWED`
   table is used by checking, code generation, `vesper test` or `vesper lsp`,
   so this lane closes.
3. semver stays: `vesper build` exports the package interface in its types
   (`src/interface.rs`).
4. Census: part 2 updates the STATUS rows in `scripts/rust-census.py` for
   simd, migrate, loom-trust, loom-admission, highlight and vesper-tools; the
   generated `docs/maintenance/rust-census.md` is left to lane 1's next
   regeneration.

## Compiler gaps

None found: the move is Rust-only dispatch and linkage.
