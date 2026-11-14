# Finite resource-model endpoint acceptance (#482)

This report records the initial endpoint checkpoint, which used a Rust
negative-peer fixture. The current test-only peer uses the existing CI
Python prerequisite and requires no runtime Rust compiler; see the
[portability follow-up](resource-model-endpoint42-portability.md) for its
reproduction, current fixture and fresh validation. Production Lean/model
and protocol/supervisor sources are unchanged by that follow-up.

Genuine integrated endpoint acceptance passed on
`e048f411e1e33543387a806fd1ddb8c528dddf6f`: ten binary protocol tests and
eighteen integration tests, with no failed or ignored tests. The integration
suite completed in 90.97 seconds; the binary tests took 0.22 seconds. The
actual persistent worker completed thirty-eight requests covering all six
stages, then confirmed exact BYE, stdout EOF and successful exit. Strict
targeted Clippy and workspace formatting also passed. Hosted CI and a larger
resource campaign are not claimed.

The actual stage counters are `[38,4,7,2,5,1,19]`: requests, validation
errors, semantic refusals, profile refusals, invocation errors, evaluation
admission errors and evaluations. Framed traffic was 8,549 bytes, with
transcript BLAKE3
`906778a597409df8c0eb418f3e92edd1930a6c32caf8efda2affc37a6de61ecd`.
The retained evidence is
`/tmp/resource-model-endpoint42-root-session-evidence.json`; complete output
is `/tmp/resource-model-endpoint42-root-tests-v2.log`. Every authored
positive assertion and all eighteen Linux hostile transport controls ran
against the production paths described below.

The session attests ten exact project source/object identities and nine
commands, all with exit code zero. The original executable
`ResourceOracle42.olean` is 1,744,976 bytes, BLAKE3
`134f2b53ea1e9674a4de669b8c7e128ca0d3cbdf56b8adb1c9df958f8ce1d1d9`.
The compiler is genuine Lean 4.24.0, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, executable BLAKE3
`ffb9dc0e90bae095f4dee95346eec8bdfad04204b219ededce15788e55cfce06`.
Standard-library objects and shared libraries come from that pinned prefix;
they are not individually attested. The worker is transport code rather
than an externally verified implementation proof, and no kernel projection
substitutes for these executable objects.

`tests/resource_oracle42.rs` includes the production endpoint module through
`#[path = "../src/bin/resource_oracle42/mod.rs"] pub mod oracle;`. The public
module keeps production declarations visible to normal compiler/lint checks.
Positive cases call the genuine `process::start`, typed `Session::evaluate`
and consuming `Session::finish` API. Missing pinned Lean is a test failure,
never an ignored test or synthetic replacement. A test mutex serializes the
genuine compilation/session cases.

The eighteen JSON fixtures in `tests/fixtures/resource_oracle42/` are complete,
sequence-zero requests in the fixed schema. They contain only the default
fourteen model limits, postfix type/source/value constructor codes, invocation
values and fuel. They contain no acceptance flag, inferred type, usage mask,
proof, trace, expected outcome, callback, module selector or Lean source.
Production decoding supplies typed requests; production encoding supplies the
session sequence. The fixture set covers unit, natural, copy inputs, returned
ticket, mint/consume, sequential resources, branch mints, unpacked scalar and
owner pairs, dynamic growth, all seven semantic check kinds and an affine
branch profile refusal. Test-local changes to limits, values and fuel exercise
the same production request path.

The authored genuine matrix checks:

- Actual unit and largest-u64 natural returns, input-zero substitution order,
  closing-let fuel and changed invocation values with identical static
  checked data.
- A returned live ticket with its mint event, live residual and ledger; exact
  mint/consume transitions including silent actions; zero and predecessor
  fuel; and terminal-first behavior with zero fuel/resource/event capacities.
- Mint/consume/mint/consume identity freshness, preserved consumed tombstones,
  both branch values without eager effects, scalar unpack order and owner
  unpack consumption order without reminting.
