# Bounded original-model process endpoint (#481)

This lane owns only `src/bin/resource_oracle42/process.rs` and this report.
It follows the endpoint contract at `1371e65` and its fixed 512-byte complete
BYE-frame reserve clarification. Protocol commit `3ed190b8336b52ba299aa84f384ace07058514d9`
and worker commit `a263e1f24ade06fe27ada334832275b56c8f5e8a` were imported as
coordinated dependencies, not edited. No existing model, metatheory, runtime,
native, validator, auditor, profile, manifest, lockfile, workflow or CLI source
changed in this lane.

## Actual production path

`Config`, default `HarnessLimits`, non-Clone must-use `Session`, `start`,
`Session::evaluate` and consuming `Session::finish` implement the frozen API.
Errors expose a typed kind and a UTF-8-safe detail bounded to 1,024 bytes.
Explicit abort returns an abandonment error, never completion evidence.
Dropping a session increments `abandoned_sessions`; ignored destructor
cleanup failures increment the separate `cleanup_failures` observation.

Five project sources are embedded with `include_str!`: MetatheoryFull,
ResourceModel42.Syntax, Checker, Evaluator and ResourceOracle42. Before any
compiler invocation, each on-disk source must be a regular, nonsymlink file
with exactly those bytes. The process creates a new private root, copies the
fixed sources and compiles all five original modules in order. No projection
object, arbitrary source/module selector or expected model result is accepted.

