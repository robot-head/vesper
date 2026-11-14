# Endpoint transport controls without a runtime Rust compiler (#525)

The hostile transport test now launches the fixed standard-library Python 3
fixture `tests/fixtures/resource_oracle42/fault_peer.py`. It no longer compiles
a Rust fixture at test runtime. `PYTHON` supplies an explicit interpreter path;
otherwise the test uses `python` on Windows and `python3` on Unix. Interpreter,
script and control name are separate `Command` arguments. The Unix BYE/EOF
fixture likewise quotes the interpreter and script as separate positional
arguments before closing stdout and replacing itself with the live sleeper.
No shell command string is assembled from a path.

The sixteen table controls retain their original expected errors: wrong READY,
stdout noise, malformed JSON, unknown diagnostic constructor, reordered
sequence, oversized frame, zero-length frame, truncated header, truncated
payload, stalled result, stderr flood, crash, wrong BYE counters, trailing
bytes, nonzero exit and missing BYE. Blocked stdin and the Unix valid-BYE plus
stdout-EOF followed by a still-live direct child remain the other two controls.
The 300ms request/shutdown bounds, 1024-byte stderr flood bound and five-second
cleanup assertions are unchanged. No test is skipped and no timeout is raised.
All controls use the unchanged production supervision seam, which returns
only a HarnessError and cannot construct successful model SessionEvidence.
The Python peer contains no inference or evaluation implementation and never
emits a successful mathematical result.

Before edits, the existing integration test at unchanged PR #498 head
`43c94af20b93d783e80463b1684ce53631860bb0` was run with `RUSTC` unset and
`PATH=/usr/bin:/bin`. Python3, sh and sleep were verified present, with rustc
absent. The exact hostile test exited 101 in 0.00228s at line 840, reporting
`required rustc for std-only negative transport fixture` and OS NotFound.
The full original command, environment overrides, binary/source hashes and
first error are retained in
`/tmp/resource-model-endpoint42-no-rustc-v1.log`, SHA-256
`45efc3a31f68af65baf372157e47ab6d7aa5299c8c9e04e7096fcd27bd224e1b`.

This proves an undeclared host-tool dependency independently of hosted CI.
Bazel declares its hermetic build compiler but does not expose runtime rustc
for this integration test. Python3 is already a CI prerequisite: BuildBuddy's
first step runs the Python build-file synchronizer, and the macOS/Windows
workflow installs Python with setup-python. All 85 open issue scopes and actual
changed-file lists of all 47 open PRs were independently refreshed before this
fix; only own #498 touched these endpoint test/fixture/report paths. Public
#525 records the narrow ownership under #478/#482. External #522/#523 owns the
separate Windows workspace-format command-line issue and is untouched.

The genuine worker, protocol, supervisor, model semantics, source/object
identity policy, Cargo files, workflows and test budgets are unchanged. Only
the owned integration test, its negative fixtures and this new report change.
The earlier integration report's Rust-fixture execution describes historical
validation; the evidence below records the replacement. The inaccessible
BuildBuddy failure log does not establish that this defect caused PR #498's
hosted failure. No hosted-success or resolved-CI-cause claim follows from this
local fix.

## Actual verification and source association

The fixed sources were uncommitted during validation on base `43c94af`, while
root separately committed documentation-only notes as
`ff66234aad0b5ccd8f42a6b0a87f225c493c50b8`. Neither base commit is claimed to
contain this fix. No test or peer source changed after the validation build.
The executed sources are bound by SHA-256:

| Source | SHA-256 |
| --- | --- |
| `tests/resource_oracle42.rs` | `833b3bda88afa1ce6836f2372c06deaa7f16bd3dff3d5c72a0b65f4df9efb1d8` |
| `fault_peer.py` | `a2cb3e5aae5989692a964b6cbb35bbafc3b29467bbff9b32caf91864b62cd421` |
| `bye_eof_stall.sh` | `36e0af48a1c357d297b96ad2ebc8d559ff482a342739a0dcd0ab1008ed98cd16` |

