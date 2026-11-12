# Loom sandbox builds backed by CAS (next wave, lane 3)

Branch: `claude/project-thread-kqm6rm` (the prompt suggested `next/loom-cached-builds`; this session's designated branch was used instead). Base: `origin/main` at `c2ba0b1`, which contains the requested minimum `0e6caf9` and the prompt commit `2a8e46e`.
Owned paths: `crates/vesper-loom-build/**` and this file. No root manifest, lockfile, CLI, CI, Forge or existing crate is changed. `vesper-loom-cas`, `vesper-loom-sandbox` and their fixtures are used read-only.

## What is implemented

`crates/vesper-loom-build` is a standalone library and executable (temporary `[workspace]`, own `Cargo.lock` seeded from the root lock, so no dependency version differs from root). It turns the CAS store and the real Wasmtime WASI component host into an offline build-step runner.

| Requirement | Implementation |
| --- | --- |
| Versioned request/outcome | `BuildRequest` (`BUILD_REQUEST_VERSION = 1`): component bytes, `Invocation`, input mounts, target, options, `Determinism`, `ResourceLimits`, `NetworkPolicy`. `BuildOutcome` (`BUILD_OUTCOME_VERSION = 1`) is serde-serializable. A JSON `RequestFile` (strict, `deny_unknown_fields`, version checked before the rest is parsed) resolves relative paths against the request file. |
| Validation before execution | Unknown request/protocol versions; `ProcMacro`/`DeriveMacro`; `network: allowed`; non-component binaries (preview1 module, garbage); empty/control-character labels; zero limits; non-absolute, unnormalized or root guest paths; mounts at or under `/out`; duplicate or nested guest mounts; identical or nested host inputs; missing or non-directory inputs; a store, scratch area or destination inside an input (or the reverse). All are rejected before any snapshot. |
| Snapshot, then execute snapshots | `Runner::prepare` stores the component (`Artifact` blob), the canonical invocation (`Source` blob) and every input (`Source` tree via `ingest_dir`). `Runner::execute` materializes each snapshot into a fresh private scratch run with `Store::materialize` and mounts *those* copies read-only. The original host directory is never mounted. |
| Inputs changing during ingestion | Each input is ingested twice; differing tree ids give `InputChangedDuringSnapshot` and nothing runs. After the run, every materialized input is re-ingested and must still equal its snapshot (`InputMutatedDuringRun`). Symlinks, special files and non-portable names in inputs are refused by CAS ingestion (`Snapshot` error). |
| Canonical derivation key | A CAS `DerivationInputs` V1 (see "Key encoding"). It binds component bytes, input tree digests, guest mount points, the exact invocation (entry, args, tokens, step, protocol), target, user options, every `ResourceLimits`/`TransportLimits`/`Determinism` field, network policy, and the `ExecutionIdentity`. The host location of an input is deliberately *not* in the key. |
| Unknown fingerprints | `ExecutionIdentity` is computed at compile time by `build.rs`. If any field is unknown, `ReadWrite` and `Verify` fail with `UnknownFingerprint` before execution. Only `CachePolicy::Uncached` runs, and it records nothing. |
| Hit verification | On a record: the manifest blob is rehashed and strictly decoded (canonical bytes, known schema, no unknown fields). Its embedded key must equal the lookup key. Every object under the output tree is rehashed and sizes are checked. The file list must match the manifest, and stdout/stderr blobs are rehashed. Outputs are then materialized from the store as independent copies, never hard links. |
| Miss execution | The real `SandboxHost` runs in a fresh scratch run with an empty dedicated `out/`. A result is accepted only if the guest returned `Ok` with no error-severity diagnostic and the sandbox's own output checks passed. The output must also ingest as a valid portable `Artifact` tree whose file listing equals the sandbox's scan. Output tree, stdout, stderr and a `ResultManifest` are stored; only then is the derivation record published (CAS no-replace publish). |
| Failures publish nothing | Guest errors, traps, fuel, memory, wall-clock timeout, stdio overflow, output-size violations and rejected outputs return errors. No record is published, and an existing destination stays byte-for-byte unchanged. The scratch run is removed by a drop guard. |
| Destination | `DestinationPolicy::CreateNew` (default) refuses an existing destination before execution. `Replace` builds the new tree in a sibling staging directory and swaps it in only after acceptance. On Linux the swap is one `renameat2(RENAME_EXCHANGE)`; elsewhere it is rename-aside, rename-in, with rollback on failure. Non-directory destinations are refused. |
| Corrupt and dangling entries | Corrupt objects, missing manifests (dangling records), corrupt records and a manifest for another key all report `CorruptCache` (`CorruptionPolicy::Fail`, default). With `Rebuild`, the step re-executes; re-ingesting the true bytes lets CAS quarantine and repair the objects/records, and the status is `Repaired`. |
| Concurrent same-key writers | Any number of runners or processes may share a store. Duplicate objects deduplicate. Exactly one writer creates the record; identical results are idempotent. A different result for an existing key is `DivergentOutput` (from CAS `DerivationConflict`) and is never overwritten. `CachePolicy::Verify` re-executes even on a hit and reports `DivergentOutput` if the recorded result differs. |
| Stale state | `Runner::sweep_stale(min_age)` removes `run-*` scratch directories left by killed processes and calls `Store::sweep_temporaries`. Destination staging uses `.vlb-stage-*`/`.vlb-old-*` siblings, which are removed on every error path that returns. |
| Step restriction | Only `StepKind::BuildScript`. Macro kinds are rejected in validation, both in the library and in request files. |

## Key encoding

The derivation key is the `vesper-loom-cas` V1 key: BLAKE3 `derive_key("vesper-loom-cas 2026-11 v1 derivation key", canonical DerivationInputs encoding)`. That crate defines the byte encoding (sorted, length-prefixed maps). This crate fixes its contents:

```text
output_domain   Artifact
builder         "vesper-loom-build"
builder_version CARGO_PKG_VERSION
target          request.target (absent = target-independent)
options
  vlb.key-schema = 1, vlb.request-version = 1, vlb.step = build-script, vlb.network = denied
  builder.source       BLAKE3 of this crate's Cargo.toml, build.rs, src/
  sandbox.source       BLAKE3 of vesper-loom-sandbox Cargo.toml, src/, wit/
  cas.source           BLAKE3 of vesper-loom-cas Cargo.toml, src/
  engine.dependencies  BLAKE3 of locked name/version/source/checksum of every
                       wasmtime*, cranelift*, pulley*, regalloc2, winch*, wasmparser,
                       wasm-encoder, wasm-metadata, wasi*, wit-*, cap-*, io-extras,
                       fs-set-times, rustix, system-interface package
  engine.rustc         rustc -vV
  engine.host-target   compile target triple of the runner
  sandbox.protocol / sandbox.wit-package / sandbox.wit-blake3
  determinism.{wall-clock-ns, monotonic-ns, random-seed}
  limits.{fuel, max-memory-bytes, max-table-elements, max-instances, max-memories,
          max-tables, max-stdio-bytes, max-output-bytes, max-output-entries,
          wall-clock-backstop-ns}
  limits.transport.{max-tokens, max-text-bytes, max-scopes-per-token,
                    max-total-scopes, max-diagnostics}
  user.<key>           each request option
inputs
  component            Artifact blob id of the exact component bytes
  invocation           Source blob id of serde_json(Invocation)
  mount:<guest path>   Source tree id of each input snapshot
```

`key.rs` destructures `ResourceLimits`, `TransportLimits` and `Determinism` without `..`, so a field added to the sandbox fails to compile until the key binds it. The canonical invocation is `serde_json` of the sandbox's `Invocation`. That encoding depends on the sandbox's struct definitions, which are covered by `sandbox.source`.

Result manifest (`vesper-loom-build/result/v1`): canonical `serde_json` of `ResultManifest { schema, key, output_tree, outputs, outcome, stdout, stderr, fuel_consumed, peak_memory_bytes }`. The derivation record points to this one `Artifact` blob.

## Compatibility domain

A key can be reused only by a runner with the same builder, sandbox and CAS sources, the same locked engine packages, the same `rustc`, the same host target triple, the same WIT and protocol, and identical limits and determinism. This is deliberately conservative. Two runners with the same engine but different lockfiles elsewhere in the engine set, or different `rustc`, never share results. After integration the root lockfile also contains Cranelift 0.125 / `wasmtime-internal-*` 38 (used by the compiler). Those match the prefixes, so the integrated runner's `engine.dependencies` digest will differ from the standalone one. That is a different identity, not a soundness issue. The build profile (debug/release) is not in the key: it does not change Wasmtime semantics or the deterministic fuel count.

`Runner::with_identity_unchecked` lets an embedder or test substitute the identity. It is documented as unchecked: claiming another configuration's identity makes its results reusable here.

## Trust assumptions

- A derivation record and result manifest are a **local builder's claim**. A byte hash is content identity, not provenance. Nothing is signed, attested or checked against a transparency log.
- A forged but well-formed record for a key is accepted as a hit under `ReadWrite`; only `Verify` (re-execution) detects it (`divergent_results_for_one_key_are_detected_not_overwritten`). The store root is assumed writable only by its owner (same threat model as `vesper-loom-cas`).
- No package confinement or authority is certified. The manifest's residual effect rows are not consulted, and the sandbox's lack of network is a property of this host, not an audit. Forge macros stay disconnected until the provenance/authority gate exists.
- Isolation is Wasmtime plus `cap-std`, as in the sandbox report. There is no kernel or hardware isolation claim.

## Output quota limits

`max_output_bytes`/`max_output_entries` are the sandbox's **post-run acceptance checks, not a disk quota**. A step can fill the scratch filesystem before the check fires; the run is then rejected and its scratch run removed. Callers should put the scratch area on a bounded filesystem if that matters. Output names must also satisfy the CAS portable naming policy, so some Linux-legal names (for example `a:b`) are rejected after the run.

## Public API

```rust
pub const BUILD_REQUEST_VERSION: u32 = 1;      pub const BUILD_OUTCOME_VERSION: u32 = 1;
pub const KEY_SCHEMA_VERSION: u32 = 1;         pub const BUILDER_NAME: &str;
pub const RESULT_MANIFEST_SCHEMA: &str;
pub struct BuildRequest { version, component, invocation, inputs: Vec<InputSpec>, target, options,
                          determinism, limits, network }
  // build_script(bytes, entry), with_args, with_input(guest, host), with_target, with_option, validate
pub struct InputSpec { guest_path, host_path }
pub enum NetworkPolicy { Denied, Allowed /* always rejected */ }
pub struct RequestFile { .. }  // parse(&str), load(&Path) -> BuildRequest, into_request(base)
pub struct LimitsFile { .. }   pub enum StepName { BuildScript, ProcMacro, DeriveMacro }
pub struct ExecutionIdentity { builder_version, builder_source, sandbox_source, cas_source,
                               engine_dependencies, rustc, host_target, wit_package, wit_digest,
                               protocol_version }  // current(), missing(), is_complete()
pub fn canonical_invocation(&Invocation) -> Vec<u8>;
pub struct BuildEngine;   // new(), executions(); SandboxHost + compiled-component cache
pub struct Runner;        // open(engine, store_root, scratch_root), with_identity_unchecked, identity,
                          // store, engine, prepare, execute, build, sweep_stale
pub struct PreparedBuild; // key, derivation_inputs, component, invocation, snapshots
pub struct RunOptions { cache: CachePolicy, on_corrupt: CorruptionPolicy, destination: DestinationPolicy }
pub enum CachePolicy { ReadWrite, Verify, Uncached }
pub enum CorruptionPolicy { Fail, Rebuild }
pub enum DestinationPolicy { CreateNew, Replace }
pub enum CacheStatus { Hit, Miss, Repaired, Verified, Uncached }
pub struct BuildOutcome { version, key, cache, newly_recorded, executed, manifest, output_tree,
                          destination, outputs, outcome, stdout, stderr, fuel_consumed, peak_memory_bytes }
pub struct ResultManifest { .. }  // encode, decode (strict)
pub enum RequestError { .. }      pub enum BuildError { .. }
```

## Runner invocation

```sh
cd crates/vesper-loom-build
cargo run --bin vesper-loom-build -- run --cache /tmp/vlb --request examples/codegen/request.json --out /tmp/gen1
#   -> {"version":1,"cache":"miss","executed":true,...}; /tmp/gen1/generated.vsp
cargo run --bin vesper-loom-build -- run --cache /tmp/vlb --request examples/codegen/request.json --out /tmp/gen2
#   -> "cache":"hit","executed":false, same key/manifest
cargo run --bin vesper-loom-build -- key --cache /tmp/vlb --request examples/codegen/request.json
cargo run --bin vesper-loom-build -- run ... --out /tmp/gen1 --replace --policy verify --on-corrupt rebuild
cargo run --bin vesper-loom-build -- sweep --cache /tmp/vlb --min-age-secs 3600
cargo run --bin vesper-loom-build -- identity
```

Exit status: 0 for an accepted result, 1 for a build or request failure, 2 for a usage error. The example request uses the committed sandbox fixture component (`../../../vesper-loom-sandbox/fixtures/bin/step.component.wasm`, checked against its `SHA256SUMS` by `fixture_matches_recorded_checksum`). This crate adds no binary fixtures.

## Validation

Rust 1.97.0, x86_64 Linux (ext4, so CAS used its copy fallback). Run from `crates/vesper-loom-build`:

```sh
cargo fmt -- --check                         # clean
cargo clippy --all-targets -- -D warnings    # clean
RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps  # clean
cargo test                                   # 2 unit + 17 integration + 2 CLI + 1 doctest passed
```

The integration suite ran three consecutive times with no failures (about 7 s each). Root `cargo metadata --locked` is unaffected. No network or WASI target is needed. No Lean, kernel or platform-specific validation applies to this lane.

| Test | Acceptance item |
| --- | --- |
| `miss_then_hit_with_identical_accepted_outputs` | Miss then hit (same runner and fresh runner); identical key, manifest, tree, files, outcome, fuel, stdout; scratch empty; `verify_all` clean |
| `workspace_edits_cannot_mutate_stored_artifacts` | Distinct inode, `nlink == 1`, edits to a materialized result leave stored bytes and later hits intact |
| `every_observable_input_and_configuration_changes_the_key` | 27 request variants (component, args, entry, tokens, target, options, each determinism and limit field, mount point), 5 input-content edits (edit, add, rename, exec bit, subdirectory) and an extra mount all give distinct keys; relocating the input does not |
| `same_interface_component_change_misses_and_executes` | Same WIT interface, different component bytes (custom section appended) → new key, real execution, both records kept |
| `incompatible_engine_builder_or_host_identities_never_share_results` | Engine deps, host target, builder source/version, sandbox, CAS, rustc, WIT, protocol: each misses and executes |
| `unknown_fingerprint_refuses_reusable_results` | Incomplete identity: `ReadWrite`/`Verify` refused before execution; `Uncached` runs and records nothing |
| `mutating_the_original_after_snapshot_does_not_affect_execution` | Original edited between `prepare` and `execute`; output reflects the snapshot |
| `inputs_are_read_only_and_ambient_authority_is_denied` | Writes to a mount fail; the original host path is unreachable; only `/in/proto` and `/out` are preopened; no env/args; DNS denied; failures publish nothing |
| `failures_publish_nothing_and_preserve_the_destination` | Panic, fuel, memory, wall-clock timeout, stdio overflow, output bytes, guest error, bad input: no record, destination unchanged, scratch empty; a later success replaces atomically without leftovers |
| `create_new_refuses_an_existing_destination_before_executing` | Existing directory or file destination; destinations in an input, the store or scratch |
| `concurrent_writers_of_one_key_agree` | 6 threads with separate runners: exactly one `newly_recorded`, same manifest, one clean record |
| `divergent_results_for_one_key_are_detected_not_overwritten` | Forged well-formed record: `Verify` reports `DivergentOutput`, leaves the record and creates no destination; an honest record verifies |
| `corrupt_and_dangling_cache_entries_are_explicit` | Corrupt output object, deleted manifest, garbage record, manifest for another key: `CorruptCache`; `Rebuild` repairs to `Repaired`, then a clean hit |
| `invalid_requests_are_rejected_before_any_execution` | Every validation rule above, plus a symlink inside an input (snapshot refusal) |
| `request_files_are_versioned_and_strict`, CLI tests | JSON round trip, future version, unknown fields, macro step in a file; executable miss/hit/key/replace/verify/sweep/identity and exit codes |
| unit `snapshot_refuses_inputs_that_change_between_passes`, `manifest_decoding_is_strict` | Mid-ingestion change detection; non-canonical, unknown-field and unknown-schema manifests rejected |

Mutation checks (each reverted): mounting the original host directory instead of the snapshot fails the mutation-after-snapshot test. Skipping output-tree verification on a hit fails the corruption test. Dropping `limits.fuel` from the key fails the key test. Dropping the execution identity from the key fails the identity test.

## Integration steps

1. Register `crates/vesper-loom-build` in root `members` and `default-members`. Delete its `[workspace]` table and nested `Cargo.lock`, and regenerate the root lockfile. Keep `default-run = "vesper"` for the root package; this crate's binary is `vesper-loom-build`.
2. Dependencies are `vesper-loom-cas`, `vesper-loom-sandbox` (path), `blake3 1.8` (1.8.7), `serde 1`, `serde_json 1`, unix `libc 0.2` (0.2.191), build-dependency `blake3`, dev `sha2 0.10`, `tempfile 3`. All of these are already in the root lock at the same versions.
3. `build.rs` reads `../vesper-loom-sandbox` and `../vesper-loom-cas` and the nearest `Cargo.lock` above the crate. Keep the three crates as siblings under `crates/`; otherwise the fingerprint becomes `unknown` and caching is refused (fail-closed).
4. CI can run `cargo test -p vesper-loom-build`. It needs no wasm target. The sandbox fixture-freshness job also covers this crate's fixture.
5. Wiring into the root `vesper` CLI, `Loom.lock` and the Loom build graph is coordinator work and was not done here.

## Ambiguities and decisions

- **Component domain.** Component bytes are stored as an `Artifact` blob. The invocation and input trees are `Source` objects. The output tree, stdout/stderr and the manifest are `Artifact`.
- **Snapshot stability.** Two-pass agreement detects changes that land between the passes, not a change that is reverted in between (ABA). The executed snapshot always equals the keyed tree, so the key never describes data that did not run. An undetected concurrent edit can only produce a snapshot that matches no single instant of the original directory.
- **Error diagnostics on success.** A guest `Ok` carrying an error-severity diagnostic is treated as a failure, so it is never cached.
- **Fuel and peak memory in the manifest.** These are deterministic for a fixed identity, so they are part of the accepted result. A divergence there is reported like any other divergence.
- **Host path not in the key.** Only contents and guest mount points are observable to the step, so moving an input directory reuses results.

## Limitations

- No registry, substituter, authenticated lockfile, Forge expander or full Loom build graph. One step per request, with no dependency edges between steps.
- Output limits are post-run acceptance only (above). Scratch and destination writes are not fsynced individually.
- Corruption *after* a successful hit verification (between verification and materialization) is caught by CAS's verified copy, which fails the materialization. It is reported as a store error, not as `CorruptCache`.
- `Replace` is atomic only on Linux (`RENAME_EXCHANGE`). Elsewhere there is a short window with no destination, with rollback on failure. Two callers must not target the same destination concurrently.
- `sweep_stale` age is based on directory mtime. A run longer than `min_age` can be removed while it is live, so choose a `min_age` above the longest `wall_clock_backstop`.
- Tested only on x86_64 Linux. Windows paths compile but are untested.
- The compiled-component cache in `BuildEngine` is keyed by component id and unbounded.
