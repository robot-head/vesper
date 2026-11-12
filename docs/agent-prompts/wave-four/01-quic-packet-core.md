# QUIC packet core and RFC 9002 loss recovery

Suggested branch: `wave4/warp-quic-packet-core`.

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

Assignment: QUIC packet core and RFC 9002 loss recovery (§35.2, §35.3, §35.5 packet-number decoder).

Exclusive ownership:
- NEW crates/vesper-warp/** (the crate name the design uses; later integration will add the end-to-end endpoint here, so keep the public API modular: `packet_number`, `varint`, `header`, `frame`, `ack`, `recovery`).
- NEW lean/WarpPacketNumber.lean and optional NEW lean/WarpPacketNumber/**.
- docs/parallel-results/warp-quic-packet-core.md.
- Root registration patch for vesper-warp only (see root patch rule).
Allowed internal dependency: vesper-ffi (read-only). No dependency on any other wave-four crate.

Baseline: nothing of §35 exists on main. vesper-ffi provides validated wire layouts (`Registry`, `split_wire`, explicit-endian integers, fixed byte arrays) but there is no QUIC code anywhere.

Implement (sans-IO, no sockets, no crypto — packet protection is lane 5):
1. RFC 9000 Appendix A.2 packet-number encoding (choose the shortest length given largest acked) and A.3 decoding, exactly as §35.5 gives it, with 1–4 byte truncated lengths, 62-bit bound and checked arithmetic. Reject pn_nbits outside {8,16,24,32}.
2. RFC 9000 §16 variable-length integers (encode/decode, non-minimal accepted on decode per RFC, typed errors on truncation).
3. Header parsing/serialization in place over byte slices using vesper-ffi layouts: the 13-byte `QuicShortHdrWire` from §35.2 (8-byte DCID) generalized to configured DCID length, and RFC 9000 §17.2 long headers (Initial/0-RTT/Handshake/Retry, Version Negotiation) with length/token fields. Header bytes are parsed before header-protection removal as a separate, explicit step (the protected-pn boundary must be visible in the API so lane 5 can plug in).
4. Zero-copy frame parse/encode for PADDING, PING, ACK (with ranges and ECN counts), CRYPTO, STREAM (all OFF/LEN/FIN variants), MAX_DATA/MAX_STREAM_DATA, CONNECTION_CLOSE, HANDSHAKE_DONE. Frames borrow payload slices; unknown or disallowed-in-space frames are typed errors.
5. Per-packet-number-space received-packet tracking that produces ACK frames with bounded range count, and duplicate detection.
6. RFC 9002 RTT estimator (latest/min/smoothed/rttvar with ack_delay rules), packet-threshold and time-threshold loss detection, PTO computation with backoff, persistent-congestion detection. Congestion control is NOT here: expose CC-agnostic events (`on_packet_sent`, `on_ack_received` yielding acked/lost packet metadata and RTT samples) per the wave-four contracts so lane 2 (BBRv3) plugs in at integration.
7. Lean: lean/WarpPacketNumber.lean defines the A.3 decoder over Nat with the same branch structure and proves (a) the result is congruent to the truncated value mod 2^nbits, and (b) if the true packet number p satisfies expected − hwin < p ≤ expected + hwin and p < 2^62, decode returns p. Add a Rust differential test against an executable Lean fixture (pattern: crates/vesper-simd/model) gated on VESPER_LEAN.

Acceptance:
- RFC 9000 Appendix A.2/A.3 worked examples (e.g. largest 0xa82f30ea with 16-bit truncated 0x9b32 decodes to 0xa82f9b32) and RFC 9000 §16 varint examples pass.
- Property tests over random (largest, true pn) windows match the Lean claim; boundary values (0, 2^62−1, window edges) are covered.
- Malformed headers/frames at every truncation point return errors without panics (a small exhaustive-truncation test and a fuzz-style random-bytes test).
- RFC 9002 loss detection reproduces the RFC's packet/time-threshold behaviour on scripted traces; PTO backoff and persistent congestion are tested.
- Header/frame parsing on the hot path performs zero heap allocations (counting allocator test).
- No crypto, no sockets, no TCP, no congestion controller, no claim of interoperability with real QUIC peers.

~~~
