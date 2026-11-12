# Hermetic WASI build/macro host (design §§8.2, 16.4)

Lane: Forge/Loom WASI sandbox. Branch: `claude/project-thread-chktjf`, based on `origin/work` at `72493b8`.
Owned paths: `crates/vesper-loom-sandbox/**` and this file. No root workspace, lockfile, CLI, CI, syntax, checker, IR, or codegen file is changed.

## What is implemented

`crates/vesper-loom-sandbox` is a standalone library (temporary `[workspace]` table) embedding Wasmtime 50 to run `wasm32-wasip2` **components** that export `vesper:sandbox/step@0.1.0` ([`wit/sandbox.wit`](../../crates/vesper-loom-sandbox/wit/sandbox.wit)). One `SandboxHost` (engine + linker) compiles and runs many steps; every run gets a fresh `Store`, `WasiCtx` and limiter.

| Requirement | Implementation | Evidence (executed against the real fixture component) |
| --- | --- | --- |
| Fuel limit | `Config::consume_fuel`, `Store::set_fuel(limits.fuel)`; `Trap::OutOfFuel` → `SandboxError::FuelExhausted`. Consumed fuel is reported. | `infinite_loop_exhausts_fuel`, `small_fuel_budget_stops_even_a_trivial_step` |
| Memory limit | Custom `ResourceLimiter` with an **aggregate** byte budget over all linear memories (and tables), plus instance/memory/table counts. Grows past a memory's own declared maximum are refused without being charged, and an approved grow that Wasmtime then fails is rolled back (`memory_grow_failed`/`table_grow_failed`). A denied grow is recorded; the resulting trap (or instantiation failure) becomes `MemoryLimitExceeded { limit, requested }`. Peak memory is reported. | `unbounded_allocation_hits_memory_limit`, `memory_limit_below_initial_size_fails_instantiation`, unit `limiter_aggregates_across_memories`, `limiter_ignores_grows_past_declared_maximum_and_rolls_back_failures` |
| Explicit read-only inputs | Each `InputMount { host_path, guest_path }` is a `FsPerms::ReadOnly` preopen. Guest paths must be absolute, normalized, non-root, non-nested, and disjoint from `/out`. Host paths are canonicalized and must be directories. | `build_step_reads_input_and_writes_output`, `input_mount_lists_only_its_own_contents`, policy unit tests |
| Dedicated writable output | One `FsPerms::ReadWrite` preopen at `/out`. The host directory must exist, be **empty** before the run, and not contain or be contained in any input. After the run the host scans it: regular files only (symlinks/special files rejected), bounded entry count and total bytes. | `output_directory_must_start_empty`, `output_byte_limit_is_enforced_after_run`, `output_must_be_dedicated_empty_and_disjoint` |
| Denied traversal | `..` out of either preopen, unmounted absolute paths, relative paths with no cwd, `read_dir("/")`, and a symlink inside an input pointing outside it all fail inside the guest; the secret file's content never crosses. | `traversal_out_of_input_mount_is_denied`, `symlink_escape_from_input_is_denied`, `unmounted_absolute_paths_are_denied` |
| Denied filesystem writes | Overwriting or creating files in an input mount, writing to `/tmp`, and `/out/../…` all fail; host input files and the temp root are byte-for-byte unchanged. | `writes_to_inputs_and_unmounted_paths_are_denied` |
| No network | `allow_tcp(false)`, `allow_udp(false)`, `allow_ip_name_lookup(false)` and a deny-all `socket_addr_check`. All of WASI p2 is *linked* so std-built components instantiate; authority is withheld per run, so each denial is observable. | `tcp_connect_to_listening_localhost_is_denied` (a real host listener receives no connection), `udp_bind_is_denied`, `dns_lookup_is_denied` |
| No ambient environment | No env vars, no args, no cwd, closed stdin, stdout/stderr captured into bounded in-memory pipes. The guest sees exactly its two preopens. A host env var set by the test is not visible. | `no_ambient_environment_arguments_or_extra_preopens`, `stdout_is_captured_and_bounded` |
| Deterministic clock/random | Wall clock fixed (default epoch zero, configurable), monotonic clock fixed and non-advancing, `wasi:random/random`, `insecure` and `insecure-seed` driven by SplitMix64 streams derived from `Determinism::random_seed`. Engine: canonical NaNs, deterministic relaxed SIMD, `threads` feature not compiled in. | `clocks_and_randomness_are_deterministic` (identical values and fuel across runs; seed/clock changes are observed), `full_step_report_is_reproducible`, `shared_memory_threads_are_not_enabled`, unit `splitmix_sequence_is_stable` |
| Deterministic filesystem metadata | Shadowing wrappers over `wasi:filesystem/types@0.2.12` (`src/fs_determinism.rs`) delegate to the stock implementation, then set every `stat`/`stat-at` timestamp to the configured wall clock, renumber `metadata-hash`/`metadata-hash-at` values in order of first observation (same object ⇒ same id), and yield `read-directory` entries sorted by name. | `filesystem_metadata_is_normalized` (two layouts with different host mtimes and creation order produce identical output; removing the wrappers makes it fail with real host times, inode hashes and order) |
| preview1 vs wasip2 | `classify()` inspects the binary before compilation: component layer → `Component`; core module importing `wasi_snapshot_preview1`/`wasi_unstable` → `Preview1Module`; other core module → `CoreModule`; else `NotWasm`. Only components compile. The fixture is built from the **same source** for both targets. | `preview1_module_is_distinguished_and_rejected`, `non_wasi_core_module_and_garbage_rejected` |
| Interface/version checks | Missing or mistyped `vesper:sandbox/step@0.1.0` export → `Link`. `protocol-version()` is called before `run`; mismatch → `ProtocolMismatch`. The payload carries `protocol_version` too; the fixture rejects unknown versions. | `component_without_step_export_fails_to_link`, `payload_protocol_version_is_checked_by_guest` |
| Blocking host calls | Fuel does not meter time blocked in host calls (e.g. `sleep`), so a non-deterministic wall-clock backstop wraps the async call (`WallClockTimeout`). | `blocking_sleep_hits_wall_clock_backstop` |

