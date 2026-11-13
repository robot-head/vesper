# Independent executable arithmetic oracle acceptance (#274)

This lane follows frozen contract
`cae1bc5b50ebe7cd448fd9a18ff87f9f4c1ec6c2` and published bounded-carrier
amendment `27d531e`. Exclusive files are
`tests/oracle42_acceptance.rs`,
`src/bin/arithmetic_oracle42/fault_peer.rs`, and this report. Model, corpus,
runner, binary registration and CI changes belong to other claims.

## Authored checks

One serial Rust test drives the real Cargo oracle binary, public runner API,
production transport probes, and the actual pinned Lean 4.24.0 static driver.
Missing or wrong default/explicit Lean fails; there is no skip or stale-binary
fallback. Bare Cargo binary names are resolved through PATH before canonical
file hashing, with Windows executable handling.

* The real CLI runs default CI128 with artifacts, and the public API repeats
  CI128 with two workers. Each run must complete 128 valid programs and all
  15 isolated negatives, with 143 parser calls, 269 checker calls, 128 AST
  optimizer calls, and all four evaluator plus four JIT settings executed
  for each valid program. The jobs1 run must spawn five workers and recycle
  four, counting the negative frames toward the 32-case cap.
* Exact kind/operation/diagnostic coverage, zero failure counters, separate
  disabled shrink counters, one Lean session per run, and actual executable,
  static model and driver hashes are checked. The Lean-only request/response
  transcript hashes must agree across worker schedules.
* An independent semantic fingerprint encoder and corpus digest are compared
  with the actual generated DTOs. All parsed non-parser-negative sources must
  normalize back to those DTOs. Changed literals and additional functions must
  fail source binding. Case IDs cannot serve as program uniqueness.
* The private fault peer has ten closed, portable modes: malformed response,
  oversize header, truncated header/payload, stall, stderr flood, reordered ID,
  bad BYE, short writes, and crash. It supplies no arithmetic outcome or run
  report. The short-write mode emits only the fixed transport marker.
  Production probe reports must show actual classifications, child wait/reap,
  all pipe-thread joins, and bounded retained buffers.
* Independent framed clients compile the exact bundled static model/driver
  once and query real Lean for all 15 static negatives, strict unused local
  overflow, a later static error after an overflowing initializer, differently
  typed unused inputs, unknown runtime values, callee resolution and fuel.
  Actual Lean answers are compared with real Rust evaluator/JIT answers;
  Rust arithmetic never supplies expected corpus outcomes.
* Direct real-driver sessions check byte-fragmented requests, wire-quota failures,
  malformed lexical/JSON/schema/profile data, sequence consumption, fuel zero,
  clean terminal counters, EOF, truncated frames and fatal sequence errors.
  Private i128 serde must use exact canonical strings, including parser-negative
  values above u64. Response schema/code/ID/arity/canonical-decimal corruption
  must be refused.
* Configuration and CLI errors, absent/wrong Lean, absent/foreign workers and
  artifact quota failure must remain failures with honest partial counters.
  Existing artifact files are preserved, listed hashes match actual contents,
  total stored bytes stay bounded, and summary files never hash themselves.

Subprocess buffers and direct-client channels are bounded. Compiler setup uses
300-second guards, direct driver responses use five seconds, and cleanup
kills/reaps before joining pipe threads. Production fault probes use their
own OS deadlines and the production transport implementation. Main execution
is serial, with at most two configured Rust workers in a successful run.
Per-command and whole-target wall time are printed for coordinator review of
the separately owned 900-second Bazel timeout. The actual final acceptance
target took 50.86 seconds on the available Linux host. This observation does
not establish timing on other platforms or a universal workload bound.

## Execution evidence

The writer authored the checks, ran rustfmt and git diff --check, and reviewed
the coordinator's actual execution logs. All Cargo builds, Rust tests, Lean
compilation and oracle execution below were performed by the coordinator,
not this lane.

| Revision and evidence | Actual coordinator result |
|---|---|
| Integration `f1b4cde`, `/tmp/oracle42-root-acceptance-final3.log` | 11 passed, 0 failed, 0 ignored; complete serial acceptance matrix and imported private carrier/process checks; 50.86 seconds |
| Same acceptance run, real CLI with jobs1 | CI128 plus all 15 negatives passed in 15.00 seconds, including exact per-mode counts, artifact checks, five workers/four recycles and one Lean session |
| Same acceptance run, real public runner with jobs2 | CI128 plus all 15 negatives passed; exact counts and Lean-only transcript hashes matched the jobs1 run |
| Same acceptance run, direct static driver and portable peers | Exact model/driver compiled, all direct semantic, malformed/fatal framing, fuel, terminal-counter and ten portable fault-mode assertions passed |
| Integration `3e0653e`, `/tmp/oracle42-root-corpus-runner-final.log` | Corpus and runner targets each passed 20 tests: 40 passed, 0 failed, 0 ignored, in 0.84 and 4.73 seconds respectively |
| Coordinator-reported `c5150f5`, `/tmp/oracle42-root-clippy-final.log` | Strict all-target Clippy passed; log records 19.33 seconds |

The corpus/runner and Clippy revisions precede the last independent-client
test fix; they are identified separately rather than relabeled as checks of
the later revision. The coordinator also reported the model-CI check passed
before the harness-only followups. The final acceptance itself independently
compiled the exact bundled RewriteWitness and ArithmeticOracle bytes with
actual Lean 4.24.0 and executed the static driver.

Both successful CI runs use the same 128 distinct programs, repeated under
different worker schedules. They are not 256 unique programs. Each run confirms
128 actual executions in every evaluator/JIT array slot; additional direct
probes are kept separate from those corpus counters.

Earlier failures were acceptance corrections, not compiler defects: a wrong
executable's real nonzero exit is a valid hard tool rejection, and an empty
code array receives the model's exact profile_limit category. The independent
test client's immediate close also raced its one-slot frame queue; dropping
the sender now drains the queued bytes before closing stdin. Boxed-array
assertions follow the published carrier amendment. None of these changes
relaxes actual failure, completion, outcome, terminal or cleanup assertions.

The coordinator subsequently completed the release build and Full100000 run
at source `8e2f653e91729e1e893d65432ef15554f628d639`: 100000 distinct valid
programs, all 15 negatives and all 800000 backend outcomes passed. The full
root-package regression also passed 946 tests across 78 result records; the
51 focused tests are included in that total. Final strict Clippy, formatting
and static-model CI passed. See the [integration report](oracle42-integration-report.md)
and [unmodified full-run JSON](oracle42-full100000-report.json) for the exact
configuration, counters, timing and coordinator execution provenance.
No Bazel, BuildBuddy, macOS or Windows execution is established by these logs.

## Scope

Passing establishes agreement only on actually completed programs and execution
settings. It does not prove the AST binding, checker/compiler refinement or
the executable Lean checker's refinement of source typing. Hashes associate
observations; they are not proof certificates. Worker recycling and logical
caps are not whole-process RSS claims. The separately completed Full100000 run
is outside this routine acceptance target. Neither that run nor these focused
checks establishes cross-platform execution, performance guarantees or full
§42 completion; the integration report states the remaining scope.
