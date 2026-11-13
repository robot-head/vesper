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
The report's lean_release is compared with the receipt's exact pinned
`leanprover/lean4:v4.24.0` field; the machine handshake's `4.24.0` version
spelling is distinct. After all positive report and artifact assertions pass,
the test logs one bounded `LEAN4LEAN42_REPORT` line containing that actual
parsed CLI report, permitting durable evidence extraction without repeating
either mathematical replay. No report is fabricated from counters or emitted
as a substitute for successful execution.

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

The writer ran rustfmt and git diff --check. Under a separate explicit
coordinator lease, the writer also compiled and executed the transport-only
harness described below. No Cargo command, Lean build, setup seal, complete
model replay or full acceptance execution has been performed by this lane.
The prepared root and full positive execution remain pending. Authored
full-matrix tests are not a claim that the matrix passes.

### Executed independent transport probes

The temporary `/tmp/lean4lean42-acceptance-transport.rs` imported the actual
integrated driver module and copied the exact owned `transport_faults`
function. The integration HEAD observed at compilation was
`1020a85948125e5f24d2ed01d24f59e291e7ac20`. Its peer was the real
`/workspace/vesper/target/parallel-next/debug/lean4lean42` executable, after
the coordinator's `/tmp/lean4lean42-root-build2.log` recorded a completed
2.56-second build. The harness used Rust 1.99.1 and existing cached rlibs;
it did not invoke Cargo or compile/install dependencies.

Actual commands, with compilation output captured separately:

```sh
taskset -c 0 rustc --edition=2024 --cfg test -C debuginfo=0 \
  /tmp/lean4lean42-acceptance-transport.rs \
  --crate-name lean4lean42_acceptance_transport \
  -L dependency=/workspace/vesper/target/parallel-next/debug/deps \
  --extern serde=/workspace/vesper/target/parallel-next/debug/deps/libserde-a1ebe81f48fb296d.rlib \
  --extern serde_json=/workspace/vesper/target/parallel-next/debug/deps/libserde_json-302f6e6a62713afe.rlib \
  --extern blake3=/workspace/vesper/target/parallel-next/debug/deps/libblake3-8ece2d105e9c83d1.rlib \
  -o /tmp/lean4lean42-acceptance-transport
taskset -c 0 /tmp/lean4lean42-acceptance-transport
```

Both commands exited0. The standalone cfg(test) compilation emitted two
unused internal-test-helper warnings; this was not a strict Clippy invocation.
Execution passed all eleven real child-process probes and invalid-cap
preflight in 1.756 seconds (wrapper wall time 1.775 seconds), with CPU affinity
restricted to one CPU. The assertions checked actual child reap and reader/
watcher joins, retained frame/stderr bounds, zero payload allocation for an
oversize header, queue peak4 under pressure, all64 pressure markers, and real
deadline cancellation while the queue was full. No kernel/control/replay
success was supplied by a peer.

Logs are `/tmp/lean4lean42-acceptance-transport-build.log`,
`/tmp/lean4lean42-acceptance-transport-run.log`, and
`/tmp/lean4lean42-acceptance-transport-result.json`. This is executed local
Linux transport evidence only, not a genuine two-model replay or a full
acceptance result. The mandatory prepared-root test was not invoked.

### Coordinator compile and initial matrix attempt

The coordinator compiled the actual acceptance target successfully with
`cargo test -p vesper --locked --test lean4lean42_acceptance --no-run`
(11.64s). An initial target execution passed eleven imported unit checks and
the eleven transport probes, then stopped before any genuine replay because
the source-only inventory fixture omitted Eq. The strengthened production
guard correctly requires an eligible form4 Eq and its earlier group coverage
before quotient initialization. The fixture now includes that prerequisite
and event, with coherent addition/coverage totals; the wrong-quotient-group
negative establishes Eq first so it still tests its intended fault. The
synthetic terminal remains deliberately unresolved, with no replay success
claim. The actual log is `/tmp/lean4lean42-root-acceptance.log`.

This correction is unexecuted at this report revision. The genuine assembled
model replay and newly sealed setup remain separate pending coordinator work.

## Scope

Complete external replay would establish the stated eligible fresh-closure
checking and binding/control conditions for the two fixed models. It would
not establish axiom hygiene, complete Lean4Lean implementation proofs,
Rust-to-model/refinement correctness, full-language checking, IO/JIT semantics,
bootstrap/DDC or full §42 completion. Hashes associate observations rather
than proving machine-code derivation. Logical buffers/deadlines and observed
cleanup do not prove whole-process RSS or interruptible allocator/filesystem
operations. No platform or hosted-CI result is claimed without execution.