Denial tests were mutation-checked (the filesystem wrappers too): making inputs read-write, inheriting env, enabling network, dropping the determinism settings, or removing the limiter each makes the corresponding tests fail.

### Fixtures

* `fixtures/guest/` — Rust probe crate (`wit-bindgen` 0.63, `wasip2` 1.0.4) with one entry per authority (`read`, `write`, `list-dir`, `codegen`, `echo`, `authority`, `tcp-connect`, `udp-bind`, `dns`, `clock-random`, `spin`, `alloc`, `sleep`, `panic`, `stdout-flood`).
* `fixtures/build.sh` — builds it for `wasm32-wasip2` and `wasm32-wasip1` with path remapping and writes `fixtures/bin/{step.component.wasm, step.preview1.wasm, SHA256SUMS}`. Two clean rebuilds produced identical hashes on this host (rustc 1.97.0). The binaries (152 KB, 91 KB) are committed so the test suite needs no wasm toolchain; `fixtures_match_recorded_checksums` catches stale binaries.
* The fixture also has hostile entries (`alias-bomb`, `alias-bomb-diagnostics`, `scope-bomb`, `bad-slice`, `bad-utf8`, `error-with-tokens`) and an `fs-metadata` probe.
* Minimal components (`(component)`, shared-memory module) are written in WAT inside the tests.

## Versioned transport

WIT package `vesper:sandbox@0.1.0`, interface `step`:

```wit
protocol-version: func() -> u32;               // payload protocol, currently 1
run: func(invocation: invocation) -> result<outcome, list<diagnostic>>;
```

`invocation { protocol-version, step: build-script|proc-macro|derive-macro, entry, args, tokens }`. A `token` is flat (`ident`, `punct`, `int-literal`, `string-literal`, `open(delim)`, `close(delim)`) with `text`, `span { file, start, end }`, opaque `scopes: list<u32>` (Forge `ScopeMark`s) and `pre-resolved: option<def-id { package, item }>`. Trees use balanced delimiter tokens because WIT has no recursive types.

Results are **packed**: `packed-result { tokens: list<packed-token>, diagnostics: list<packed-diagnostic>, text: list<u8>, scopes: list<u32> }`, where each token's text/scopes and each diagnostic's message is a `slice { offset, len }` into the shared tables. Typed `string`/`list` results would be copied out of guest memory by Wasmtime *before* any host check, and many records could alias one large region of linear memory, so a guest could make the host allocate far more than its own memory limit without spending fuel. With packing, what Wasmtime lifts is flat and bounded by guest linear memory. The host then checks every slice's bounds and the summed text/scope budgets **before** copying any slice (`unpack` in `src/host.rs`), then validates UTF-8 and delimiter balance. An `error` result must carry no tokens.

The Rust mirror in `vesper_loom_sandbox::transport` (serde DTOs, independent of Wasmtime types) is what Forge and `build.vsp` orchestration should code against. `TransportLimits` bounds token count, text bytes, scopes per token, total scopes and diagnostics, and checks delimiter balance, on input (before execution) and output. Scope marks, spans and `DefId`s round-trip unchanged (`tokens_round_trip_with_spans_scopes_and_def_ids`). Hostile packed results are rejected without amplification: 100k tokens or diagnostics each aliasing the same 1 MiB (`aliased_text_slices_cannot_amplify_host_allocation`), aliased scope slices (`aliased_scope_slices_are_bounded`), out-of-bounds slices, invalid UTF-8 and tokens in an error result (`malformed_packed_results_are_rejected`).

