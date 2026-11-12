# RFC 9293 TCP sans-IO endpoint

Suggested branch: `wave4/warp-tcp`.

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

Assignment: RFC 9293 TCP sans-IO endpoint (§35 title and intro: "QUIC/TCP", RFC 9293).

Exclusive ownership:
- NEW crates/vesper-warp-tcp/**.
- docs/parallel-results/warp-tcp.md.
- Root registration patch for vesper-warp-tcp only.
Allowed internal dependency: vesper-ffi (read-only) for header wire layouts. No dependency on other wave-four crates; congestion control is CC-agnostic per the contract (provide a minimal RFC 5681 NewReno controller behind the contract's controller trait for standalone tests; BBRv3 is wired in at integration).

Baseline: nothing exists.

Implement:
1. TCP header (RFC 9293 §3.1) and options (MSS, window scale RFC 7323, SACK-permitted/SACK RFC 2018, timestamps RFC 7323) parse/serialize in place with checksum computation over a supplied IPv4/IPv6 pseudo-header; malformed option lists are typed errors.
2. The full RFC 9293 connection state machine (LISTEN … TIME-WAIT, simultaneous open/close, RST handling and RFC 5961 challenge-ACK mitigations), modular sequence-number arithmetic, send/receive windows with window scaling, zero-window probing, out-of-order reassembly with bounded memory, FIN handling.
3. Retransmission: RFC 6298 RTO estimation with Karn's rule, fast retransmit/recovery, SACK-based loss detection (RFC 6675) with bounded scoreboard, TIME-WAIT duration configurable for tests. Timers are surfaced as deadlines for the caller (sans-IO); time is virtual ns.
4. Zero-copy: segments are produced into caller-provided buffers and received payloads are surfaced as borrowed slices into the caller's buffer; retransmission keeps references to caller-owned send-buffer ranges, not copies, with an explicit release on ACK.
5. A deterministic two-endpoint harness with a lossy/reordering/duplicating link (seeded) for tests.

Acceptance:
- State-machine tests for every RFC 9293 transition including simultaneous open/close and RST in each state; RFC 5961 challenge ACK cases.
- Bulk transfer over the lossy harness delivers an exact byte stream under loss, reordering, duplication and zero windows; SACK recovery retransmits only missing ranges.
- Checksums match known packets (include pcap-derived byte fixtures you construct and document).
- Malformed segments never panic (exhaustive truncation + random bytes); steady-state segment processing performs zero heap allocations (counting allocator test).
- No kernel/TUN/raw-socket I/O, no interop claim with real stacks.

~~~
