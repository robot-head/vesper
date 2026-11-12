# Next parallel implementation wave

Status: **completed and integrated**. The user confirmed all seven handoffs are complete. Their code is merged and registered; see `docs/parallel-results/wave-two-integration.md`. These prompts are historical. The next assignments are in `../wave-three/README.md`.

Each lane starts from a fresh `origin/main` in a separate worktree or cloud checkout. Minimum implementation baseline: `0e6caf9`. Each file contains the complete, copyable prompt for its assigned agent. These are assignments for new work, not claims that the remaining features are already implemented.

| Lane | Copyable prompt | Exclusive implementation boundary |
| --- | --- | --- |
| 1 | [Checked arithmetic](01-checked-arithmetic.md) | Numeric semantics, checker/IR/evaluator/optimizers/native/Lean, existing root tests/examples |
| 2 | [Native lifecycle](02-native-lifecycle.md) | Fiber ABI/runtime, native experiment; compatible scheduler changes only if necessary |
| 3 | [Cached sandbox builds](03-loom-cached-builds.md) | New `vesper-loom-build`; existing sandbox and CAS APIs remain read-only |
| 4 | [Pact compiler bridge](04-pact-compiler-bridge.md) | New `vesper-pact-bridge`; checked source types, coherence-gated evidence consumption |
| 5 | [Strata collections](05-strata-collections.md) | Existing `vesper-alloc`; bounded vector/buffer/string storage |
| 6 | [Source simulation](06-source-simulation.md) | New `vesper-sim-session`; reference source execution through SimPulse record/replay |
| 7 | [Forge hygiene and authority](07-forge-hygiene-authority.md) | New `vesper-forge`; host-owned provenance, lexical resolution and authority gate |

The arithmetic lane owns the cross-backend migration as one change. Its broad compiler ownership is deliberate: splitting checked arithmetic across independent backend branches would leave incompatible failure semantics between merges. Other lanes consume existing public interfaces and put tests/examples inside their own crates. No other lane edits the shared AST, parser, checker, IR or compiler backends.

The coordinator owns root workspace/dependency registration, the lockfile, root CLI/module wiring, CI and global documentation. Existing members must remain registered. Only new crates use temporary standalone workspaces, which integration removes. Keep the sandbox's fixture guest as a separate wasm workspace. Preserve `default-run = "vesper"` when registering executable examples or new binaries.

## Integration order

1. Review each branch against its actual recorded base. Run its focused acceptance checks and check public API compatibility; do not treat a session's completion message as acceptance.
2. Integrate checked arithmetic as a complete semantic migration. Update semantic identities used by the new bridge/simulation/build consumers and adjust any source fixtures that depended on implicit wrapping. Keep typed `Fail` identities and the approved field-mode restriction intact.
3. Integrate runtime/collection changes and each new library with root members/default-members, the shared lockfile and CI. The Loom runner accepts build scripts only; connect Forge macro execution/cache publication only after the authority/provenance boundary has been reviewed.
4. Run formatting, strict workspace Clippy, real Lean extraction, the complete workspace suite, host executables and AArch64 checks. Platform cross-compilation is useful evidence; it does not establish platform execution or kernel behavior.
5. Commit/push the integrated checkpoint on `main`. Preserve successful owners and report kernel/hardware limits.

Independent libraries can be reviewed before arithmetic finishes. Compiler-consuming branches must be checked again against the merged arithmetic semantics before integration. New APIs should remain compatible with the baseline; a required shared-API change goes to the coordinator instead of creating another writer.

## Compiler work after this batch

Two important source-facing lanes follow these prerequisites and should not run concurrently with lane 1:

- **Source-to-native suspension:** after checked arithmetic and native lifecycle merge, own `src/native.rs` and the necessary explicit IR/evidence metadata. Acceptance is an actual checked source linear effect parking/resuming through the pinned-stack ABI with evaluator agreement, without interpreter substitution. Hand-written CLIF is the current prototype milestone.
- **Source traits/models:** after arithmetic and the Pact bridge merge, own AST/syntax/checker changes for traits, associated types, canonical impls, named models, lexical model selection and evidence-backed call elaboration. The bridge in this batch does not add source trait syntax or dictionary codegen.

Region lifetime/rewind typing also remains compiler work after the collections' `RegionAccess` contract is reviewed. Prism/Aura/full Ignis production features still depend on those runtime, allocation, trait and FFI boundaries. Proof-artifact verification and registry/trust work remain separate future lanes.

See the [previous integration report](../../parallel-results/handoff-integration.md) and [contract audit](../../rfcs/parallel-contracts.md) for the current limits.
