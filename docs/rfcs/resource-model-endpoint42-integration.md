# Executable finite resource-model integration (#478)

This endpoint runs data requests through the existing
`validate → checkProgram → validateInvocation → evalSteps` implementation.
The fixed Lean worker imports the existing model; it does not copy inference
or evaluation. Requests contain constructor codes, model limits, invocation
values and fuel. They cannot provide inferred types, masks, proofs, expected
results, module names or executable source.

The Rust supervisor compiles the five original project modules with pinned
Lean 4.24.0 in a fresh private root. It records the compiler executable,
source/object/companion identities, actual command exits and framed transcript.
Those identities are checked around every observation and retirement.
Standard-library objects and shared libraries come from the pinned toolchain
prefix and are not individually attested by this production supervisor. The
serialized evidence states that limitation explicitly.

The CLI accepts one complete sequence-zero JSON request:

```sh
cargo run --locked --bin resource_oracle42 -- \
  --request tests/fixtures/resource_oracle42/mint_consume.json \
  --lean /path/to/pinned/lean
```

`--repo-root` selects the regular source tree checked against the embedded
sources. `--artifacts DIR` retains the private build epoch below that existing
directory. Otherwise its temporary epoch is removed after the session.
Successful JSON output is serialized only after exact BYE counters, clean
stdout EOF, zero process exit and final artifact checks. A model refusal is
data with exit zero. Build, protocol, quota, identity or lifecycle failure
exits nonzero without a completed observation report.

Validation, semantic-check, profile-check, invocation and evaluation-admission
refusals have distinct payloads without execution fields. Actual evaluation
returns the status/value, chronological events, residual live owners, complete
live/tombstone ledger and successful transition count, including silent steps.
A returned live owner remains live. The endpoint never invents consumption
events or turns a static refusal into an empty dynamic trace.

Transport bounds and the frozen wire/API are in
[the contract](resource-model-endpoint42-contracts.md). The host and worker
both prepay a complete request, maximum response and 512-byte BYE reserve.
The process supervisor bounds pipe output and request/compilation/shutdown
time. Graceful retirement reserves half its remaining shutdown deadline for
kill, reap and pipe joins; requiring a kill still fails retirement.

The acceptance target uses the existing regular-copy Bazel launcher setting
because source attestation rejects leaf symlinks. It remains enabled on macOS;
its platform-sensitive framing and process checks are not a duplicate logical
proof replay. The source model, metatheory, kernel/proof profiles, existing
runtime/compiler/native providers, Cargo files and workflows are unchanged.

## Validation status

Component reports preserve their genuine worker and production-supervisor
probes, standalone codec checks, and exact source/artifact associations:
[worker](resource-model-endpoint42-worker.md),
[protocol](resource-model-endpoint42-protocol.md),
[process](resource-model-endpoint42-process.md), and
[acceptance](resource-model-endpoint42-acceptance.md).

Integrated acceptance, strict Clippy and CLI smoke validation are in progress.
No integrated pass is claimed here yet. The first integrated no-run attempt
failed because the shared build volume was full; its log was retained and
obsolete generated build artifacts were evicted before retrying. The next
compile identified an ambiguous test-local iterator sum, which is being fixed
without changing the model or protocol.

This is an executable observation endpoint for a finite model. It does not
establish source/model equivalence, native resource observation, whole-checker
or compiler verification, or a successful 100,000-program campaign. External
#453 owns the separate source/reference lockstep lane and #454 owns metatheory
extensions. Existing implementation-proof audit status remains unchanged.
