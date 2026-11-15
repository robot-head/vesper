# Nursery cancellation timeout diagnostics

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

Issue #577 follows the user-confirmed Linux timeout of
`nursery_cancellation_drains_every_in_flight_request` in #407 at
`2969db0103c3ee81e5c31ab17b9009e0ea85a42f`, invocation
`0056585e`. The available failure block does not identify the backend or the
blocking phase. This change adds test diagnostics; it does not establish or
repair the hosted timeout's cause.

The fixture starts four reads on empty pipes, retains their writers, queues a
fifth child, cancels, and joins. It still requires four typed I/O cancellations
and one child that never started. Closing the writers earlier would allow EOF
to mask failed cancellation. The pool startup repair in #573 is a separate
path: this nursery fixture has no pool or worker startup barrier.

## Observations

The test writes backend startup and nursery tick, cancellation, join, driver
destruction, and writer destruction phases directly to stderr. Libtest's
ordinary captured output may not be emitted when Bazel terminates a hanging
test. Each line identifies the test thread and backend.

A test-only scheduler driver delegates to the same real `AnyDriver<Child>`.
It records submission IDs, poll entry/exit and requested wait mode, pending
counts, completion IDs/results, raw OS errors, and aggregate poll/harvest
counts. It preserves the existing scheduler adapter's conversion of every
successful or failed-poll completion, including the retained request on
submission rejection. It neither manufactures completions nor retries an
operation. Its destruction markers surround destruction of the actual driver.

Poll boundary output covers the first 32 calls, then power-of-two samples, so
an empty-batch busy loop cannot produce unbounded boundary logs. Every error
and request completion is logged. Cancellation and destruction report aggregate
counts. A sampled entry without its exit identifies a blocked call; a timeout
between samples does not identify that call's exact number. These observations
are driver API results, not raw io_uring CQEs or evidence that cancellation
acknowledgements succeeded.

No production driver, scheduler, selection, timeout, assertion, pipe ownership,
test count, or concurrency configuration changes. All eight existing driver
tests remain selected, and every host driver that starts remains exercised.

## Baseline evidence and limits

Before this change, genuine pinned Bazel 9.3.1 ran
`//crates/vesper-pulse:test/drivers` uncached: all eight tests passed on epoll.
io_uring setup was denied with EPERM. A further 200 full eight-test executions
and 200 exact nursery executions of that Bazel-produced binary passed all
1,800 cases in 1.232 seconds; no child reached its five-second diagnostic
capture bound. These direct binary executions are supplementary to the actual
Bazel target invocation.

The baseline was built at main `9f26c917e967853fbc6ccf581743a568309da9ab`.
The drivers test, scheduler, io_uring, readiness, and epoll source bytes exactly
match the failed #407 head. The baseline receipt is
`/tmp/pulse-nursery-cancellation-diagnosis-v1/receipt.json`, SHA-256
`48d19c20db6133b1b75a4ee299e0a26473b39872e9c7b973af3f6b0f9cd0326a`.
The actual binary's SHA-256 is
`7777fc360a9303ca2abfc6da4fc97cde4ccfddb93d2330cc2171bca5e73459c9`.

The ignored negative cancellation-acknowledgement result in the io_uring ledger
is a conditional source concern, not a captured hosted result. Epoll's
cancellation path synchronously moves pending requests into its completion
queue. Neither observation diagnoses the hosted timeout. A fresh hosted run
with these diagnostics is required, especially on a host permitting io_uring.

## Patched validation

The patch is based on the exact failed #407 head. Genuine pinned Bazel 9.3.1
runs the complete drivers target uncached with `--nocapture --test-threads=3`
and repository-default timeouts: all eight Linux tests pass in 18.564 seconds.
The separate strict package Clippy and rustfmt targets both pass in 18.356
seconds, without forwarding test arguments to those targets.

One supplementary execution of the genuine Bazel-produced binary runs the
exact nursery case with ordinary libtest capture enabled. Direct stderr still
contains backend startup, four cancellation completion IDs, nine poll calls,
driver destruction, join returning five results, and writer destruction. This
confirms the diagnostics bypass libtest capture; it is not a hosted timeout
reproduction. Both patched runs exercise epoll; io_uring is explicitly
unavailable with EPERM.

All 30 original assertion calls and the complete platform-dependent test
catalog are unchanged. The initial compile attempt found ambiguity between
the two driver traits; explicit `CompletionDriver` method qualification fixed
that before the successful checks. The final Rust source SHA-256 is
`13d268c5ba9b3b6689e5634cdcef5a3abd5453276e91e9b3a84889a1cab3f5db`;
the genuine binary SHA-256 is
`59c6cfaefde0e7b1976c7fd3bfff71af23fd989a2437bc8340a326c401e04ad2`.
Commands, logs, test XML, source preservation, and the capture probe are bound
in `/tmp/nursery-cancellation-diagnostics577-validation-v1/receipt.json`, SHA-256
`7ebf7bafe60a4ad3795f9d0094adacbfd07cb8dbe6f7541b5018b26c9511e82a`.

The hosted cause remains unresolved. Run this diagnostic commit on a Linux
host where the io_uring startup marker says `startup complete`; an
`unavailable` marker establishes no io_uring cancellation coverage. Keep the
full output, including the last poll or destructor boundary if it hangs.
