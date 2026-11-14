# Existing proof-queue CI reservations

Public issue #537 applies the established configuration to PR #526 at original head fef2b4f83595b121555cb8348e3629c3b31a3dad. The five configuration/documentation files already match merged #495. Only the 8 existing fixed proof-packet wrappers receive resources:memory:5120 and resources:cpu:1.

The scheduler reservation accounts for the unchanged Lean 4096 MiB heap allowance plus 1024 MiB estimated wrapper/runtime overhead. It is not an RSS cap or a measured memory guarantee. Test all targets requests 19GB; Native acceptance remains at 16GB. BuildBuddy-scoped chunking and diagnostics, and the generated-log ignore/source exclusions, match #495.

Pinned parser: https://github.com/bazelbuild/bazel/blob/fee2bf2d1c14bc2096068a4d46f1842472092044/src/main/java/com/google/devtools/build/lib/actions/ExecutionRequirements.java

Actual Bazel 9.3.1 XML queries before and after verify exactly 8 wrapper tag pairs and identical remaining wrapper attributes. The existing BUILD bytes are recovered exactly by removing the inserted reservation block. Every other tracked source is unchanged apart from the explicit configuration import above. BUILD synchronization and whitespace checks pass. Queries retain the trusted system rc, use batch/no-home-rc with a 2 GiB JVM and one loading thread, and share existing repository caches; no build, test, Lean or Cargo compilation was run.

Full local before/after XML, command/exit/timing/hash and static receipts: /tmp/proof-ci-six537-validation-v1/. These local query results do not establish hosted success. The unavailable BuildBuddy failure logs leave the earlier CI failure causes unknown. No proof, validator, regular-copy, platform, timeout or assertion policy changes are made.
