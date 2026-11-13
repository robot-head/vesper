# Independent executable arithmetic oracle acceptance (#274)

This lane follows frozen contract
`cae1bc5b50ebe7cd448fd9a18ff87f9f4c1ec6c2`. Exclusive files are
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
* Direct real-driver sessions check byte-fragmented requests, all wire quotas,
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
the separately owned 900-second Bazel timeout.

## Execution evidence

Writer checks: rustfmt and git diff --check only. No Cargo build, Rust test,
Lean compilation or actual oracle execution has been performed by this lane.
Coordinator execution against coherent integrated model/corpus/runner sources
is pending. Authored assertions are not passing execution evidence.

## Scope

Passing establishes agreement only on actually completed programs and execution
settings. It does not prove the AST binding, checker/compiler refinement or
the executable Lean checker's refinement of source typing. Hashes associate
observations; they are not proof certificates. Worker recycling and logical
caps are not whole-process RSS claims. The explicit Full100000 run is not part
of this routine acceptance target and no 100000-program/800000-outcome
completion, cross-platform execution, performance or full §42 completion is
claimed without separate actual evidence.