Two version axes: the WIT package version is part of every export name (incompatible components fail to link); `PROTOCOL_VERSION` versions payload semantics within one WIT version.

## Public API

```rust
pub struct SandboxHost;
impl SandboxHost {
    fn new() -> Result<Self, SandboxError>;
    fn compile(&self, bytes: &[u8]) -> Result<StepComponent, SandboxError>;
    fn run(&self, &StepComponent, &StepConfig, &Invocation) -> Result<StepReport, SandboxError>;
    async fn run_async(&self, &StepComponent, &StepConfig, &Invocation) -> Result<StepReport, SandboxError>;
}
pub struct StepConfig { inputs: Vec<InputMount>, output_dir: PathBuf, limits: ResourceLimits, determinism: Determinism }
impl StepConfig { fn new(output_dir) -> Self; fn input(self, host_path, guest_path) -> Self }
pub struct InputMount { host_path: PathBuf, guest_path: String }
pub struct ResourceLimits { fuel, max_memory_bytes, max_table_elements, max_instances, max_memories, max_tables,
                            max_stdio_bytes, max_output_bytes, max_output_entries, wall_clock_backstop, transport }
pub struct Determinism { wall_clock_ns: u64, monotonic_ns: u64, random_seed: u64 }
pub const OUTPUT_GUEST_PATH: &str = "/out";
pub struct StepReport { result: Result<Outcome, Vec<Diagnostic>>, fuel_consumed: u64, peak_memory_bytes: usize,
                        stdout: Vec<u8>, stderr: Vec<u8>, outputs: Vec<OutputFile> }
pub struct OutputFile { path: String, bytes: u64 }
pub enum SandboxError { Config(ConfigError), NotAComponent { detected }, Compile, Link, ProtocolMismatch { host, guest },
                        FuelExhausted { limit }, MemoryLimitExceeded { limit, requested }, TableLimitExceeded { .. },
                        WallClockTimeout { limit }, Trap { message }, Transport { direction, violation },
                        Output(OutputViolation), Host { message } }
pub enum ConfigError { GuestPathNotAbsolute, GuestPathNotNormalized, GuestPathIsRoot, GuestPathOverlapsOutput,
                       GuestPathsNested, HostPathMissing, HostPathNotDirectory, OutputNotEmpty, HostPathsOverlap, ZeroLimit }
pub enum OutputViolation { NonRegularEntry, TooManyEntries, TooManyBytes, Unreadable }
pub enum ArtifactKind { Component, Preview1Module, CoreModule, NotWasm }
pub fn classify(bytes: &[u8]) -> ArtifactKind;
pub mod transport { PROTOCOL_VERSION, WIT_PACKAGE, STEP_INTERFACE, WIT_SOURCE, Invocation, StepKind, Token, TokenKind,
                    Delimiter, Span, DefId, Outcome, Diagnostic, Severity, TransportLimits, TransportViolation,
                    PackedTable, Direction }
```

All error and report types are `serde`-serializable and `PartialEq`.

## Validation

Run from `crates/vesper-loom-sandbox` (standalone workspace):

```sh
cargo fmt -- --check                                   # clean
cargo clippy --all-targets -- -D warnings              # clean
cargo test                                             # 16 unit + 35 integration + 1 doctest, all pass
(cd fixtures/guest && cargo clippy --release --target wasm32-wasip2 -- -D warnings)   # clean
./fixtures/build.sh                                    # needs wasm32-wasip2 + wasm32-wasip1 targets
```

The integration suite was run three consecutive times with no failures (~1.2 s each). Root `cargo metadata --locked` is unaffected.

## Integration requirements

