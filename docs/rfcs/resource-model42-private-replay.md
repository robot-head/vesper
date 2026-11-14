# Checked kernel replay with full private names

Issue [#380](https://github.com/robot-head/vesper/issues/380) changes the auditor's
declaration replay plumbing. It does not change the CLI/report schemas, trusted
imports, blanket rejection of unsafe/partial package declarations, exact
`ConstantInfo` comparisons, requested-theorem requirements, or axiom policy.

Lean 4.24's high-level `Environment.addDeclCore` first checks a declaration in the
kernel, then registers it in an elaboration index keyed by its normalized user
name. Different full private names can share that user name. Replaying both into
one elaboration environment therefore panics even when both declarations are
valid. Private names must retain their full module-qualified identities; dropping
or renaming one would change the audited artifact.

## Implementation

The fixed block between `BEGIN CHECKED KERNEL REPLAY` and
`END CHECKED KERNEL REPLAY` in `tools/proof-audit/Audit.lean` exposes:

```lean
CheckedKernelReplay.replay
  (newConstants : Std.HashMap Name ConstantInfo)
  (base : Environment) : IO Environment
```

The block adapts Lean's Apache-2.0 `Lean/Replay.lean` with attribution. Its state
holds `Kernel.Environment`, and every declaration is checked through
`Kernel.Environment.addDeclCore 0 declaration none`. It never calls an unchecked
insertion API, sets `debug.skipKernelTC`, normalizes private names, or silently
skips unsafe/partial declarations. The helper repeats the unsafe/partial and
trusted-shadow refusals internally.

Dependency ordering includes ordinary type/value references, projection
structure names, inductive constructor/parent references, and recursor rule
constructors and expressions. Mutual inductive groups are reconstructed through
one checked declaration. Original constructor and recursor metadata is compared
exactly against the kernel-generated metadata afterward. The caller's existing
full `sameInfo` pass remains in place for every original declaration, including
the remaining inductive metadata.

`Environment.ofKernelEnv` reconstructs the inspection wrapper after replay.
Newly checked declarations reside in the kernel map's second stage, whereas
Lean's elaboration `Environment.find?` first checks imported map entries and its
asynchronous index. Consequently the auditor's post-replay equality and theorem
lookups now explicitly use `replayed.toKernelEnv.find?`. Lean's unchanged
`collectAxioms` already reads `env.checked.get`; it therefore observes the actual
checked proof bodies without populating the conflicting elaboration index.

The first compiled source checkpoint was `920b6ac`; the checked-map observation
follow-up is `7cb63f1`. The snapshot tool can copy the marked replay block
verbatim and must likewise inspect checked results through `toKernelEnv`.

## Executed evidence

All commands below used the actual pinned executable
`/home/agent/.local/share/vesper-lean/bin/lean` (Lean 4.24.0). No Cargo build or full
resource-model replay was run by this lane.

1. The actual updated helper compiled successfully with no diagnostics:

   ```sh
   /home/agent/.local/share/vesper-lean/bin/lean \
     -o /tmp/resource-kernel42-Audit.olean tools/proof-audit/Audit.lean
   ```

2. The independently authored tracked fixtures at
   `tools/proof-audit/fixtures/private-replay/` (fixture commits `c3a6c1a` and
   `9a4c27e`, from the separate fixture worktree) were copied into
   `/tmp/resource-kernel42-replay-controls-v1`. Each was compiled with that
   directory as both working directory and `LEAN_PATH`:

   ```sh
   lean -o PrivateReplayLeft.olean PrivateReplayLeft.lean
   lean -o PrivateReplayRight.olean PrivateReplayRight.lean
   lean -o PrivateReplayBoth.olean PrivateReplayBoth.lean
   ```

   All three commands exited zero. The independent tracked metadata probe was
   run with `LEAN_PATH=/tmp/resource-kernel42-replay-controls-v1`:

   ```sh
   lean --run /workspace/vesper-worktrees/resource-kernel42-private-controls/tools/proof-audit/fixtures/private-replay/PrivateReplayMetadata.lean
   ```

   It exited zero and asserted four actual declarations, two different full
   private names with the same user name, and different private proof
   dependencies for the public `left` and `right` theorem roots. The full names
   are `_private.PrivateReplayLeft.0.ProofAuditPrivateReplay.shared` and
   `_private.PrivateReplayRight.0.ProofAuditPrivateReplay.shared`.

3. `request.json` selected module `PrivateReplayBoth` and both public theorem
   roots under that private artifact directory. The updated helper ran as:

   ```sh
   lean --run tools/proof-audit/Audit.lean \
     /tmp/resource-kernel42-replay-controls-v1/request.json
   ```

   It exited zero in 11.46 seconds, reported `kernel_replayed: true`, checked
   four constants, and reported an empty axiom set for each root. Standard error
   was empty. The initial pre-follow-up attempt had returned an explicit
   "kernel replay omitted declaration" error because it used the elaboration
   wrapper lookup; it was not counted as successful evidence.

4. The pre-change helper from `af1bf5e` was saved as `Audit-before.lean` outside
   the repository and run against the **same** request and artifacts:

   ```sh
   lean --run /tmp/resource-kernel42-replay-controls-v1/Audit-before.lean \
     /tmp/resource-kernel42-replay-controls-v1/request.json
   ```

   It exited one with the real `AsyncConsts.add` panic naming the two colliding
   full private declarations. This establishes that the fixture distinguishes
   the old and new replay paths.

The retained evidence directory contains `PrivateReplay*.log`, `metadata.stdout`,
`metadata.stderr`, `audit.stdout`, `audit.stderr`, `before.stdout`, and
`before.stderr`. `git diff --check` passed for the source changes.

## Remaining integration evidence

The coordinator must rebuild the Rust CLI so its embedded helper includes these
bytes, rerun the existing six `proof_audit_cli` tests and the new independent
private-name regression, and complete the projected resource-model audits. This
report does not claim those outcomes. Kernel logical replay does not establish
native helper equivalence, compiler correctness, full-language resource-oracle
completion, or an OS sandbox.