- Distinct validation, semantic-check, profile-check, invocation,
  eval-admission and evaluation stages. Semantic diagnostics check kind,
  site and optional slot. Invocation negatives cover count, shape, supplied
  handle, a representable natural above u64 and aggregate initial capacity.
  Fuel admission is distinct from evaluation fuel exhaustion.
- Genuine resource, event and dynamic-value capacity prefixes, preserving
  prior chronological events, exact action count, residual owners and full
  live/tombstone ledger without invented cleanup. The larger exact dynamic
  value budget completes the same program.

Successful evaluation assertions use typed `Outcome`, `Evaluation`, `Status`,
`Event`, `ResidualOwner`, `LedgerEntry` and constructor-code DTOs. Refusal
branches match their typed stage-specific payloads, which have no execution
field. Negative codec controls also reject an appended execution payload on a
refusal, unknown enum constructors and extra fields. The successful session
must return confirmed complete evidence with exact host request/stage counts,
partitioning counters, a transcript hash, bounded framed bytes and nonempty
artifact identities. There is no completed report from an unconfirmed
retirement.

The production codec controls cover fixed-request round trips, every short
header/payload read/write boundary, boundary EOF, every nonempty truncated
prefix, zero and oversized frame lengths, forbidden lexical forms, natural
canonicality, semantic-range-independent natural transport, malformed
postfix arity, non-atom operands, multiple roots, depth/row/input-count caps,
unknown tags and exact diagnostic/message payload shape. They do not call a
second model evaluator.

The standalone std-only `fault_peer.rs` fixture is exclusively a hostile
transport peer. The test builds it with a finite thirty-second/64-KiB-output
rustc bound, then calls the process owner's `cfg(test)` negative-only
`test_transport_failure` seam. That seam uses production READY, framing,
request, BYE, EOF, exit and cleanup machinery and cannot return a mathematical
response or `SessionEvidence`. Authored controls cover wrong READY, stdout
noise, malformed/unknown payloads, reordered sequence, oversized/zero frames,
truncated header/payload, stall, stderr flood, crash, bad/missing BYE, trailing
bytes, failing exit, blocked stdin and a valid BYE/EOF followed by a still-live
direct child. That Unix control requires Timeout, bounded kill/reap/join and
an unchanged cleanup-failure counter. The blocked-write case is a bounded
request with sixty-four independent balanced input trees: 131,008 value
rows plus one source row, more than 256 KiB of request bytes, within the
fixed transport row/depth quotas. No synthetic peer supplies a successful
mathematical case.

Fixed-source controls copy only the five production source paths into a
fresh regular directory, then change, remove, replace by a directory or
oversize the worker source. A Unix-specific control substitutes a symlink
with identical target bytes. These must fail fixed-source identity before a
model session exists. A genuine compiled-object control mutates
`ResourceOracle42.olean` before requesting evaluation and requires artifact
identity refusal with no response or completed evidence. Another genuine
session is deliberately dropped after READY: its abandonment counter must
increase, its private epoch must be removed after bounded cleanup, and there
is no returned completed evidence.

The retirement-budget control uses a genuine READY and a lower-only session
wire limit one byte below the next request frame, maximum response frame and
fixed 512-byte BYE reserve prepayment threshold. It requires refusal before
evaluation and no evidence from a failed epoch. The actual READY bytes are
included in the threshold. The bound is independent of the model limits and
does not weaken any schema or semantic check.

Static checks performed in this lane:

- All eighteen JSON fixtures parsed with the exact eight request fields and
  default limit vector. Postfix stacks had exact arities, atom-only operands
  where required and exactly one final root. Every fixture was within the
  request-byte, independent input-count, per-code-row, aggregate-row and
  constructed-depth bounds. This was a schema/structure inspection only.
- `rustfmt --edition 2024 --config skip_children=true --check tests/resource_oracle42.rs tests/fixtures/resource_oracle42/fault_peer.rs`
  passed. `skip_children` confines this lane's formatter to its owned files.
