# CI platform lanes

Linux runs the workspace, lint, proof, and general acceptance suites in the
BuildBuddy `Test all targets` and `Native and acceptance scripts` workflows.
The GitHub `Platforms` workflow checks the exact planned SHA on macOS AArch64
and Windows x86_64 MSVC. Both required check names remain unchanged.

The native lanes run only host-specific coverage:

- macOS runs direct-fiber ABI tests, Pulse's host-driver and pool tests against
  kqueue, the macOS temporary-directory symlink regression, and Mach-O
  executables built and run on Apple silicon.
- Windows runs its direct-fiber and IOCP suites, Pulse's host-driver and pool
  tests against IOCP, and COFF executables linked with MSVC and run on Windows.

The native executable cases are shared with `scripts/test-native-host.sh`;
`scripts/ci/test-native-executables.sh` runs them with the Cargo-built compiler.
Linux retains general source-macro, formatting, proof, and workspace tests.
The native lanes no longer compile or run the entire workspace, Lean proofs,
Clippy, generic acceptance scripts, or doctests. Tests for other host-sensitive
features must be added explicitly to these lanes when needed; platform-neutral
coverage on Linux does not prove those features work on other systems.

PR, scheduled, manual, and release-candidate platform runs use the same native
OS selection. The release workflow still requires the two Linux statuses and
successful native checks for the same immutable SHA; `full` identifies the
release workflow profile, not a full workspace rerun on each native OS. The
workflow publishes nothing. A planning failure, failed test, or wrong checkout
fails the corresponding required check.
