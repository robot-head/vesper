# BBRv3 congestion control and EDT pacing

Suggested branch: `wave4/warp-bbrv3`.

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

Assignment: BBRv3 congestion control and EDT pacing (§35.1 item 3, §35.3, §35.4 theorem 2, §35.5 Bbrv3FlowState).

Exclusive ownership:
- NEW crates/vesper-warp-bbr/**.
- NEW lean/WarpBbr.lean and optional NEW lean/WarpBbr/**.
- docs/parallel-results/warp-bbrv3.md.
- Root registration patch for vesper-warp-bbr only.
No internal dependencies (std only plus test dev-dependencies).

Baseline: nothing exists. §35.5 sketches `Bbrv3FlowState::{target_cwnd_bytes, schedule_edt_ns}`; that sketch is a starting point, not the whole algorithm.

Implement (sans-IO, integer arithmetic, virtual nanosecond time supplied by the caller):
1. The BBRv3 state machine following the current IETF CCWG BBR draft (draft-ietf-ccwg-bbr; record the exact revision you follow): Startup, Drain, ProbeBW (Down/Cruise/Refill/Up), ProbeRTT; full-pipe detection; windowed max bandwidth filter; min_rtt with ProbeRTT scheduling; delivery-rate sampling (per-packet delivered/delivered_time/first_sent_time/app_limited state, rate samples on ACK); inflight_hi/inflight_lo and bw_lo with loss and ECN responses; pacing rate with pacing gain, cwnd with cwnd gain; send quantum.
2. Keep the §35.5 API shape: `target_cwnd_bytes(mss)` clamps to [4·MSS, max(inflight_hi, 4·MSS)], `schedule_edt_ns(pkt_bytes, now_ns)` returns max(now, next_edt) and advances next_edt monotonically. All products use u128 or checked math; zero bandwidth/rtt and saturations are defined, documented and tested.
3. Input events are the CC-agnostic ones in docs/rfcs/wave-four-contracts.md (sent, acked with rate sample, lost, ECN-CE, app-limited, RTT sample). Define them in this crate exactly as the contract names them; lane 1 produces equivalent data and the integration adapter maps between them.
4. A deterministic fluid/packet link simulator inside the crate's tests (bottleneck bandwidth, propagation delay, finite buffer, random/burst loss with a seeded PRNG) used to check behaviour, not to claim real-network performance.
5. Lean: lean/WarpBbr.lean proves §35.4 `bbrv3_edt_launch_monotonic` for the exact integer update you implement (including saturation), that target cwnd always lies within its clamp bounds, and an inflight safety bound: if the sender never sends when bytes_in_flight ≥ cwnd, then bytes_in_flight ≤ cwnd + max packet size. Differential test against an executable Lean fixture gated on VESPER_LEAN.

Acceptance:
- Unit tests for every state transition and the loss/ECN responses; the simulator shows Startup exits on full pipe, ProbeRTT is entered after the min_rtt window, steady-state throughput converges near bottleneck bandwidth with bounded queue, and inflight_hi reacts to loss above the draft's threshold.
- EDT schedule never regresses across arbitrary interleavings (property test), and pacing matches pacing_rate within integer rounding.
- `on_ack`/`schedule_edt_ns` perform zero heap allocations (counting allocator test).
- No claim of fairness/interop with Linux BBRv3; no hardware SO_TXTIME use.

~~~
