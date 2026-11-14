# Negative-peer semantic startup headroom (#545)

At #498 head `61cde52a869b1f7932f53bc6ae0a05c232655c35`,
[Windows job 132472444556](https://github.com/robot-head/vesper/actions/runs/43603289173/job/132472444556)
passed strict Clippy but failed the hostile-peer test at
`tests/resource_oracle42.rs:898`: `wrong_ready` expected `Protocol`, received
`HarnessError { kind: Timeout, detail: "worker deadline" }`. The endpoint
test binary reported sixteen passes and one failure. This establishes a
deadline/category mismatch, not a diagnosis of Windows interpreter startup.

The sixteen-case fixture loop now gives its fifteen non-timing semantic
fault cases a 2000 ms request/READY deadline. The intentional `stall` case
keeps 300 ms. Every shutdown deadline remains 300 ms; the separate blocked
stdin and Unix BYE/EOF/live-child timing probes retain 300/300 ms. Exact
expected categories, cleanup assertions below five seconds, and cleanup
failure accounting remain unchanged. Timeout is never accepted in place of
Protocol, Sequence, Quota, CounterMismatch or Crash.

`Conversation::attach` starts the READY deadline after transport creation
using `request_timeout_ms`. The Python peer imports `struct`, `sys` and
`time`, then its first `wrong_ready` branch immediately writes and flushes
the invalid identity, without an intentional sleep or input read. Semantic
fault checks therefore need enough time to reach that response. Two seconds
is bounded fixture headroom, not a measured Windows minimum or a guarantee.
Production defaults remain 30000 ms request and 5000 ms shutdown, with their
existing hard limits. No production source, peer code, proof policy or
300-second proof budget changed.

## Actual local validation

Rust/cargo 1.100.0 and rustfmt 1.11.0 were used. Strict Clippy passed in
2.15 seconds through a temporary minimal package pointing directly at the
actual child CLI and integration-test sources, with the real pinned serde
1.0.229 (`derive`, `rc`), serde_json 1.0.154 and blake3 1.8.7 dependencies.
Targeted formatting, BUILD synchronization and whitespace checks passed.
A byte comparison confirms that only the one fixture expression and its
two-line comment changed in Rust; all other test bytes are identical.

The actual complete integration-test source was separately compiled with
Rust 1.100.0, using those recorded dependency artifacts and
`CARGO_MANIFEST_DIR` set to this checkout. Only
`hostile_peers_use_production_process_and_cannot_return_mathematical_evidence`
was executed. Both Linux runs passed all sixteen table cases plus the two
Unix timing controls, reporting one passed test and seventeen filtered:

- `PATH=/usr/bin:/bin`, `RUSTC`, `PYTHON` and `VESPER_LEAN` unset: 1.39 s.
- The same rustc-free PATH with explicit `PYTHON` naming an interpreter path
  containing spaces: 1.27 s.

Exact compile/run commands, environment, dependency/source/binary hashes and
logs are retained under
`/tmp/resource-endpoint42-peer-startup545-validation/`; dependency selection
comes from `/tmp/vesper-endpoint-ci-build-1.100-artifacts-v1.jsonl`.
This is exact-source direct-Rust harness coverage, not a new Bazel build or
whole-workspace run. No Lean, endpoint CLI or genuine model case executed.
No local Windows runtime result is claimed; the hosted Windows retry must
confirm the fixture estimate. No inaccessible Linux CI cause is inferred.
