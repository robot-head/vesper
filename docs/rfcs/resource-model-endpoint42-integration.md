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

The integrated code at `e048f411e1e33543387a806fd1ddb8c528dddf6f` passed:

- Ten binary codec tests (0.22 seconds) and eighteen integration tests
  (90.97 seconds), with no ignored or failing cases. The integration target
  also includes the ten codec tests; these are not twenty-eight distinct
  mathematical cases.
- The genuine 38-request matrix, with counters
  `[38,4,7,2,5,1,19]`, 8,549 framed bytes, exact retirement and transcript
  BLAKE3 `906778a597409df8c0eb418f3e92edd1930a6c32caf8efda2affc37a6de61ecd`.
  All ten original project source/object identities were recorded and all
  nine tool/worker commands exited zero.
- Eighteen negative peer controls, including blocked stdin and valid BYE
  plus stdout EOF followed by a still-live direct child; source/object
  corruption, abandonment, and retirement-reserve refusal controls.
- Strict targeted Clippy with no suppressions or source patches:
  `cargo clippy --locked -p vesper --bin resource_oracle42 --test resource_oracle42 -- -D warnings`
  (1 minute 41 seconds).
- Eight actual CLI cases. Help succeeds; missing/unknown arguments,
  directory input, nonzero initial sequence and forbidden lexical input
  fail without stdout. Genuine mint/consume evaluation (21.418 seconds)
  and unused-linear semantic refusal (23.450 seconds) both return verified
  complete sessions with correct counters and the same ten artifact
  identities. The refusal has no execution payload. The evaluation reports
  mint then consume of identity zero, three transitions and a dead tombstone.
- The unchanged production Bazel launcher with a separately compiled test
  using `CARGO_MANIFEST_DIR="."` and a tree of actual leaf symlinks. Without
  regular copying, the genuine test fails immediately with SourceIdentity
  and no session evidence. With copying, the same 38-request matrix passes
  (24.399 seconds), as do the hostile peer controls (1.218 seconds). Copied
  inputs were independently checked to be regular files. This exercises
  the launcher's topology; it is not a local full Bazel build.

The executed worker object is 1,744,976 bytes with BLAKE3
`134f2b53ea1e9674a4de669b8c7e128ca0d3cbdf56b8adb1c9df958f8ce1d1d9`.
Worker source SHA256 is
`3d6e4f99a46e51e95511a4fd69a993a86167a32d2002b12fa31c4996d7b31e41`.
The actual CLI binary SHA256 is
`cfa886d7aacfb0887b0c685cd1e06e309e500148c0ac2a6d11f96e5d00b0a20b`.

Local receipts and retained reports are under
`/tmp/resource-model-endpoint42-cli-v1/` and
`/tmp/resource-model-endpoint42-runfiles-v1/`; the integrated matrix evidence
is `/tmp/resource-model-endpoint42-root-session-evidence.json`.
These paths identify local evidence, not repository fixtures or remote CI.
Hosted platform checks remain pending.

Failures were retained before retrying: the first no-run attempt exhausted
the shared build volume; obsolete generated artifacts were evicted. The
next compile found an ambiguous test-local iterator sum, fixed with an
explicit `u64`. The first integration attempt correctly encountered a
read-only sealed object when the corruption test tried writing it. The
test now deliberately grants owner-write permission only to its private
fixture before corruption, and its mutex recovers from poisoning. No model
or protocol acceptance rule was changed to obtain a pass.

This is an executable observation endpoint for a finite model. It does not
establish source/model equivalence, native resource observation, whole-checker
or compiler verification, or a successful 100,000-program campaign. External
#453 owns the separate source/reference lockstep lane and #454 owns metatheory
extensions. Existing implementation-proof audit status remains unchanged.