The configured Lean invocation resolves its prefix; the real prefix binary
must report version 4.24.0 and full commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`. Its executable bytes are hashed.
Commands use a clean environment, explicit private-plus-prefix `LEAN_PATH`,
one thread, `-j1 -M4096`, and necessary platform environment variables.

Every actual project source, original object and generated companion in the
private root is inventoried with relative path, byte count and BLAKE3. Source
copies are checked against embedded bytes after every compile. File/total
artifact sizes and entry counts are also monitored while compilation runs.
Files and project directories become read-only before worker execution.
Every evaluation and final retirement check the exact project inventory,
fixed on-disk sources and real compiler executable again.

The receipt explicitly scopes attestation to
`project_artifacts_and_compiler_executable`. Standard-library objects and
shared libraries come from the pinned prefix and are not individually
attested. This is neither a whole-toolchain attestation nor a machine-code or
checker implementation proof. Temporary roots are removed; setting
`artifact_dir` retains a uniquely created private child for inspection.

The live process has bounded asynchronous stdin writing and stdout/stderr
reading, write queue capacity one and event queue capacity four. A blocked
stdin write is covered by the same request deadline as the response. Stderr
retains only its configured bounded prefix. Failures poison the epoch and
force bounded cleanup; no failed epoch can later return completion evidence.
Shutdown kills when necessary, polls/reaps the child and joins completed pipe
threads. A pipe that cannot close within the shutdown deadline yields failure,
not an unbounded join or successful retirement.

READY must match the exact protocol, Lean identity and twelve fixed bounds.
Every response must be one typed result with the next sequence. Host counters
are derived from actual decoded stages. Before a model call the host prepays
the request frame, a maximum response frame and the fixed 512-byte complete
BYE reserve against the remaining wire budget, including actual READY bytes.
EOF retirement requires exact BYE sequence/counters, stdout EOF with no
trailing bytes, successful process exit and final artifact checks. Receipts
include actual command arguments/cwd/search path/exits and stream hashes,
the full project manifest, total framed bytes, independently derived counters
and a direction-plus-length-plus-payload transcript BLAKE3.

## Outer limits

| Limit | Default | Hard maximum |
| --- | ---: | ---: |
| Compile timeout per module/tool, ms | 120000 | 180000 |
| Request timeout including write, ms | 30000 | 60000 |
| Shutdown timeout, ms | 5000 | 15000 |
| Requests/session | 64 | 4096 |
| Total framed session bytes | 67108864 | 67108864 |
| Retained stderr bytes | 262144 | 1048576 |
| Compiler stdout bytes | 262144 | 1048576 |
| Artifact bytes/file, including executable hashing | 67108864 | 268435456 |
| Project artifact bytes total | 268435456 | 1073741824 |

Artifact entries additionally have a fixed 128-entry bound. Callers can only
lower outer limits. They cannot change schema checks, model source, semantic
limits, sequence validation, the reserve or artifact policy.

## Genuine execution and negative controls

After the coordinator explicitly granted exclusive Cargo/shared-target and
Lean lease 1, a fresh standalone wrapper was created at
`/tmp/resource-model-endpoint42-process-probe-v1`. It imports the actual owned
process and frozen protocol files by path, with exact dependencies serde
1.0.229, serde_json 1.0.154 and blake3 1.8.7. The wrapper permits unused public
module exports; it suppresses no other Clippy warnings. All invocations used:

```sh
CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next
CARGO_BUILD_JOBS=1
CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0
CARGO_INCREMENTAL=0
```

Executed commands, with those variables exported, were:

```sh
cargo check --offline --manifest-path /tmp/resource-model-endpoint42-process-probe-v1/Cargo.toml
cargo run --offline --manifest-path /tmp/resource-model-endpoint42-process-probe-v1/Cargo.toml
cargo clippy --offline --manifest-path /tmp/resource-model-endpoint42-process-probe-v1/Cargo.toml --all-targets -- -D warnings
cargo run --offline --manifest-path /tmp/resource-model-endpoint42-process-probe-v1/Cargo.toml -- --controls
rustc --edition=2024 /workspace/vesper-worktrees/resource-model-endpoint42-acceptance/tests/fixtures/resource_oracle42/fault_peer.rs -o /tmp/resource-model-endpoint42-process-fault-peer-v1
cargo test --offline --manifest-path /tmp/resource-model-endpoint42-process-probe-v1/Cargo.toml hostile_production_transport -- --nocapture
cargo clippy --offline --manifest-path /tmp/resource-model-endpoint42-process-probe-v1/Cargo.toml --all-targets -- -D warnings
cargo run --offline --manifest-path /tmp/resource-model-endpoint42-process-probe-v1/Cargo.toml -- --drop
cargo clippy --offline --manifest-path /tmp/resource-model-endpoint42-process-probe-v1/Cargo.toml --all-targets -- -D warnings
```

All commands exited zero. There were no failed Cargo or Lean invocations.
Direct rustfmt and whitespace checks also pass. The last source change only
clears the private root directory's Windows read-only attribute during cleanup;
its final Linux strict Clippy check passed. Windows execution remains with CI.

Every genuine Session itself ran real pinned Lean `--print-prefix`, `--version`
and `--githash`, then these commands in its fresh private directory, with
`LEAN_NUM_THREADS=1` and explicit `LEAN_PATH=<private>:<prefix>/lib/lean`:

```sh
lean -j1 -M4096 -o MetatheoryFull.olean MetatheoryFull.lean
lean -j1 -M4096 -o ResourceModel42/Syntax.olean ResourceModel42/Syntax.lean
lean -j1 -M4096 -o ResourceModel42/Checker.olean ResourceModel42/Checker.lean
lean -j1 -M4096 -o ResourceModel42/Evaluator.olean ResourceModel42/Evaluator.lean
lean -j1 -M4096 -o ResourceOracle42.olean ResourceOracle42.lean
lean -j1 -M4096 --run ResourceOracle42.lean
```

The actual eleven-response probe covered all six stages: one validation
error, two semantic checking errors, one checking profile refusal, one input
invocation error, one evaluation-admission error and five evaluations. The
evaluations included completed unit, mint/consume, returned-live owner, zero
fuel and a resource-limited prefix. One natural above u64 was transported to
the actual checker, rather than rejected or clamped by the host. All nine
recorded subprocess exits were zero. Actual BYE and host counters matched
`[11,1,2,1,1,1,5]`; complete wire bytes were 2276 and the transcript BLAKE3 was
`3ab36dc19600a906f2dba3481cff14713e9362598f2678915a37502c5e8a8eaf`.

The full ten-record original-project manifest and command/stream identities
are in `/tmp/resource-model-endpoint42-process-genuine-v1-evidence.json`,
SHA-256 `269eab2fc3bf0b97db6fc42092347ee9b2ad77e0f4ae204e2312797be2167e5f`.
Its project artifacts total 20,852,248 bytes. No generated companions were
present in this run; the inventory would include them if emitted.

| Original artifact | Bytes | BLAKE3 |
| --- | ---: | --- |
| MetatheoryFull.lean | 117218 | a982ff27e761a06abed814356757e72b74bc50229cb3d00b5ee7561e30268a1f |
| MetatheoryFull.olean | 13455096 | f1a7ad2f84d789107f1fa7b4447b624af0a745126fdbc2fc24267cf896aea8a4 |
| ResourceModel42/Syntax.lean | 19712 | 74e234aea0203044d2d5159fba90c90d39279cd126515f3bdf8fe176e0147dae |
| ResourceModel42/Syntax.olean | 3257760 | 80978781b5feb67b5f136a8517bb28ca905820cc2507d689aa371eb90d2b8873 |
| ResourceModel42/Checker.lean | 9026 | 6d76de5e04384e4239bb9836bdc24b3f1846e63b16f7c13b5faac93ee4f5a2e4 |
| ResourceModel42/Checker.olean | 757568 | 2a7368b13eb0dd34d5b783dd4894d7bd5ad61a749ef2433250397765225cc74f |
| ResourceModel42/Evaluator.lean | 11133 | 8c82416335a00edbe7a3d8a1b9b1803c4233e1da8f8768b0be3a2c839ed3896b |
| ResourceModel42/Evaluator.olean | 1462904 | 7f822144363d11012047e1ceae87c0e81b7cb770cf65e35a8f1662f1b3d02790 |
| ResourceOracle42.lean | 16855 | 7381930f329b5feeeb3c26dd3f3bb1301dc8641bcea497663ea653aed14c00e5 |
| ResourceOracle42.olean | 1744976 | 134f2b53ea1e9674a4de669b8c7e128ca0d3cbdf56b8adb1c9df958f8ce1d1d9 |

Production controls separately confirmed poisoned near-budget refusal,
original-object corruption refusal before a request, abandonment detection,
bounded successful kill/reap/join and private-root cleanup. Missing, directory,
same-length changed, oversized and symlink source files all produced
SourceIdentity before any compiler invocation. A final genuine drop probe
checked the abandonment counter, unchanged cleanup-failure count and removed
private root. No cleanup operation emitted model consume events or evidence.

The frozen acceptance peer came from commit
`168ca538fd40e7935e1e999bc54913dcf3262a76`, source SHA-256
`a5626b88775734e4c700f679a150e19bb52617d8d4647f67d0642077daf1b9a7`.
Through the production negative-only `test_transport_failure` seam, seventeen
hostile sessions passed: wrong READY, stdout noise, malformed JSON, unknown
constructor, response sequence, oversized/zero frames, truncated header/payload,
stderr flood, stall, crash, missing BYE, bad BYE counters, trailing bytes,
nonzero exit and blocked stdin with a valid request above 524 KiB. The test
completed in 1.35 seconds. This seam never returns a mathematical response or
SessionEvidence; even a cooperative synthetic peer returns SyntheticPeer.

Logs are `/tmp/resource-model-endpoint42-process-check-v1.log`,
`-genuine-v1.log`, `-controls-v1.log`, `-transport-v1.log`, `-drop-v1.log`,
`-clippy-v1.log`, `-clippy-v2.log` and `-clippy-final.log` with the same prefix.
The source `process.rs` has SHA-256
`064831f7367c5206b9a321c7bd199bccd59459c1e877171c8054b7e01a4f493d`.
Both the Cargo/shared-target lease and Lean lease 1 were explicitly released
after final validation. Root CLI/acceptance integration and hosted CI remain
separate gates. No source lockstep, native provider, resource campaign,
exhaustion theorem or implementation-proof badge follows from these results.
