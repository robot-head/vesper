# AoSoA connection tiles, SIMD CID lookup and RSS steering

Suggested branch: `wave4/warp-connection-shards`.

~~~text
You are implementing one substantial milestone of design §35 (Vesper Warp: user-space kernel-bypass QUIC/TCP networking) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline a5fa024b8d6f62eaa336337d825a2be2ec8972b4 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/specification.txt, docs/implementation.md, docs/rfcs/wave-four-contracts.md and docs/agent-prompts/wave-four/README.md (which quotes the §35 design text) before editing.

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; explicitly reject unsupported cases.
- Edit only the exclusive paths below. Other lanes run concurrently; read their APIs on main if already merged, but never edit their paths and never depend on an unmerged lane's crate. The wave-four contracts list the only allowed internal dependencies.
- Root patch rule (user-approved 2026-11-12): you MAY commit your own minimal root patch onto YOUR PR branch so CI goes green and the PR merges in one piece: register your new crate in root Cargo.toml `members` AND `default-members`, add any new `[workspace.dependencies]` entries you need, and regenerate Cargo.lock. Nothing else at the root (no src/main.rs/src/lib.rs CLI wiring, no CI workflow edits, no global docs) unless your assignment says so. Keep the patch in its own commit. If another wave-four lane merged first, rebase and regenerate the lockfile rather than hand-merging it.
- Preserve the separate sandbox/forge-runner guest workspaces and current validated dependency versions. Prefer mature, widely used crates for anything cryptographic or kernel-facing; pin exact compatible versions; never vendor or hand-roll cryptographic primitives.
- Use an isolated target directory per checkout/worktree. Do NOT share CARGO_TARGET_DIR between source-divergent worktrees. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics (vesper.arith.checked/1), interface semantics v2 and native/fiber ABI v2 are frozen. src/lean*.rs, crates/vesper-lean-syntax/** and crates/vesper-loom-tuf/** are coordinator-owned. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Rust crates in this wave forbid unsafe code unless the assignment explicitly allows a gated, audited unsafe module. Linear §35 obligations (UmemFrame, InFlightQuicPkt, NonceTicket, leases) are modelled in Rust as non-Clone, non-Copy, #[must_use] owners whose only exits are the documented consuming calls; a dropped owner must be detected (debug panic or counted leak reported by the auditor), never silently recycled. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Failed consuming operations return the owner to the caller. Unsupported, ambiguous, malformed, overflowed and over-budget inputs are typed errors, never success, and never panics on attacker-controlled bytes. All arithmetic on wire-derived values is checked or explicitly wrapping with a documented reason.
- "0 Alloc" claims require a test with a counting global allocator over the steady-state path after warm-up; otherwise do not claim it. Hardware/kernel features (AF_XDP, DPDK, NIC EDT/SO_TXTIME, crypto offload, RSS hardware) must be reported as unavailable unless actually exercised; a software model validates the modelled protocol only.
- Lean: if your assignment owns a Lean file, it must compile with the pinned Lean 4 toolchain (`lean <file>`), contain no `sorry`/`admit`/new axioms, and be checked from your crate's tests when `VESPER_LEAN` is set (follow crates/vesper-simd/model and tests/model.rs). Do not edit the CI workflow; the workspace test job already sets VESPER_LEAN=lean. Executable Lean or a differential test is not a theorem about Rust; state exactly what is proved.
- Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands/results and limitations.
- Commit and push YOUR branch only; do not merge. Open a ready-for-review PR against main. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: AoSoA connection tiles, SIMD CID lookup and RSS shard steering (§35.1 item 4, §35.3 QuicConnTile / lookup_cid_simd).

Exclusive ownership:
- NEW crates/vesper-warp-shard/**.
- docs/parallel-results/warp-connection-shards.md.
- Root registration patch for vesper-warp-shard only.
Allowed internal dependency: vesper-simd (read-only) if its kernels or ISA-profile types fit; vesper-alloc read-only. No dependency on other wave-four crates. Strata's generic `TiledSeq`/`tiled(N)` is not implemented on main; implement a Warp-local fixed tile table here and do not edit crates/vesper-alloc.

Baseline: nothing exists.

Implement:
1. `QuicConnTile`-equivalent storage: tiles of 16 connections with hot fields (largest_rx_pn, next_tx_pn, bbr bandwidth, min_rtt, cwnd, bytes_in_flight, next_edt_ns) stored as per-field 16-lane arrays and cold fields (peer CID, idle deadline, generation) in a separate array; fixed capacity chosen at construction; slot handles with generation checks; insert/remove/compact without heap allocation after construction. Document the actual cache-line layout with size/align assertions.
2. CID lookup: a scalar reference and a SIMD implementation that compares an 8-byte (or configured ≤20-byte) DCID against 16 tile lanes at once, using std::arch with runtime feature detection on x86_64 (SSE2/AVX2) and aarch64 (NEON), falling back to scalar. Plus an open-addressed CID→slot index for O(1) average lookup at 1M connections. SIMD and scalar results must be identical (property tests), and unsafe std::arch use is confined to one audited module with safety comments — this lane may use `unsafe` only there.
3. Toeplitz RSS hash (Microsoft RSS verification-suite vectors for IPv4/IPv6 with and without ports), indirection table, and flow steering of 5-tuples to N shards; a `ShardSet` where each shard owns a disjoint table and nothing is shared across shards (no locks, no Arc<Mutex>). Document that NIC RSS programming is not performed.
4. Batch processing of up to 64 descriptors: given a batch of (shard-local slot, event) pairs, vectorized updates of bytes_in_flight/cwnd/largest_rx_pn and an idle-timeout scan that returns expired slots, with scalar equivalence tests.

Acceptance:
- RSS vectors pass; steering is deterministic and balanced on random tuples within a documented tolerance.
- SIMD≡scalar for lookup, batch update and idle scan across random tables including empty slots, duplicates rejected at insert, and generation reuse.
- 1M-connection table build and lookups work within a documented memory bound; steady-state lookup/update/scan perform zero heap allocations (counting allocator test).
- Report actual ISA paths executed on the test host; no line-rate or hardware RSS claim.

~~~
