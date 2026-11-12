# Linear UMEM frames and AF_XDP four-ring model

Suggested branch: `wave4/warp-umem-rings`.

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

Assignment: Linear UMEM frames and AF_XDP four-ring model (§35.1 item 1, §35.2 XdpNic/UmemFrame, §35.4 theorem 1, §35.5 XdpUmemAccounting).

Exclusive ownership:
- NEW crates/vesper-warp-umem/**.
- NEW lean/WarpUmem.lean and optional NEW lean/WarpUmem/**.
- docs/parallel-results/warp-umem-rings.md.
- Root registration patch for vesper-warp-umem only (plus `libc` as a target-gated dependency if you implement the Linux backend).
Allowed internal dependencies: vesper-ffi and vesper-alloc (read-only; use vesper-alloc's slab/backing if it fits a Domain-1 DMA chunk pool, otherwise document why not).

Baseline: nothing exists. vesper-ffi has wire layouts; vesper-alloc has slab/region host runtime but no DMA slab or tiled collections.

Implement:
1. `XdpDescWire` (exact 16-byte Linux `struct xdp_desc` layout via vesper-ffi) and a UMEM of N fixed-size frames (default 4 KiB, configurable power of two, headroom configurable).
2. Typestate owners `UmemFrame<RxHarvested>` and `UmemFrame<TxStaged>` (non-Clone, #[must_use], drop-detected) and the §35.2 XdpNic operations as a Rust trait: harvest_rx, view bytes (in-borrow), recycle_to_fill_ring (consumes), turn_around_tx_in_place (consumes Rx, yields Tx in the same chunk), submit_paced_tx (consumes Tx, yields an in-flight lease carrying pkt_num and EDT), completion harvesting that returns the chunk to Fill/free. The in-flight lease is a plain owner type here; parking it in a timing wheel is lane 4's job and integration's wiring.
3. Single-producer/single-consumer ring model with Linux AF_XDP semantics (power-of-two size, cached producer/consumer indices, wrapping u32 indices, batch reserve/submit/peek/release) for Fill, Rx, Tx and Completion. A deterministic software NIC (loopback and scripted-ingress modes) that consumes Fill/Tx and produces Rx/Completion, including fault injection (ring full, descriptor out of range, misaligned address, length beyond frame).
4. `XdpUmemAccounting::check_conservation` from §35.5 generalized to every location a frame can be (fill ring, rx ring, app-held linear, tx ring, completion ring, in-flight leases, free list); the auditor runs after every software-NIC step in tests and counts dropped owners as leaks.
5. Optional, gated: a real Linux AF_XDP backend (socket(AF_XDP), XDP_UMEM_REG, ring mmap, bind with XDP_ZEROCOPY then XDP_COPY fallback) behind a cargo feature and cfg(target_os = "linux"), in one audited unsafe module. Probe at runtime; tests skip with an explicit "unavailable: <errno>" message when the host denies it. If you cannot execute it anywhere, still keep it compiling and report it as unexecuted. No DPDK.
6. Lean: lean/WarpUmem.lean models the ring/location multiset as counts and proves §35.4 `umem_step_conserves_total_frames` and its generalization: every modelled transition (harvest, recycle, turn-around, submit, complete, ack/free) preserves the total, and the fill ring is replenished on every malformed-packet path in the modelled step function. Differential test of the Rust accounting against an executable Lean fixture gated on VESPER_LEAN.

Acceptance:
- Long randomized software-NIC runs with malformed packets, ring-full backpressure and completions keep exact conservation; deliberately dropping an owner is detected.
- Descriptor validation rejects out-of-UMEM, overlapping-headroom and over-length descriptors without losing the frame.
- Steady-state harvest→turn-around→submit→complete loop performs zero heap allocations (counting allocator test).
- Real AF_XDP execution evidence or an explicit unavailable report; no claims about NIC zero-copy, hardware isolation or line rate.

~~~
