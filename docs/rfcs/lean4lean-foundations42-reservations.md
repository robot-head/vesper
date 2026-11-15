# Foundation packet resource backport (#537)

At parent `d7663dcfbf71dcef1f5e969b6e49ebd7c5432081`, this branch has
one fixed proof packet, `test/lean4lean_foundations42`. Its existing
`test_tags` interface now requests `resources:memory:5120` and
`resources:cpu:1`. No later packet labels are introduced.

The reservation estimates the unchanged Lean `-j1 -M4096` stage plus
1024 MiB for the Rust wrapper and runtime. It replaces the medium test's
100 MiB default reservation; it is neither measured peak RSS nor a process
memory limit. Other heavyweight tests remain outside this narrow accounting.
The exact pinned Bazel 9.3.1 sources define the
[resource-tag parser](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/actions/ExecutionRequirements.java#L163)
and [size defaults](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/analysis/test/TestTargetProperties.java#L46).

The separate configuration commit imports only already-merged main #495,
`f9929e561e6ac3e72b21e4ed94f077351ec0bc00`: Test-all VM memory becomes
19 GB, with the existing scoped BuildBuddy diagnostic logs/chunking and
generated-log exclusions. All five configuration/documentation files exactly
match that merged commit. No full main merge was performed; #407's separate
main-integration conflict remains outside this backport.

Pinned Bazel queries before and after the BUILD edit passed. The expanded
`cargo_test` has exactly the two requested tags; every other XML attribute
matches the baseline. Removing only the new tag block restores the original
BUILD bytes, preserving copy mode, macOS selections and timeouts. All eleven
tracked foundation proof/vendor/test files match the original parent bytes.
BUILD synchronization and whitespace checks pass.

Queries used `--batch --nohome_rc`, system trust, a 2 GiB JVM with one active
processor, one loading thread and isolated output base
`/tmp/vesper-bazel-foundations537-query-v1`. Query does not accept the
build-only `--jobs` option. Exact commands, tool/source hashes, XML and logs
are retained under `/tmp/proof-reservations537-backport-validation/` in
`foundations-{before,after}.{json,xml,log}` and `verification.json`.
Module loading may invoke metadata-only `cargo metadata --no-deps --offline`;
no Cargo compilation, Bazel build/test or Lean execution ran for this change.

The current hosted Test-all failure has no accessible BuildBuddy failure log.
This backport does not diagnose that failure, claim a passing retry or change
any proof source, matrix budget, catalog, policy or refusal control.
