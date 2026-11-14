# Calling-thread BBR allocation measurement (#570)

The macOS job 132504771963 failed the existing BBR zero-allocation test with
four counted allocation calls rather than zero. Its log contains no allocating
thread identity or call stack. The source of those four calls remains
unproved; this change does not assert that they came from libtest.

The old fixture enabled one process-wide AtomicBool and counted allocator
calls from every thread. That measures unrelated concurrent harness activity
as well as the synchronous operation under test. The BBR implementation has
no heap-backed controller fields, worker dispatch, allocating callbacks or
dependencies: the inspected hot paths operate on scalars, fixed arrays,
options and caller-provided slices. That source inspection is not a dynamic
macOS allocation trace.

## Fixture change and controls

The global allocator still delegates every operation to System. Its observer
is now const-initialized, non-Drop thread-local Cell state, enabled only on
the calling thread for the measured window. It counts alloc, alloc_zeroed
and realloc explicitly, saturating counts so they cannot wrap to zero.
It performs no formatting, thread-ID lookup, heap-backed initialization or
RefCell borrowing. try_with handles unavailable TLS during teardown, and a
reentry flag protects the observer. A thread-bound RAII guard resets each
window and disables measurement on unwinding; nested windows are rejected.

The synchronous BBR workload therefore remains subject to an exact zero
threshold for all its measured-thread allocation entry points. Production
BBR code is unchanged. If BBR allocates on that thread, the new observer still
records a nonzero count and the existing assertion fails. This test does not
claim that the entire process, its harness or arbitrary asynchronous workers
allocate nothing.

Two positive/isolation controls prevent a disabled observer from passing:

- Real std::alloc alloc, alloc_zeroed and realloc calls return usable memory.
  The test writes and reads it, verifies zero initialization and retained data,
  and escapes pointers through black_box. Each entry point must count exactly
  one call. Repeating the window and then measuring an empty window checks
  reset behavior. Successful allocations are deallocated with their current
  matching layouts after measurement.
- A worker performs those same three real allocations while the calling
  thread's window is active. Release/acquire handshakes guarantee that overlap.
  The worker's counter must report three, and the calling thread must report
  zero. Thread creation, joins and Arc allocation occur outside the calling
  window. A drop signal also releases the caller if the worker unwinds, so a
  failed worker assertion is propagated by join rather than leaving the caller
  waiting for a missing signal.

The exact existing step function is byte-identical. Ring capacity 256,
5,000 warm-up steps, full_bw_reached assertion, 200,000 measured steps,
periodic loss/ECN every 97 packets, and the >15,000,000,000 virtual-nanosecond
ProbeRTT-interval assertion are preserved. The allocation threshold remains
zero. No workload reduction, test skip, time-limit relaxation or production
algorithm/Cargo/BUILD/workflow change is included.

## Ownership and validation

Before edits, the complete current preflight fetched all 89 open issue full
bodies and all 26 open PR metadata/complete paginated filename lists (461
path occurrences); every PR's changed_files count matched. Ascending and
descending open issue/PR inventories agreed. No active external BBR test or
production owner was found. The external platforms.yml owner #347/PR #360
and Flux's separate workflow PR #536 remain untouched. Public #570 claimed
the exact fixture and this new report before implementation.

Snapshot `/tmp/warp-bbr-zero-alloc-ownership-20261114-current-v1.json`, SHA256
`4c1f7a21f53af3b2e1eede14ec46a3e25bcc3be376ebeaee0bf0185ed7bb4258`.
Direct gh was unavailable for this inventory; the GitHub connector supplied
the complete successful fetches, including each all-pages PR file list.

The isolated branch starts at existing own Pulse PR #563 head
`9b590ac9e60176fd469656fb2eed8bc2aa5fb3b2`. It changes only the fixture and
this report; root will combine it into that existing PR. No push, merge or
new PR action was performed by this author.

Actual Rust 1.100.0 validation on Linux used the granted exclusive Cargo
lease, one build job and the reused small target:

```sh
export CARGO_TARGET_DIR=/tmp/pulse-windows562-cargo-target
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo +1.100.0 test --locked -p vesper-warp-bbr --test zero_alloc -- --test-threads=3
cargo +1.100.0 clippy --locked -p vesper-warp-bbr --test zero_alloc -- -D warnings
rustfmt +1.100.0 --edition 2024 --check crates/vesper-warp-bbr/tests/zero_alloc.rs
```

All commands passed. The actual test binary ran all three tests concurrently:
three passed, zero failed or ignored, in 0.06 seconds. Strict targeted Clippy,
formatting and git diff --check passed. There were no failed compilation or
test attempts in this implementation. The exclusive Cargo lease was explicitly
released after validation and artifact preservation. No Lean or Bazel ran.

Fixture source SHA256:
`3c49854d5a7c725e2702e6469d9cc8bf566fe1c4ba8d0f480bddb013c7f2dae7`.
Actual test binary SHA256:
`f2c477b3017220bd00884eb93cbb88a3dd9eeb4341e1edd779aea6833f7bb46b`.
The binary, exact test/production source snapshots and logs are preserved
read-only at `/tmp/warp-bbr570-zero-alloc-evidence-v1` before target reuse.
Receipt `/tmp/warp-bbr570-zero-alloc-receipt-v1.json`, SHA256
`068d7e50f774851a24622dfa65560894ac177497e87dc2aa7365890e19a9dc3a`,
binds the exact commands, source/binary/log identities and preservation checks.

This is genuine Linux test/lint evidence. The modified fixture has not been
run on macOS or Windows here. Hosted native CI remains the validation gate;
no claim that the observed macOS failure is fixed is made before that retry.
