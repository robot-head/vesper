# Lens cancellation regression (#305)

This is a test-only correction on clean main
`20ee620474ac79f706e23d883dfa47c17c75560b`. Production Lens cancellation,
proof/runtime code, Cargo metadata, and external replay paths are unchanged.

## Reproduced failure

The exact baseline command was:

```sh
CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test -p vesper --locked --lib lsp::lens_tests::proof_lens_cancellation_stops_the_whole_process_tree -- --exact --nocapture
```

It failed with exit 101 after 10.05 seconds of test execution, reporting
`grandchild 735270 survived cancel`. An independent
`ps -o pid=,ppid=,stat=,comm= -p 735270` observation showed
`735270 1 Z sleep`. The descendant had terminated and become an unreaped zombie;
the old `kill -0` check still succeeded because the PID remained present.
This was a false assertion of a surviving live descendant, not evidence that
the sleeping process continued to execute.

## Changed assertion and actual controls

The helper now queries `/bin/ps -o stat= -p PID`, supported by Linux and
BSD/macOS ps, and distinguishes an absent process or state `Z` from every other
reported state. In particular sleeping `S`, stopped `T`/`t`, running and unknown
present states remain alive. An unexpected ps failure is a test error, not a
successful cancellation assertion. The cancellation fixture still forks the
same grandchild and requires it to be live before cancellation; its existing
cancellation response and ten-second termination checks are retained.

A separate real-process control owns a sleeping child, observes `S`, sends
SIGSTOP and observes `T`, kills it without waiting, observes `Z`, then waits/reaps
it and checks absence. The Linux branch also verifies that `kill -0` still
succeeds for that zombie, without imposing that Linux behavior on other Unix
hosts. It asserts that the helper counts `S`/`T` alive and `Z`/absence dead. A drop guard kills and
waits for this direct control child even if an assertion fails.

## Final focused validation

Each command used the exact Cargo environment above and
`cargo test -p vesper --locked --lib TEST -- --exact --nocapture` with the
following complete test names. Every invocation passed one test with zero
failures or ignored tests:

| Test | Execution | Log |
| --- | --- | --- |
| `lsp::lens_tests::proof_lens_cancellation_stops_the_whole_process_tree` | 0.06 s | `/tmp/lens305-tree-fixed.log` |
| `lsp::lens_tests::proof_lens_process_state_distinguishes_live_stopped_and_zombie` | 0.11 s | `/tmp/lens305-state-final.log` |
| `lsp::lens_tests::proof_lens_cancellation_answers_once_and_keeps_not_attempted` | 0.71 s | `/tmp/lens305-response-final.log` |

The final tree invocation rebuilt the current source in 16.52 seconds. The
baseline failure log is `/tmp/lens305-baseline.log`. An earlier run of the real
state control also passed in 0.10 seconds, logged in
`/tmp/lens305-state-controls.log`.

With the same Cargo environment,
`cargo clippy -p vesper --locked --lib --tests -- -D warnings` passed (exit 0,
`/tmp/lens305-clippy.log`). This checks the changed test code; it does not execute
the expensive external replay tests. `rustfmt --edition 2024 --check
src/lsp/lens_tests.rs` and `git diff --check` also passed.

The state distinction is not a claim that Lens reaps an adopted grandchild.
The original tree fixture can leave a dead zombie for PID 1 to reap; the helper
now describes that state accurately. Execution evidence here is Linux only.
The Unix/macOS branch is source-supported, with no macOS or Windows execution
claimed. Formatting and diff checks passed.
