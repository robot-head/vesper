# Deterministic Windows foreign-IOCP fixture (#562)

The inherited Windows test expected a duplicate file-handle value never to
have appeared in Pulse's process-wide binding table. That premise was not
established: numeric HANDLE values can be recycled, and known idle values may
legitimately move between Pulse completion ports. Job 132486940981 observed
Ok(File) at the old unwrap_err assertion. Its logs do not establish the exact
recycled-value history or underlying cause.

The new test reexecutes this actual test binary with an exact single-test
filter and one test thread. A child-only Command::env marker selects the
fixture; no process-global environment mutation or ignored test is used. The
child creates a real overlapped file and associates it with a real native
completion port before any Pulse submission. Its Pulse binding table therefore
has no history. The Pulse submit must return the actual foreign-port InvalidInput
diagnostic, retain the original file and buffer, and leave no pending request.

The private fixture returns after dropping the rejected request, driver, native
port and watchdog token and removing the temporary file. Only afterward does
the child exit with dedicated receipt code 73. The parent rejects normal zero
exit, including a stale exact filter selecting zero tests. It also rejects
spawn/wait failures, native setup failures, panics, and timeout. The parent
starts the existing 60-second watchdog before spawning and polls under a
30-second deadline, then attempts to kill and reap a stalled child. The
independent watchdog remains active during cleanup, so a blocked kill/wait
cannot hang the parent indefinitely. The child also retains its existing
60-second watchdog. Inherited output avoids captured-pipe backpressure.

The existing close/open reuse exercise, observed reuse flag, real cross-port
reads, rebinding capability check and unsupported-platform typed refusal are
unchanged. Only the duplicate-is-untracked assertion and its unjustified
comment are removed from that migration test. The isolated foreign case runs
regardless of rebinding support.

Scope is only windows.rs and this report. Existing windows-sys IO/Foundation
features and standard process/handle APIs suffice; no production IOCP source,
Cargo metadata, workflow, deadline increase or test skip is introduced.

Validation used rustc 1.100.0 (0ef1bc386 2026-11-09) and Cargo 1.100.0
(3acc79c0c 2026-11-05), with genuine workspace dependencies and the installed
x86_64-pc-windows-msvc standard library. These final commands all exited zero:

```text
rustfmt +1.100.0 --edition 2024 --check crates/vesper-pulse/tests/windows.rs
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/pulse-windows562-cargo-target cargo +1.100.0 check --locked -p vesper-pulse --test windows --target x86_64-pc-windows-msvc -j1
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/pulse-windows562-cargo-target cargo +1.100.0 clippy --locked -p vesper-pulse --test windows --target x86_64-pc-windows-msvc -j1 -- -D warnings
```

Final source SHA-256 is
`814d4d1ad69d9899b8b7de2bb4e72b436f8a79aa243f574fa426de0b0eba57be`.
Logs are retained in `/tmp/pulse-windows562-evidence-v2`:

- `rustfmt-check.log`: empty, SHA-256 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.
- `windows-check.log`: SHA-256 `bbb0d6ee5ff7487339fedcc520b1a6667d855c971e800956b066c5f41edb9550`.
- `windows-clippy.log`: SHA-256 `363540152e6f5b0a2362aee174e6538b146542836b6e3396e169573bdaa6b921`.

The earlier successful metadata/Clippy logs and source snapshot remain in
`/tmp/pulse-windows562-evidence-v1`. Source review then added the parent
watchdog and shortened the polling deadline; the final checks above cover
that revision. No compiler, formatting or Clippy check failed. No linking or
native Windows runtime test ran on Linux; native execution remains required
in Windows CI.
