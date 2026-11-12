# Completed handoff integration

All seven feature handoffs and the additional platform-CI branch are integrated into `main`. Each library is registered in root `members` and `default-members`; the native experiment is also a workspace member. Temporary nested lockfiles/workspaces were removed. The root `default-run = "vesper"` preserves plain `cargo run` now that the workspace has a second executable. The sandbox fixture guest remains a separate wasm workspace with committed binaries and its own lockfile.

## Review fixes

- Lean handler frames include implicit mutable method arguments and every argument of same-named effect operations with different arities. Regression programs execute in Lean and agree with the evaluator.
- Pact validates public certificate terms and declaration IDs before structural checking, handles sparse inference IDs, and rejects children on a coinductive hypothesis. Consumers must still run coherence checks before trusting evidence.
- Native fiber IDs and generations cannot wrap into reused tokens; stack guard sizing checks overflow; oversized custom-driver completions become `EIO`.
- WASI directory streams continue returning EOF until dropped and release cached entries at resource destruction. Link counts and directory sizes are normalized along with timestamps.
- CAS materialization accepts a legal maximum-length destination filename. Windows uses an atomic no-replace rename that preserves read-only objects; discarded private staging files have their read-only attribute cleared before deletion.
- Rust 1.99 Clippy findings are resolved. CI includes `EffectPlaces.lean` on all lanes, avoids duplicate standalone builds, limits debug-info/build resource usage, and reports unsupported MSVC executable linking with an ignored test and a skipped host-linking step.

## Validation

On the Linux integration host:

- `VESPER_LEAN=/home/agent/.elan/bin/lean cargo test --workspace --locked`: 657 tests including doctests passed; no failures.
- After the final CAS/platform and Clippy changes, the 83 affected CAS/Ignis tests passed again. Workspace formatting and strict all-target Clippy passed.
- `Foundations.lean`, `RuntimeFrames.lean`, `SpecificationCounterexample.lean`, and `EffectPlaces.lean` compiled with Lean 4.24.0.
- All eight executable examples returned the expected results on x86_64 and AArch64 under QEMU.
- Four native prototype fibers completed through the Pulse scheduler; x86_64 and AArch64 objects were emitted. The experiment explicitly used fixture I/O because `io_uring_setup` returned `EPERM`.

Windows CAS conditional code and tests also passed `cargo check -p vesper-loom-cas --tests --target x86_64-pc-windows-gnu --features blake3/pure` (cross-compilation only). The macOS/Windows jobs configure further acceptance checks; they have not been executed on this host. No kernel-I/O, D3D12 hardware, compiler-preservation, termination, or physical machine-stack relocation claim follows from these results.

## Next integration work

These delivered libraries do not complete the full expanded specification. The remaining boundaries are described in [implementation coverage](../implementation.md) and the [contract audit](../rfcs/parallel-contracts.md). The next useful independent work packages are:

1. Migrate checked integer arithmetic atomically across checker, evaluator, optimizers, native and Lean backends, preserving the approved field-mode premise.
2. Define source-to-native suspension metadata and lower a source linear effect through the pinned-stack ABI; preserve original-I/O-completion ownership during cancellation.
3. Connect Loom build execution to CAS with complete implementation/target/toolchain keys, explicit failed-run cleanup, and authority/provenance checks before memoizing macro output.
4. Introduce source trait/model syntax and a checker adapter to Pact, with mandatory coherence checking and package-qualified identities.
5. Define region alias origins, exclusive mutation and rewind eligibility before connecting Strata to source lifetimes and linear exhaustion.

The existing handoff prompts record delivered scope. New assignments should use these integration boundaries and separate worktrees, with shared compiler representations and workspace files kept under one coordinator.