All Cargo commands used `--locked -p vesper`, shared target
`/workspace/vesper/target/parallel-next`, `CARGO_BUILD_JOBS=1`,
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0` and
`CARGO_INCREMENTAL=0`. The genuine run additionally set
`VESPER_LEAN=/home/agent/.local/share/vesper-lean/bin/lean`, pinned 4.24.0,
commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`. Commands from the endpoint
integration worktree were:

```sh
cargo test --locked -p vesper --bin resource_oracle42 --test resource_oracle42 --no-run
cargo test --locked -p vesper --bin resource_oracle42 --test resource_oracle42 -- --nocapture
cargo clippy --locked -p vesper --bin resource_oracle42 --test resource_oracle42 -- -D warnings
rustfmt --edition 2024 --check tests/resource_oracle42.rs
python3 tools/bazel/sync_build_files.py --check
```

The build passed in 45.57s. The binary's ten codec tests passed in 0.29s and
all eighteen integration tests passed in 91.65s, with zero failures or ignored
tests. The genuine 38-request matrix retained counters
`[38,4,7,2,5,1,19]`, 8549 framed bytes, successful exact retirement and the
unchanged transcript BLAKE3
`906778a597409df8c0eb418f3e92edd1930a6c32caf8efda2affc37a6de61ecd`.
All ten actual project source/object identities match the preceding genuine
run byte-for-byte, and all nine recorded tool/worker commands exited zero.
The executed worker object remains 1744976 bytes with BLAKE3
`134f2b53ea1e9674a4de669b8c7e128ca0d3cbdf56b8adb1c9df958f8ce1d1d9`.
Exact evidence is retained in
`/tmp/resource-model-endpoint42-portability-session-evidence-v1.json`.

The exact hostile test was also run directly against the rebuilt real test
binary `/workspace/vesper/target/parallel-next/debug/deps/resource_oracle42-2b87bf9f5455bb57`:

```sh
env -u RUSTC -u PYTHON PATH=/usr/bin:/bin \
  /workspace/vesper/target/parallel-next/debug/deps/resource_oracle42-2b87bf9f5455bb57 \
  --exact hostile_peers_use_production_process_and_cannot_return_mathematical_evidence --nocapture
```

This passed all eighteen controls in 1.39s with rustc verified absent.
A second restricted-PATH run explicitly set `PYTHON` to a Python executable
path containing spaces; it passed in 1.46s, including the quoted Unix wrapper.
Both full command/environment receipts are retained as
`/tmp/resource-model-endpoint42-portability-hostile-v1.log` and
`/tmp/resource-model-endpoint42-portability-hostile-configured-v1.log`.
The rebuilt Cargo test binary SHA-256 is
`ba2a4a82f1c6bdfbe459e2bf70511c95c19a72e68700d230c8a4133072e50700`.

Root independently compiled the same exact test source with
`CARGO_MANIFEST_DIR="."` and ran the actual production Bazel copy launcher
against leaf-symlink inputs. With `PATH=/usr/bin:/bin`, RUSTC/PYTHON unset,
rustc absent and the Python fixture copied to a regular file, the same
hostile suite passed in 1.319s. Receipt:
`/tmp/resource-model-endpoint42-portability-runfiles-v1/receipt.json`.
That receipt binds the source and Python hashes above, launcher SHA-256
`19a4948374da7db4cf63ec8c1ce803153541823604d15eb31ebfc2937f2b53d3`
and standalone test binary SHA-256
`610ebda6a47cea5cc45eb9fc5d9f43e296c280728045e7b84706b2da17bc0b14`.
This verifies the production launcher's relative-path/copy topology; it is not
a full local Bazel build.

Strict targeted Clippy passed in 10.74s without suppressions; rustfmt, BUILD
synchronization and whitespace checks passed. The retained pre-fix NotFound
failure is the deliberate reproduction; no new build, test or lint failed.
Logs use the prefix `/tmp/resource-model-endpoint42-portability-`, including
`build-v1.log`, `tests-v1.log`, `clippy-v1.log`, `fmt-v1.log` and
`build-sync-v1.log`. The validation receipt is
`/tmp/resource-model-endpoint42-portability-validation-receipt-v1.json`;
a final commit association receipt records the same source hashes from the
owned commit after committing. Lean and shared Cargo leases were explicitly
released after their respective final successful checks. This lane performs
no push, PR creation or merge; the coordinator updates the existing #498.
