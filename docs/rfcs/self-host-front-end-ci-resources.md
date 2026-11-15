# Scheduler resources for compiler and endpoint tests

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

Issue #615 corrects the local scheduler accounting for two root test wrappers. The complete self-hosted frontend differential test starts four real workers, while Bazel 9.3.1's default medium test books one CPU and 100 MB. The endpoint test serializes its genuine sessions, compiling the fixed Lean modules with `-j1 -M4096`, but also inherits the medium reservation. The existing `vesper_crate.test_tags` map forwards resource tags to the executable test wrappers.

`test/self_host_front_end` now declares `resources:cpu:4` and `resources:memory:5120`. `test/resource_oracle42` declares `resources:cpu:1` and `resources:memory:5120`. The memory values are scheduler estimates following the existing 4096-plus-1024 overhead policy; they are not measured peaks, hard RSS limits or cgroup enforcement. Four 512 MiB worker stacks reserve virtual address space and do not by themselves establish resident memory use.

The pinned scheduler applies `resources:*` overrides when building a spawn's local resources. An explicit `--local_test_jobs=2` initially supplies a local-test count resource; subsequent overrides add the CPU/memory reservations while preserving that count. Thus the Linux concurrency ceiling and these tags can both apply. macOS uses the ordinary resource scheduler. Bazel's CPU availability calculation includes an empirical 0.6 ratio, so these declarations do not guarantee exclusive execution or physical CPU isolation.

The observed macOS frontend target exceeded its existing 900-second outer timeout on #526/#538, while other runs completed near that limit. This establishes an accounting mismatch and makes reduced contention a reasonable repair to evaluate; it does not prove contention caused those timeouts. The existing four workers, complete corpus, native comparisons and every test assertion remain unchanged. No runtime, protocol, compile, request, shutdown or test timeout changes are included. The endpoint retains #574's separate `long` classification.

Before editing, the ownership review inspected 109 open issue bodies and all 1,381 filenames across 16 PRs, with stable issue bodies/sets and individually rechecked PR heads/counts/bodies. Root BUILD overlaps only the coordinator's proof PRs; no external claim or filename overlaps this target accounting or new report. Source-bound inventory: `/tmp/self-host-front-end-ci-resources-preflight-v1/receipt.json`, SHA256 `9932dc68a5986bc33e881e0cc25a9e3a9bbcba89f0c511a4e09e6c424abd8e5a`.

Actual pinned Bazel before/after XML queries verify only the selected resource tags change; the existing foundation tags, both `long` timeouts and all other queried wrapper attributes remain exact. Build-file synchronization, whitespace and full tracked mode/blob comparisons verify that only `BUILD.bazel` and this report change. Commands, XML and source identities are retained in `/tmp/self-host-front-end-ci-resources615-validation-v1/receipt.json`. These static checks are not a hosted performance or whole-suite pass; the single-PR CI validation remains required.
