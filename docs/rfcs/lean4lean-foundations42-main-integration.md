# Foundation packet integration with accepted main (#554)

Claim #554 preceded candidate edits. This isolated candidate merges existing
#407 head76d7323a795044109d295a855eea0f6f61d65881 with already-accepted main
ac9c31f4720be707476285cfa05ba9d0ae39ab73. The tested merge commit is
7a12315b409eed9bc8e683d46cdd779fe593e423, with exactly those two parents.
No external PR branch or unmerged commit was imported. The actual #407 worktree
and remote were left to the coordinator for review and publication.

The read-only preview found one conflict and one hunk in
`crates/vesper-kinetic/src/rigid.rs::RigidWorld::kinetic_energy`. Accepted main's
`rotate_inv(s.principal(), s.w)` preserves its principal-inertia-frame behavior;
the iterator loop already retains the Clippy fix. The complete resolved file is
byte-identical to accepted main, SHA256
`5031ac4ab5d4262d228b7980cbcce0ef5653dbffe6b6c5e0a70f936e7143a433`.
No new physics implementation was introduced.

The original d766 preview imported536paths. After the coordinator's separate
#495 CI backport and #537 reservation, this candidate imports531paths, including
366under src/crates and52root source files. Those accepted checker/mode,
language-extractor, native/runtime and domain changes remain intact. No new
AGENTS.md is present in the accepted main tree.

All22selected foundation, borrowed-helper, acceptance, kernel-probe, audit and
checker-CLI source paths remain byte-for-byte identical to76d. Eleven are new
foundation files absent from main; eleven existing proof/runtime files already
match main. In particular, `src/proof_commands.rs`,
`tools/proof-audit/Audit.lean` and `tools/resource-model42/KernelSnapshot.lean`
are unchanged. The existing foundation regular-file copy, platform scheduling,
timeouts and one-packet5120MiB/CPU1 reservation are preserved. Before this report,
the diff against accepted main contains only23owned foundation/docs/BUILD paths.

## Actual integration validation

Using pinned local Bazel9.3.1, system trust, the shared local disk cache, JVM2GiB,
two build jobs and actual Lean4.24.0:

- Actual build of `//:test/lean4lean_foundations42` passes in193.892seconds.
- A single actual Bazel test with `--nocapture` and one local test job passes in
  185.066seconds. Its Rust test executes for127.92seconds:1passed,0failed,
  0ignored. The result is not cached, and complete stdout/XML are retained.
- All49roots, original/projected artifact fingerprints, catalogs, provenance,
  snapshot contents and audit policies match the historical foundation evidence.
  Only nine scratch-directory paths differ.
- The projection is5,058,968bytes, BLAKE3
  `4d1483789427ed0be66c1245852d00c584ffc7d84a673cc4966142ae00459d7e`.
- The newly built accepted-main CLI is bound by SHA256
  `d818eea9ea6fe6993b09e8ac58f37a72cc3240ea8d40c72ebbaa8106cf7672a6`;
  its five actual audit command lines are retained. `auditor_blake3` in an audit
  report identifies the included Audit.lean source, not this CLI executable.

The retained receipt is `/tmp/vesper-bazel-foundations-main-v1-receipt.json`.
The read-only plan, complete imported-path list,22before/after source hashes,
merge parents, raw build/test logs, stdout/XML, historical comparison and binary
identities are saved with that receipt. The mathematical scope remains
`audited_named_foundation_proofs`; whole-checker verification remains false.

This is a targeted post-import foundation/CLI packaging check. It does not claim
a new full-workspace run, all eleven descendant packet reruns, typed pipeline
semantic validation, hosted/RBE execution or a diagnosis of any CI failure.
Those wider main changes were imported as accepted code, not reimplemented or
silently discarded by this proof packet. Protected integration stays with the
coordinator after review.
