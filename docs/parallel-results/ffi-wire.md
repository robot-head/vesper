# Bridge wire-layout milestone

Branch: `parallel/ffi-wire`, based on `c5328d0`.

Implemented `crates/vesper-ffi` as a standalone, dependency-free Rust library. Public APIs cover declaration/layout validation, nested field metadata, safe immutable/mutable zero-copy wire views, header/payload splitting, endian-aware integer access, range checking, zero initialization, canonical padding validation and padding-insensitive semantic comparison.

The crate interprets explicit-endian scalars as byte-aligned wrappers, while native-endian scalars retain natural alignment and require explicit internal/tail `ZeroPad`. This resolves the specification's packed 61-byte Kafka example without unaligned typed references. The policy is documented in the crate README for frontend/codegen owners to adopt or revise explicitly.

Validation covers exact Kafka offsets, little/big/native endian signed/unsigned boundaries, short and misaligned buffers, disjoint mutation of payload/header, nested projection, padding construction/comparison, invalid fields/names/cycles/widths, arithmetic overflow, deep/expansive descriptor graphs and non-mutating errors. Formatting, Clippy with warnings denied, and all crate tests pass.

Integration: remove the temporary child `[workspace]` stanza and add the crate to root workspace members. No root manifests, lockfiles, existing source modules or other worktrees were changed. Parser/HIR/PCG/CLIF wiring, cross-target native-field layout, `.vsys`, foreign handles and hardware isolation remain separate milestones. No heap-allocation, isolation-latency or compiler-proof claims are made beyond the implemented byte-view API.
