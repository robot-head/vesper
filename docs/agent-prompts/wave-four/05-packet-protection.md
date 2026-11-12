# QUIC packet protection with linear nonce tickets

Suggested branch: `wave4/warp-packet-protection`.

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

Assignment: QUIC packet protection with linear nonce tickets (§35.1 item 2; the Vault §23.3 NonceTicket slice Warp needs).

Exclusive ownership:
- NEW crates/vesper-warp-crypto/**.
- docs/parallel-results/warp-packet-protection.md.
- Root registration patch for vesper-warp-crypto plus its RustCrypto (or equivalent mature) dependencies.
No internal wave-four dependencies. Do not create a general Vesper Vault crate; this crate is the Warp-specific slice and must not claim to implement §23.

Baseline: nothing exists. No cryptography is implemented anywhere in the workspace except BLAKE3/SHA-256/Ed25519 used by Loom.

Implement (using mature audited crates such as `aes-gcm`, `chacha20poly1305`, `aes`, `chacha20`, `hkdf`, `sha2`, `subtle`, `zeroize`; no hand-rolled primitives):
1. RFC 9001 §5.2 Initial secret derivation (HKDF-Extract with the v1 salt, HKDF-Expand-Label "client in"/"server in", "quic key"/"quic iv"/"quic hp"), and the same key schedule for supplied traffic secrets (TLS itself is out of scope).
2. AEAD seal/open in place over a header||payload buffer with the header as AAD, for AES-128-GCM, AES-256-GCM and ChaCha20-Poly1305; nonce = iv XOR left-padded packet number (RFC 9001 §5.3).
3. Header protection (RFC 9001 §5.4) for AES (ECB mask) and ChaCha20, applied/removed in place, with sample-offset bounds checks. Integrate with lane 1 only through the contract's byte-offset boundary (pn offset and pn length), not by depending on vesper-warp.
4. Linear `NonceTicket`: a per-key, per-direction sealer issues tickets for strictly increasing packet numbers; sealing consumes a ticket; reuse, regression and exhaustion (2^62 or the AEAD confidentiality limit from RFC 9001 §6.6) are typed errors that return ownership. The key-update path (RFC 9001 §6) derives next-phase keys, tracks the key phase bit and retires old keys after a documented window.
5. Secrets and keys are zeroized on drop; tag comparison and any secret-dependent branching use constant-time primitives. Document which properties come from the dependency crates and which you test.
6. Hardware AEAD offload (kTLS/NIC inline crypto) is NOT implemented; expose an extension trait only if it is tested with a software implementation, and report hardware offload as unavailable.

Acceptance:
- RFC 9001 Appendix A vectors: client/server Initial keys, the protected client Initial, server Initial, Retry integrity tag (A.4) and ChaCha20-Poly1305 short-header packet (A.5) reproduce byte-for-byte.
- Tampered header, payload, tag or packet number fails open without modifying the caller's owner state; nonce reuse and regression are rejected; key update round-trips.
- Seal/open on the hot path perform zero heap allocations (counting allocator test).
- No TLS handshake, no constant-time proof claim beyond dependency guarantees, no hardware offload claim.

~~~