1. Register `crates/vesper-loom-sandbox` in root `members` and `default-members`, delete its `[workspace]` table and nested `Cargo.lock`, and regenerate the root lockfile. **Keep** `fixtures/guest/Cargo.lock` and its `[workspace]` table: the guest is a separate wasm build, not a workspace member.
2. Dependencies (crate-local): `wasmtime 50` and `wasmtime-wasi 50` (default features off; `cranelift`, `component-model`, `async`, `runtime`, `std`, `wat`, `parallel-compilation`, `addr2line`; WASI `p2`), `wasmparser 0.261` (matches Wasmtime's), `tokio 1` (`rt`, `time`, `macros`), `rand_core 0.10`, `serde 1`; dev: `serde_json`, `sha2 0.10`, `tempfile 3`, `wat 1`.
3. Wasmtime 50 uses Cranelift 0.137, while the compiler uses Cranelift 0.125 (and already pulls `wasmtime-internal-*` 38.0.4 via `cranelift-jit`). The root lockfile will carry both Cranelift versions; no existing dependency is downgraded. See decision D1.
4. CI does not need wasm targets to run the tests. A fixture-freshness job could run `fixtures/build.sh` and `git diff --exit-code fixtures/bin`.
5. Forge (§16.4) should call `SandboxHost::run` with `StepKind::{ProcMacro, DeriveMacro}` and its own `TransportLimits`, assign/verify scope marks and `PreResolvedDefId`s, and apply `[T-MacroConfinement]` to the returned tokens. Loom's `[build-sandbox]` maps `wasi_read_paths` → `InputMount`s, `fuel_limit` → `ResourceLimits::fuel`; `allow_network = true` must be rejected by the manifest layer, because this host has no network-granting mode.
6. Memoization by `(component BLAKE3, invocation hash)` belongs to the CAS lane. Inputs that must be in the key: component bytes, canonical invocation, `Determinism`, the contents of every input mount, and the limits (a step that runs out of fuel under one budget succeeds under another).

## Ambiguities and proposed decisions

* **D1 — Wasmtime version.** The design scaffold names Wasmtime 29. Wasmtime 38.0.4 would share Cranelift 0.125 with the compiler, but it is outside Wasmtime's support window. A security sandbox should track a supported release, so this lane uses **50.0.1** and accepts a second Cranelift in the lockfile. The coordinator may choose to align later.
* **D2 — Scaffold `env("OUT_DIR", "/out")`.** Dropped: the guest receives *no* environment. `/out` is a protocol constant (`OUTPUT_GUEST_PATH`), so the guest needs no variable to find it.
* **D3 — Scaffold `run-build: func() -> result<_, string>`.** Replaced by the versioned `step` interface with typed tokens/diagnostics, shared by build steps and macros.
* **D4 — Secure randomness.** `wasi:random/random` is seeded and reproducible, which is the price of hermetic builds. Build steps and macros must not generate secrets. The guest cannot tell the difference.
* **D5 — Empty output directory.** The design says "dedicated `$OUT_DIR`". This lane enforces "empty before the run" so the post-run scan describes exactly the step's writes. Callers clear or recreate it.
* **D6 — Monotonic clock never advances.** Timing loops spin until fuel runs out (deterministic). Real `sleep` still blocks on the host timer, hence the backstop.
* **D7 — Coordinator note mismatch.** The coordinator's note on this thread quoted the Ignis barrier-graph prompt, but the user's own message assigned this Forge/WASI lane. This lane follows the user's message. No Ignis paths were touched.

## Limitations (explicit)

* Isolation is Wasmtime's software sandbox and `cap-std`. No hardware isolation, kernel sandboxing (seccomp/landlock), or formal soundness is claimed. Wasmtime/cap-std bugs are in the trusted base.
* Fuel counts are reproducible for a fixed Wasmtime version and engine configuration on this host. Cross-version or cross-architecture equality is not claimed. Fuel is checked at function entries and loop headers, so a step may overrun its budget slightly before trapping.
* The wall-clock backstop is inherently non-deterministic. A step stopped by it must not be cached as a result.
* Output size is checked **after** the run, not as a quota. A step can fill the disk up to available space before the check fires. `max_output_bytes` limits what is accepted, not what is written. A real quota needs a bounded filesystem implementation.
* On a failed run (trap, fuel, memory, timeout), captured stdout/stderr and partial outputs are not returned in the error. The output directory may hold partial writes, and callers should discard it.
* Filesystem metadata normalization covers timestamps, `metadata-hash` and directory order. Guests may still call `set-times` on their own outputs (it affects only host-side mtimes, which are not part of the report). The wrappers are pinned to the WASI 0.2.12 package name that `wasmtime-wasi` 50 registers; a Wasmtime upgrade must keep `filesystem_metadata_is_normalized` passing.
* The host still lifts the flat packed lists before checking them; that allocation is bounded by guest linear memory (`max_memory_bytes`) times a small constant, not by `TransportLimits`.
* Directory creation, renames and deletions inside `/out` are allowed (WASI read-write preopen). The fixture exercises file writes only.
* Preview1 modules are classified and rejected; no preview1 adapter path is provided.
* Not wired into the CLI, the Loom manifest parser, Forge, or CAS memoization. Not validated on macOS or Windows (symlink test is Unix-only).
* Scope-set hygiene, `[T-MacroConfinement]`, CST arenas and `AdtSchema` are out of scope. The token transport is flat and experimental, not the canonical `.vhir`/CST representation.
