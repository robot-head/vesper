# Comparison packet resource backport (#537)

At parent `8b99003edd8aa964020471291bf81abb89cc0ce8`, reserve resources
for exactly seven existing fixed proof packets: foundations, construction,
order, map, pre-subsumption normalization, full normalization and comparison.
Each expanded `cargo_test` receives `resources:memory:5120` and
`resources:cpu:1` through the existing `test_tags` interface. No later
Shape, Addability, Completion or Reconstruction label is introduced.

5120 MiB estimates the unchanged Lean `-j1 -M4096` stage plus 1024 MiB of
wrapper/runtime overhead. The medium default previously reserved 100 MiB.
This is scheduler accounting, not a measured RSS peak or a process memory
limit, and other heavyweight tests remain outside this narrow accounting.
The exact pinned Bazel 9.3.1 sources define the
[resource-tag parser](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/actions/ExecutionRequirements.java#L163)
and [size defaults](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/analysis/test/TestTargetProperties.java#L46).

All five #495 configuration/documentation files already exactly match merged
main commit `f9929e561e6ac3e72b21e4ed94f077351ec0bc00`, including the
19 GB Test-all VM and scoped diagnostic logs. They remain unchanged here;
no main merge or macro extension is needed.

Pinned Bazel queries before and after the BUILD edit passed for all seven
wrappers. Each has exactly the requested tags; every other XML attribute
matches its baseline. Removing only the new tag block restores the original
BUILD bytes, including all copy modes, macOS selections and timeouts. All
41 tracked proof/vendor/test files match the original parent bytes. BUILD
synchronization and whitespace checks pass.

Queries used `--batch --nohome_rc`, system trust, a 2 GiB JVM with one active
processor, one loading thread and isolated output base
`/tmp/vesper-bazel-comparison537-query-v1`. Query does not accept the
build-only `--jobs` option. Exact commands, tool/source hashes, XML and logs
are retained under `/tmp/proof-reservations537-backport-validation/` in
`comparison-{before,after}.{json,xml,log}` and `verification.json`.
Module loading may invoke metadata-only `cargo metadata --no-deps --offline`;
no Cargo compilation, Bazel build/test or Lean execution ran for this change.

The inaccessible hosted Test-all failure is not diagnosed by this backport.
Passing Linux checks on later #538 support use of the existing policy but
do not establish a causal fix or a passing retry here. Proof source, matrix
budgets, catalogs, policies and refusal controls remain unchanged.
