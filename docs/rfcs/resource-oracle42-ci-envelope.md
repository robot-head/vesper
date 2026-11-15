# Resource endpoint test envelope

Issue #574 classifies the complete `test/resource_oracle42` Bazel target as `long` (900 seconds). Its previous medium-size default was 300 seconds. Every existing test and platform selection is retained.

The macOS job [132622570502](https://github.com/robot-head/vesper/actions/runs/43652086425/job/132622570502) at PR #423 head `31e3812dcc594108c9bfbe190f73f0242b5892d9` reported `TIMEOUT in 300.3s`. Seventeen of eighteen Rust tests had printed successful completion. The retirement-reserve test was unfinished when Bazel killed the target. The captured output contains no Rust panic, protocol failure, or individual harness deadline error.

Four genuine tests hold the existing `GENUINE_SESSION` mutex. Their fresh artifact roots and five original module compilations per start therefore run serially within this binary; the concurrent libtest “over 60 seconds” messages also include time waiting for that mutex. The genuine matrix, changed-object control, and abandonment control had completed at the outer deadline. Exact per-command timings and the source of extra host contention are not recorded by this log.

The same endpoint source, worker, five model modules, and integration-test bytes passed an earlier real macOS execution in 195.2 seconds (job 132581604551, head `36dc9844bb67dc25adf641ceabdba21563949ae5`). That earlier pass is historical evidence, not a rerun of the failed checkout. The later checkout differs in inherited proof/build coverage, so its wall-clock duration is not assumed equal.

This change affects only the outer Bazel target timeout. Production limits remain 120 seconds per compilation command, 30 seconds per request, and 5 seconds for shutdown. Synthetic-peer stall, blocked-input and BYE/EOF probes retain their explicit 300 ms limits and prompt-cleanup assertions. All source/compiled-object identity checks, fresh compilation, mutex behavior, semantic results, retirement accounting and all eighteen test bodies remain unchanged. No session or compilation artifacts are shared to shorten the test.

Validation is a before/after query of the actual wrapper with pinned Bazel 9.3.1, build-file synchronization, whitespace and complete tracked-object invariance checks. No heavy runtime test or compiler run is performed for this classification-only change. Fresh hosted completion of the whole target remains the runtime gate.

The actual pinned before/after queries completed successfully in 31.293 and 32.123 seconds. The wrapper XML gains only the explicit `timeout=long` attribute; its size, tags, inputs, copy mode and every other attribute are identical. Build-file synchronization and whitespace checks pass. Complete commit-tree comparison verifies that only this map entry/comment and this new report differ from base `2969db0103c3ee81e5c31ab17b9009e0ea85a42f`. Query stdout/stderr and comparison receipts are retained under `/tmp/resource-oracle42-ci-envelope574-validation`.
