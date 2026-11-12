# Source XdpNic effect and adversarial WAN simulation

Suggested branch: `wave4/warp-source-simulation`.

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

Assignment: Source-level XdpNic linear effect and adversarial WAN simulation (§35.1 items 1 and 5, §35.2 `linear effect XdpNic`, §35.3 `poll_and_step_quic_rx_one` control flow).

Exclusive ownership:
- crates/vesper-sim-session/** (existing crate; preserve current scenario/recording versions and APIs, version any schema you change explicitly).
- docs/parallel-results/warp-source-simulation.md.
No dependency on other wave-four crates (the packet model here is the simulator's own bounded model; integration may later back it with lane 3's software NIC). Compiler/checker (src/check.rs etc.), SimPulse kernel/journal, native session and allocation crates are read-only. If the checker rejects something §35.3 requires, record it as a coordinator request in your report; do not edit the checker.

Baseline: crates/vesper-sim-session runs checked source on the reference CEK evaluator with virtual clock, process epochs, WAL and a message network (`net-v1`, see docs/parallel-results/crash-wal-network-simulation.md). Source already supports `linear struct`, `linear effect` and `sink`/`in` passing; there is no `wire struct`, `tiled(N)` or `split_wire_prefix` source syntax.

Implement:
1. A source binding for the §35.2 `linear effect XdpNic` operations (harvest_rx_single, view_frame_bytes, recycle_to_fill_ring, turn_around_tx_in_place, submit_paced_tx, park_for_ack_or_pto) over typestate `linear struct UmemFrame[State]` / `InFlightQuicPkt` source values, checked against the source signatures at scenario load. Header splitting is an effect/helper operation returning checked source values (no new syntax). The simulator tracks every frame location and enforces UMEM conservation after each step; a lost frame is an explicit model error.
2. Source fixtures: a §35.3-shaped `poll_and_step` that recycles on malformed headers and unknown CIDs, turns valid packets around and parks leases; plus rejected fixtures showing the existing checker refuses (a) dropping a harvested frame on a branch, (b) `?` while a linear frame is live ([T-Let]), (c) using a frame after recycle. If any of those is NOT rejected by today's checker, that is a finding: report it precisely as a coordinator request.
3. A datagram link model (`net-v2` or a new `datagram-v1`, leaving `net-v1` behaviour unchanged): Gilbert–Elliott two-state burst loss, independent loss, reordering, duplication, NAT rebinding (source address/port change mid-flow), and a bottleneck queue with finite buffer and service rate that produces micro-burst bufferbloat. All choices come from the seeded scenario, are recorded in the model trace, and replay exactly or reject the first mismatch.
4. Scenario actions to drive bulk ingress, ACK/PTO deadlines and NAT events; budgets enforced before retaining data.

Acceptance:
- The correct fixture runs thousands of packets under burst loss/rebinding with exact conservation and replays byte-identically; a deliberately buggy host-side binding that drops a frame is caught by the conservation check.
- The rejected fixtures fail checking with the expected diagnostics (or are reported as checker gaps).
- Gilbert–Elliott statistics over long seeded runs match the configured stationary loss rate within a documented tolerance; reorder/duplicate/rebind events replay exactly; changed choices are rejected on replay.
- Existing sim-session tests, CLI and identity rejection still pass. No real sockets, clocks, randomness or sleeps. Not Crucible concolic fuzzing (§29), not DPOR, not native execution.

~~~
