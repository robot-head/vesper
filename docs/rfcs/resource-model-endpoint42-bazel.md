# Reproducible endpoint source defaults (#535)

The production endpoint CLI selects its default source root at runtime:
`BUILD_WORKSPACE_DIRECTORY`, when provided by `bazel run`, otherwise the
current directory. `--repo-root` overrides either default. The help text
documents that behavior. Source-byte attestation and every model/protocol/
supervision limit remain unchanged.

The original `PathBuf::from(env!("CARGO_MANIFEST_DIR"))` retained a build
sandbox path in the production binary. An actual local Bazel 9.3.1 build
rejected it through the existing reproducibility guard:

```text
compiled Rust output .../bin/resource_oracle42 embeds the absolute working directory ...
Do not retain env!("CARGO_MANIFEST_DIR") or env!("OUT_DIR") in compiled code
```

The guard was preserved. The corrected source is committed at
`d6357113e08805064e438498b81a81fa0dcf1122`, SHA256
`e283e03ae80d3e9a060b86e35a18887817e49b674fefcb21e7cee1c726558121`.
This establishes a local Bazel blocker; it does not infer the cause of an
unavailable hosted failure log.

## Actual validation

The repository-pinned Bazel 9.3.1 release was checked against its published
SHA256 `7f775b54bbec487efaf0f49ccda1d2ed02cd34f582edfdd6aeeec76e39ef585c`.
Its hermetic Rust toolchain is the repository-pinned 1.100.0. The local build
uses the environment's supplied system trust-store configuration, with
certificate validation enabled, two build jobs and no remote cache.

- `build --config=ci //:test/resource_oracle42` passes. The corrected build
  took 52.509 seconds, with 3,347 action-cache hits and eleven total actions.
- `test --config=ci //:test/resource_oracle42` passes through the actual
  regular-copy Bazel launcher: all eighteen integration tests pass with zero
  failures or ignored cases in 83.03 seconds. This includes the genuine
  thirty-eight-request model matrix, source/object/refusal controls and
  negative-only transport peer controls. Bazel's total time was 109.160
  seconds. The default successful Rust test harness captures its debug
  evidence; no uncaptured full-matrix transcript is claimed from this run.
- `test --config=ci //:clippy` passes against all root Rust targets in 49.798
  seconds. No lint policy or process-wrapper check was weakened.
- Six actual Bazel-built CLI cases pass: help; a genuine mint/consume request
  from a different working directory using the runtime workspace default;
  a semantic refusal with an explicit root overriding a bad runtime default;
  a unit request using the current directory; missing default sources; and
  a bad explicit override. The final two fail with no stdout. The three
  genuine sessions require exact retirement and have identical original
  project artifact identities. No refusal receives an execution object.

The tested Bazel CLI is 6,701,456 bytes, SHA256
`e3bd18a5c194c560d95896459a8392922beda6cbb095f7dc24ac61e6fa10d471`.
Its bytes were checked before and after the CLI probes and remained unchanged
across build, test and Clippy. Runtime-workspace, explicit-override and
current-directory sessions took 21.745, 21.624 and 21.616 seconds respectively.

Local evidence is retained in `/tmp/vesper-bazel-endpoint-cli-v1/receipt.json`,
`/tmp/vesper-bazel-endpoint-test-v1.test.log` and `.test.xml`, and the
`/tmp/vesper-bazel-endpoint-{build-v3,build-v4,test-v1,clippy-v1}.log` files.
The v3 failure is retained alongside the corrected pass. Earlier local
bootstrap/certificate and stale-server attempts are also preserved; batch
mode uses the provided system configuration. No network-policy bypass or
certificate-validation relaxation was used.

These are local Linux checks. Fresh hosted checks remain the merge gate.
The endpoint remains an executable finite-model observation, not source/
native equivalence, a whole-checker proof or a native campaign result.
