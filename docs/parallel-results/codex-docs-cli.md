# Wave six lane 14: Codex proof badges, SimPulse doctests and default CLI paths (#256)

Base: `07322e3ba47890e939a48967a1ea8f62fdd7aac1` (origin/main, a descendant of the
required `cda092f`). Branch: `claude/project-thread-gi0t7d`. Claim issue: #256.
Before starting, open issues and PRs were checked; none overlapped (open PRs were
#267 replay witnesses, #165 eBPF builders, #151 Chorus).

## What was built

### 1. `vesper lint` and `vesper test --doc` without a path

`src/tooling/doc/sources.rs` (`vesper::tooling::doc::sources`) finds the package
root, the nearest ancestor of the start directory (inclusive, after
canonicalization) holding a regular `Loom.toml`, else the start directory. It
walks the root for `.ves` files in sorted order without following symlinks,
skipping hidden entries, `target/`, `bazel-*` and nested packages (directories
with their own `Loom.toml`). Files, entries, depth and ancestors are bounded
(1024 / 65,536 / 64 / 256); exceeding a bound, or finding no sources, is an
error, never a silent cut or a vacuous pass.

- `vesper lint [FILE|DIR] [--json] [--fix] [--semver-safe] [--config PATH]`:
  no path or a directory selects package mode. Every file is linted (fixes stay
  per-file atomic); the run fails after all files if any file failed (`failed`)
  or a deny remains (`denied`). JSON: `{schema_version, status, root, manifest,
  files: [{path, report | error}]}`. An explicit file keeps the previous
  single-file output exactly. The package `Loom.toml` is not read as lint policy;
  `--config` stays explicit.
- `vesper test --doc [FILE|DIR] [--json]`: package mode runs every file that has
  Vesper doc fences, lists the others as having no examples (they are not
  checked; the doctest checking profile rejects e.g. closures, which ordinary
  files may use), and fails when no file in the package has examples or any file
  fails. `doctest::has_examples` is the new public pre-check.

### 2. Hermetic SimPulse doctests (§40.2)

A fence `vesper,sim(seed=N,clock,net,disk)` declares a fixed seed and the
simulated devices. Devices are source `linear effect`s `Clock`, `Net`, `Disk`
whose operations must match the contract in `vesper_doc::sim::OPERATIONS`
exactly (all parameters `in u64`, results `u64`, no generics or nested effects):
`Clock::now/sleep`, `Net::send(port, value)/recv(port, timeout_ns)`,
`Disk::write/read/flush`. A body fence gets exactly the declared row
(`fn wrapper() eff [Clock, Net]`); a declaration entry may use only declared
devices. Anything else is a typed failure.

Execution uses the reference evaluator (`Runtime::begin_external`), not the JIT:
`NativeBatch` has no effect support. Each operation reaches
`vesper_doc::sim::SimHost`, which owns one `vesper_simpulse::Simulator` task:
latency parks it on `schedule_io`/`sleep` completions, so virtual time advances
only through the kernel. Network latency is 50..=100 ns from a seed-derived
splitmix64 stream; messages are ordered by arrival then sequence; `recv` times
out with 0; disk writes are volatile until `flush`. Every example runs twice
(`runs_per_example`, minimum 2): a recording and a replay through
`Simulator::replay` of the recorded journal, which rejects the first divergent
transition and any unconsumed suffix. Result, device trace, durable blocks and
journal must be identical. Bounds: 4096 operations, 10 s virtual, 64 messages per
inbox, 1024 blocks, 1,000,000 evaluator fuel.

### 3. Wire layout descriptors and typestate graphs (§40.3)

`vesper_doc::visual` implements the §40.3 records: `WireFieldSpan` with an exact
16-byte little-endian encoding (`to_bytes`), and `LinearEdge` (tags 1 = inout
step, 2 = terminal sink discharge, plus 3 = sink step to another tracked state,
an extension; target 0 = discharge; `resumption_is_linear` from the function's
effect resumption modes). Symbols are interned from 1.

Current Vesper source has no `wire struct` syntax, so `vesper doc --wire
LAYOUTS.json` reads a bounded `vesper-wire-layouts` v1 JSON file
(`u8`/`u16be`/`i32le`/`u64`, `bytes[N]`, `pad[N]`, struct names) and compiles it
with the real Bridge checker `vesper_ffi::Registry`, which rejects implicit
padding. Nested structs flatten to dotted names. Checked source structs also
get rulers from their native eight-byte ABI layout, labelled "native ABI".
Enum variants overlap and are reported as unavailable. HTML gains static SVG
cache-line rulers (one 64-byte row per line, endianness colour, a record table
with each span's 16 hex bytes) and a linear obligation state machine (one row per
checked transition, dead-end states flagged). `is_hot_tile_col` is always 0:
there is no `tiled(N)` source layout yet.

### 4. Proof-backed badges

`vesper doc --proof-audit REPORT.json` (repeatable, up to 16) reads
`vesper proof-audit --json` reports with a strict schema (unknown fields, error
reports, duplicate theorems and verdicts inconsistent with the listed axioms are
rejected). An item's `@proof(Name)` doc lines name its theorems. A badge is
**proved** when every anchor is in an accepted, kernel-replayed audit with no
axioms, **audited** when they all are and some use axioms the audit's explicit
policy allowed, and **unverified** otherwise, with the reason. No flag or prose
grants a badge, and without a report everything is unverified. The badge says
that Lean theorems about a model were kernel-replayed by a local audit; it is not
a theorem about the Rust compiler or the documented Vesper code, and not a
provenance certificate.

## Tests

- `crates/vesper-doc/src/sim.rs` unit tests: same seed reproduces and replays the
  journal; seeds change latency; divergent replays, unconsumed journals and seed
  mismatches are rejected; attribute, device, arity, time and operation limits.
- `crates/vesper-doc/tests/codex.rs`: exact 16-byte record encoding; the example
  wire layout's contiguous hole-free spans; implicit padding and bad inputs
  rejected; §40.3 edge tags; badges from audit reports only (proved, audited,
  rejected, missing); forged/inconsistent audits rejected.
- `src/tooling/doc/sources.rs` unit tests: nearest manifest, skipped trees,
  nested packages, empty/oversized trees, symlinks never followed.
- `tests/codex_cli.rs` (real binary): both no-path forms from a package
  subdirectory, directory and file forms, multi-file failure reporting, empty
  packages; package doctests running compile_fail, native and simulated examples;
  cross-process determinism of the simulation report; seed sensitivity;
  undeclared devices and contract mismatches; a rendered-HTML snapshot
  (`crates/vesper-doc/tests/fixtures/ledger-codex.html`, regenerate with
  `VESPER_UPDATE_SNAPSHOTS=1`); and `badges_from_a_real_kernel_replayed_proof_audit`,
  which compiles a Lean model, runs the real `vesper proof-audit --json` and checks
  proved / audited / unverified badges from its output.
- `tests/doctest_cli.rs`: the help assertion and the old "no file" usage error
  were updated (no path is now valid; two files is the usage error).

The real-audit test needs Lean 4.24.0 via `VESPER_LEAN` and runs on Linux only
(prelude kernel replay is slow on shared macOS/Windows runners). Lean could not
be installed in this cloud session, so locally it skipped; it runs in Linux CI.
The audit fixture `proof-audit-ledger.json` has the real report schema but
placeholder hashes; it drives the snapshot and unit tests, not the real-audit test.

## Checks (local, Linux x86_64)

```sh
cargo fmt --all -- --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test -p vesper-doc
cargo test -p vesper --test codex_cli --test doctest_cli --test doc_cli --test lint_cli --test doc_links_cli
cargo test --workspace --locked
vesper fmt --check crates/vesper-doc/examples/package/src/ledger.ves
```

Results are recorded in the PR description.

## Root patch and other paths

No root manifest change: `vesper-doc` gained `vesper-simpulse` and `vesper-ffi`
(both root-free, already workspace members); `Cargo.lock` gains those two edges.
`tools/bazel/sync_build_files.py` produced no BUILD changes. New test file
`tests/codex_cli.rs` and the two-line `tests/doctest_cli.rs` update sit outside the
listed exclusive paths because they test this lane's CLI behaviour.

README (lane 14 lines only): the Getting Started `lint` / `test --doc` commands
now name `crates/vesper-doc/examples/package`. Without a path at the repository
root (no `Loom.toml`) they would walk every `.ves` file in the repository,
including deliberately rejected fixtures, and fail. The `vesper lint` row no
longer claims `Alloc`-to-`Region` demotion or `egglog` rules, and the `vesper
test` row no longer claims parallel unit tests, `--sim`, `--sim-ui`, `--mutate`
or a measured `<100ms`.

## Remaining gaps

- Other README lines are not lane 14's: `cargo build --release -p vesper-cli`
  (there is no `vesper-cli` package; the CLI is the root package), the `vesper
  doc` row (FieldTree heatmaps, WASM Hoogle and `.olean` linking are not built),
  and the `vesper bench` row (lane 13).
- `src/main.rs` help still shows `vesper <command> <file.ves>`; it is
  coordinator-owned.
- No source `wire struct` / `tiled(N)` syntax, so wire layouts come from JSON
  declarations and no field is a hot tile column.
- SimPulse doctests run on the reference evaluator, not in the shared JIT
  module; devices are a fixed scalar model (no byte buffers, partitions, crashes
  or concurrency between tasks). The scenario-driven model of
  `vesper-sim-session` depends on the root package and cannot be used from
  the facade without a cycle.
- No `<100ms` claim was measured. Badges do not link `.olean` files from a
  store or `~/.vesper/store`; reports are supplied explicitly.
