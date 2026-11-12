# Claimed continuation of the expanded foundations

The live [design](../specification.txt) now contains sections 1–41 and Appendix A.
Its [source inventory](../specification-source.json) records the revision and
section/appendix digests. New requirements and scaffold limits are reviewed in
[the continuation review](continuation37-review.md).

GitHub issues are the current ownership board. Before claiming these tasks, the
coordinator checked open issues and PRs; each claim was posted before code began.
Each issue identifies Codex, exclusive paths, dependencies and acceptance checks.
The external Claude Warp plan and implementation (#36–#45, including shard PR
#53) remain read-only. Source/native/Forge/Prism/simulation paths are not assigned
to this batch. Other agents may supply their approved root-registration patches;
the coordinator reconciles those rather than claiming their shared files.

| Issue | Branch | Exclusive implementation |
| --- | --- | --- |
| [#46](https://github.com/robot-head/vesper/issues/46) | `continuation37/serde-binary` | `crates/vesper-serde/**`: actual StreamVByte and a versioned validated tree archive |
| [#47](https://github.com/robot-head/vesper/issues/47) | `continuation37/beacon-arrow` | `crates/vesper-beacon/**`, new `crates/vesper-beacon-arrow/**`: standard Arrow IPC span export |
| [#48](https://github.com/robot-head/vesper/issues/48) | `continuation37/quarry-pages` | `crates/vesper-quarry/**`: real slotted-page bytes and recoverable pool pins |
| [#49](https://github.com/robot-head/vesper/issues/49) | `continuation37/tensor-kv` | `crates/vesper-tensor/**`: actual BF16 KV-page storage and sequence rollback |
| [#50](https://github.com/robot-head/vesper/issues/50) | `continuation37/flux-checkpoints` | `crates/vesper-flux/**`: CDC bytes and complete local checkpoint/resume |
| [#51](https://github.com/robot-head/vesper/issues/51) | `continuation37/aether-virtio` | `crates/vesper-aether/**`: captured packed-queue descriptor bytes and explicit leases |
| [#52](https://github.com/robot-head/vesper/issues/52) | `continuation37/integration` | Source snapshot/checker, new global continuation docs, root package/example/workflow integration |
| [#54](https://github.com/robot-head/vesper/issues/54) | `continuation37/metatheory` | New `lean/MetatheoryAppendix.lean`: pinned-toolchain repairs of the explicit small Appendix A model |

Lane reports are exclusive `docs/parallel-results/continuation37-<lane>.md` files.
All other production paths are read-only. Every implementation uses a separate
worktree/private target, one build job and bounded debug/incremental storage.
Agents commit only their owned files; the coordinator reviews, cherry-picks,
registers the adapter, updates the root lock and runs combined validation.

Successful byte codecs/export must have independently checked output. Invalid
input/capacity failures preserve caller bytes, committed state and recoverable
owners. Queue/page/sequence handles use private origin/generation checks; normal
Rust Drop does not establish source-language total linear exhaustion.

The Arrow adapter keeps allocations out of the dependency-free no_std Beacon
core; its own allocator/RSS and OTAP transport claims remain explicit limits.
Local Flux images bind complete query/policy identity and offsets; they do not
establish distributed exactly-once or durable storage commits. Captured VirtIO
metadata is not a live DMA memory-ordering or hardware isolation guarantee.

The Appendix model must keep its definitions, statements and `k_i ⊑ m_i` field
premise. Repairs cannot add axioms, admissions, trusted executors, or strengthen
hypotheses to hide failures. Its one-binding calculus and resource-count theorem
remain distinct from the production compiler and full surface language.
