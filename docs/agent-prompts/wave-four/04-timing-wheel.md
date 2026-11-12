# Hierarchical timing wheel for retransmission leases and EDT launch

Suggested branch: `wave4/warp-timing-wheel`.

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

Assignment: Hierarchical hashed timing wheel for retransmission leases and EDT launch scheduling (§35.1 item 3, §35.2 park_for_ack_or_pto).

Exclusive ownership:
- NEW crates/vesper-warp-wheel/**.
- docs/parallel-results/warp-timing-wheel.md.
- Root registration patch for vesper-warp-wheel only.
No internal dependencies (std only plus test dev-dependencies). Lean is optional for this lane; if you add a proof file, own it as NEW lean/WarpWheel.lean.

Baseline: nothing exists. vesper-simpulse has a deterministic timer heap for simulation; read it, do not edit or depend on it.

Implement:
1. A core-local hierarchical hashed timing wheel (Varghese–Lauck) with a fixed-capacity, preallocated entry slab: O(1) insert and O(1) cancel by handle, amortized O(1) advance, configurable tick (ns) and level count, overflow level for far deadlines, and exact firing order guarantees you document (deadline order within a tick, FIFO for equal deadlines).
2. Generic linear payloads: `Wheel<T>` stores owners of T (e.g. an in-flight packet lease). Insert returns a non-Clone, #[must_use] `TimerHandle` (generation-checked). Cancel returns the payload; firing yields payloads to the caller; stale/foreign handles are typed errors. Capacity exhaustion returns the payload to the caller. Dropping a non-empty wheel is reported (no silent leak).
3. Two uses as thin typed layers: (a) a retransmission/PTO lease table keyed by (space, packet number) supporting "ACK removes lease", "PTO fires lease for retransmit"; (b) an EDT launch queue that releases packets whose departure time ≤ now in nondecreasing EDT order, with a configurable horizon beyond which submission is rejected (modelling SO_TXTIME's horizon).
4. A deterministic virtual clock API; no real time, sleeps or threads in tests.

Acceptance:
- Model-based property test against a simple BTreeMap reference over random insert/cancel/advance sequences, including far deadlines, wrap-around of the tick counter and cascading across levels.
- Insert/cancel/advance perform zero heap allocations after construction (counting allocator test); capacity is fixed.
- Stale handles after cancel/fire and handles from another wheel are rejected.
- EDT queue never releases out of order or early; horizon rejection returns the payload.
- No claims about kernel timer, NIC launch-time offload or interrupt-free pacing; this is the software scheduling model.

~~~
