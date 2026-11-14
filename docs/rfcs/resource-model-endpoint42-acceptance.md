# Finite resource-model endpoint acceptance (#482)

This lane authors the new integration test and fixed request fixtures for the
existing finite resource model. It has not run Cargo, Lean, the endpoint,
Clippy or hosted CI. Genuine endpoint/CLI/transport execution and integrated
validation remain coordinator work. Static checks below do not establish a
successful mathematical observation or an executed control.

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
bytes, failing exit and blocked stdin. The blocked-write case is a bounded
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
coordinator owns the BUILD copy label, integrated CLI behavior, genuine
endpoint/control run, strict Clippy, workspace formatting and final source/
artifact associations. Existing model/metatheory/runtime/native, profile,
extractor/auditor/projector, Cargo and workflow sources are unchanged here.
The endpoint remains a model-only observation of the existing
`validate → checkProgram → validateInvocation → evalSteps` pipeline. It
claims neither source/native refinement nor a resource campaign or
whole-checker correctness, and leaves existing proof-audit status unchanged.