- `git diff --check` passed.

The first static fixture check observed request sizes of 85–193 bytes, one
to fifteen source rows, source depth one to seven and at most fifteen total
constructor rows. No execution pass is inferred from these counts. The
coordinator owns the BUILD copy label, integrated CLI behavior and final
source/artifact associations. The acceptance lane subsequently ran the real
endpoint/control suite, strict targeted Clippy and workspace formatting under
the exclusive Cargo and one Lean compiler leases. Existing model/metatheory/runtime/native, profile,
extractor/auditor/projector, Cargo and workflow sources are unchanged here.
The endpoint remains a model-only observation of the existing
`validate → checkProgram → validateInvocation → evalSteps` pipeline. It
claims neither source/native refinement nor a resource campaign or
whole-checker correctness, and leaves existing proof-audit status unchanged.


The successful commands used Rust/Cargo stable 1.99.1 with
`CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next`,
`CARGO_BUILD_JOBS=1`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`,
`VLB_CARGO_LOCK=/workspace/vesper-worktrees/resource-model-endpoint42-integration/Cargo.lock`,
`LEAN_NUM_THREADS=1` and
`VESPER_LEAN=/home/agent/.local/share/vesper-lean/bin/lean`, with no
`RUSTFLAGS` or temporary source patch. The final commands were:

- `cargo test --locked -p vesper --bin resource_oracle42 --test resource_oracle42 --no-run`
  passed in 14.80 seconds before the final fixture correction; the final
  actual test command rebuilt the corrected test in 1.84 seconds.
- `cargo test --locked -p vesper --bin resource_oracle42 --test resource_oracle42 -- --nocapture`
  passed all ten binary and eighteen integration tests. The integration test
  includes ten protocol unit tests through the same public module and eight
  endpoint/transport acceptance tests.
- `cargo clippy --locked -p vesper --bin resource_oracle42 --test resource_oracle42 -- -D warnings`
  passed in 1 minute 41 seconds without suppressions or compatibility patches.
- `cargo fmt --all -- --check` and `git diff --check` passed.

Failures remain recorded rather than replaced by the successful retry. The
initial root build exhausted disk while creating a generated Cranelift
archive. Root cleanup and eviction of 494,441,856 bytes of thirty-two stale
generated test executables freed space; no source, evidence, library or
current endpoint object was evicted. The v2 build reached the new test and
reported an ambiguous counter sum, fixed by explicit `sum::<u64>()` in
`b026e25`. The first actual test run encountered the deliberately mutated
private object being production-sealed read-only; its unwritable fixture
then poisoned the serialization mutex. `e048f41` enables owner writing only
for that corrupted private fixture and recovers the serialization guard so
one failure cannot prevent independent genuine cases. Production code and
model semantics were not weakened. The successful retry exercises actual
byte corruption and observes `ArtifactIdentity` before any model response.
Failure logs remain at `/tmp/resource-model-endpoint42-root-build.log`,
`/tmp/resource-model-endpoint42-root-build-v2.log` and
`/tmp/resource-model-endpoint42-root-tests-v1.log`; successful build, test,
Clippy and format logs use the corresponding `build-v3`, `tests-v2`,
`clippy-v1` and `fmt-v1` names. The BYE/EOF live-child control was added in
`c51c71c` and passed with the process owner's bounded-cleanup correction.

Source comparison against frozen main
`23528ac37145b5389d54b64198edb0daf3633224` confirmed that
`MetatheoryFull.lean` and the existing ResourceModel42 Syntax, Checker and
Evaluator sources are unchanged. No model/metatheory/runtime/native,
profile/extractor/auditor/projector, Cargo or workflow source was edited by
this acceptance lane. Root reports CLI smoke and production launcher topology
separately; these endpoint results do not imply a source adapter, native
refinement or a 100,000-program campaign.
