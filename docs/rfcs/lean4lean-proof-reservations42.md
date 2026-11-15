# Fixed proof-packet local resource reservations (#537)

The nine fixed proof-packet test wrappers now request
`resources:memory:5120` and `resources:cpu:1` through the existing per-label
`test_tags` map. No macro extension is needed. Their previous medium size
reserved only 100 MiB and one CPU despite each running a sequential Lean
`-j1 -M4096` stage. Existing labels, copy mode, macOS compatibility selections,
timeouts, internal budgets and all proof/refusal policies remain unchanged.

## Pinned Bazel behavior

The workspace pins Bazel 9.3.1, release commit
`fee2bf2d1c14bc2096068a4d46f1842472092044`. Exact sources establish:

- [The parser accepts `resources:<name>:<amount>` and legacy `cpu:<int>`](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/actions/ExecutionRequirements.java#L163).
- [Legal execution tags include `resources:` and `cpu:`](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/packages/TargetUtils.java#L53).
  `ram:` is filtered out; `resources:memory_mb:` would name a separate custom
  resource rather than built-in `memory`.
- [Size defaults reserve 20/100/300/800 MiB, each with one CPU](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/analysis/test/TestTargetProperties.java#L46).
- [Spawn resource construction applies explicit tags as overrides](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/actions/ResourceSetOrBuilder.java#L39),
  and [local execution acquires those resources](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/exec/local/LocalSpawnRunner.java#L142).
- [Default local memory is 67% of detected host memory](https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/exec/ExecutionOptions.java#L356).
  The workspace's `rbe_build` runs tests locally; `--jobs=100` is an upper
  bound and does not replace local resource accounting.

5120 MiB is an estimate: the existing 4096 MiB Lean heap allowance plus
1024 MiB for the Rust supervisor, native runtime and wrapper overhead. It is
neither a measured RSS peak nor an enforced process/cgroup memory limit.
If Bazel sees 19 GiB, its default local budget is 13035.52 MiB: two such
reservations fit, three do not. Actual detected capacity and command-line
overrides determine scheduling. Other untagged heavy tests remain outside
this narrow concurrency bound; no whole-VM memory guarantee follows.

## Evidence and cause boundary

The original #449 report of 17.58 GB used against 17.18 GB was user-supplied.
The separate 19 GB VM configuration in #495 has passing Linux checks. Current
#407/#449/#526 Test-all failures lack accessible failure logs; this change
does not diagnose them or claim that reservations fix their cause.

Pinned Bazel query passed for all nine wrappers: each expanded `cargo_test`
has exactly the two requested tags. It used `--batch --nohome_rc`, retained
system trust configuration, and isolated output base
`/tmp/vesper-bazel-proof-reservations42-query-v1`; query options included
`--lockfile_mode=off --loading_phase_threads=2 --output=xml`. The XML and
receipt are retained as `/tmp/lean4lean-proof-reservations42-query.xml` and
`/tmp/lean4lean-proof-reservations42-query-receipt.json`.

Static validation confirmed exactly nine labels and unchanged pre-existing
macro attributes, including every macOS selection and timeout. BUILD
synchronization and whitespace checks passed. Module loading may invoke the
existing metadata-only `cargo metadata --no-deps --offline` extension. No
build, test execution, Lean or Cargo compilation ran for this change; query
does not establish a passing hosted retry or measured memory improvement.
