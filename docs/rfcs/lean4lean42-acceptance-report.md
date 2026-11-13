# Independent external replay acceptance (#302)

This lane follows frozen base `43b1d40696b890d9a9918f25ab4e7332e4fdb031`
and the published stored-metadata/test-seam amendments `859429c` and
`e38f4b5`. Exclusive writes are `tests/lean4lean42_acceptance.rs`,
`src/bin/lean4lean42/fault_peer.rs`, and this report. Driver, protocol,
process, setup, pins, kernel/control and machine changes belong to other lanes.

## Authored matrix

The default integration test requires the actual prepared candidate named by
`VESPER_LEAN4LEAN42_ROOT`. Missing setup is a hard failure, never a skipped
pass. One genuine CLI operation replays both complete eligible fresh model
closures. Additional checks use focused production parser/binding failures,
not repeated full successful replays or canned checker replacements.

The positive checks require complete RewriteWitness then ArithmeticOracle,
native compilation and external exit0, all eligible IDs covered, nonzero
target-owned coverage, exact 33/37 required-root coverage, successful
constructor/recursor comparison counts, exact unsafe/partial skips and zero
pending/remaining work. Historical 86k observations are not golden totals.
Source, receipt, pin/control, compiler/build/control-import and stream digests
remain distinct. Both separate audit statuses must remain not_run.

All 94 registry entries are compared in pinned order by ID, category,
expectation and actual result/reason, with an independently computed typed
controls hash. This includes closure FVar/MVar refusals and closed positives,
plus the five genuine shared metadata conditions. The latter are condition
checks, not raw kernel refusals or new implementation proofs.

Focused source-only native inventories exercise the production Machine parser
and coverage state: missing/dotted/incorrect-origin/skipped required roots,
changed repeated inventory, duplicate/unknown IDs, wrong groups/actions,
constructors before their owners, skipped declarations, sequence faults and
unresolved terminal work must fail. Their generic counters are kept coherent
so root mutations cannot pass by merely triggering a name-byte discrepancy.
These are rejection fixtures, never actual external checking evidence.

Eleven closed portable peers test malformed/token/length/truncation faults,
crash, stall, stderr overflow, bytewise marker writes, a 64-marker bounded-queue
pressure completion, and cancellation under queue pressure. Only transport
markers can complete; no peer supplies replay/control success evidence.
Production process observations must show actual child reap and stdout/stderr/
watcher joins, frame/stderr caps and at most four queued complete frames.

Binding negatives use a private regular-file copy of the genuine candidate.
Unchanged files may be hard-linked; mutations atomically replace directory
entries, and restoration preserves permissions. The original prepared root is
not mutated. Production source verification checks the clone before/after
source, extra input, generated control artifact and receipt-claim mutations.
Failures cannot claim external coverage. Incorrect limits, absent candidates,
unmatched compiler overrides and unsupported CLI options must also fail.

## Execution evidence

Writer checks are rustfmt and git diff --check only. No Cargo command, Lean
build, setup seal, complete replay or acceptance execution has been performed
by this lane. The actual pins/assembled source are coordinator-owned; the
prepared root and positive execution lease are pending at this checkpoint.
Authored tests are not a claim that the matrix passes.

## Scope

Complete external replay would establish the stated eligible fresh-closure
checking and binding/control conditions for the two fixed models. It would
not establish axiom hygiene, complete Lean4Lean implementation proofs,
Rust-to-model/refinement correctness, full-language checking, IO/JIT semantics,
bootstrap/DDC or full §42 completion. Hashes associate observations rather
than proving machine-code derivation. Logical buffers/deadlines and observed
cleanup do not prove whole-process RSS or interruptible allocator/filesystem
operations. No platform or hosted-CI result is claimed without execution.
