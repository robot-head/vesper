# Warp QUIC TLS 1.3 handshake

Suggested branch: `wave5/warp-tls`. Claim issue: #132.

~~~text
You are implementing one substantial milestone of the Vesper design (§35 (Vesper Warp) with RFC 9001 §4) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline af03122ea74d403f203170926945f9d35786b289 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/rfcs/wave-five-contracts.md, docs/agent-prompts/wave-five/README.md and the whole of §35 (Vesper Warp) with RFC 9001 §4 in docs/specification.txt (the snapshot matches the Google Doc revision of 2026-11-12 09:18 UTC) before editing.

Claim: this work is claimed by GitHub issue #132. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #132 from your PR ("Closes #132").

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; reject unsupported cases explicitly with typed errors.
- Edit only the exclusive paths below. Other lanes run concurrently; never edit their paths and never depend on an unmerged lane's crate. The wave-five contracts list the only allowed internal dependencies. Do not depend on the root `vesper` package.
- Other agents own §16.4 source macros/Forge (#103-#110) and §39 formatting (#111-#120). Do not edit crates/vesper-forge*, crates/vesper-fmt, crates/vesper-lint, crates/vesper-loom-sandbox or root macro/formatter code.
- Root patch rule (user-approved 2026-11-12): you MAY commit your own minimal root patch onto YOUR PR branch so CI goes green and the PR merges in one piece: register your new crate in root Cargo.toml `members` AND `default-members` (insert before "experiments/native-pulse"), add any new `[workspace.dependencies]` entries you need, and regenerate Cargo.lock. Nothing else at the root (no src/** CLI wiring, no CI workflow edits, no global docs) unless your assignment says so. Keep the patch in its own commit. If another lane merged first, rebase and regenerate the lockfile rather than hand-merging it.
- Prefer mature, widely used crates for cryptography, parsing and kernel/hardware access; pin exact compatible versions; never vendor or hand-roll cryptographic primitives. Reuse versions already in Cargo.lock where possible (Cranelift 0.125, Wasmtime 50, rustls 0.23).
- Use an isolated target directory per checkout/worktree. Do NOT share CARGO_TARGET_DIR between source-divergent worktrees. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics (vesper.arith.checked/1), interface semantics v2 and native/fiber ABI v2 are frozen. src/**, crates/vesper-lean-syntax/** and crates/vesper-loom-tuf/** are integration-owned. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Rust crates in this wave forbid unsafe code except the gated module the contracts allow for your lane. Linear obligations are modelled as non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner must be detected (debug panic or counted leak reported by an auditor), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes. All arithmetic on untrusted values is checked or explicitly wrapping with a documented reason.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised; a software model validates the modelled protocol only. Performance numbers in the design are proposals; report only what you measured, with the setup.
- Apply the corrections in docs/rfcs/expansion-37-review.md that concern your section.
- Lean: if your assignment owns a Lean file, it must compile with the pinned Lean 4 toolchain (`lean <file>`), contain no `sorry`/`admit`/new axioms, and be checked from your crate's tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). Do not edit the CI workflow; the workspace test job already sets VESPER_LEAN=lean. State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #132. The user has authorized merging this effort's PRs once all required CI checks (`check`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: the QUIC TLS 1.3 handshake Warp lacks (RFC 9001 §4, RFC 9000 §7 and §18).

Exclusive ownership:
- NEW crates/vesper-warp-tls/**.
- docs/parallel-results/warp-tls.md.
- Root registration patch for vesper-warp-tls (rustls 0.23 with its QUIC API, already in Cargo.lock; choose a crypto provider that builds on all three CI platforms).
May depend on vesper-warp-crypto (read-only). Do not edit crates/vesper-warp (lane 13 owns it); integration A wires your crate into WarpEndpoint.

Read docs/parallel-results/warp-integration.md, warp-packet-protection.md and the vesper-warp-crypto API first.

Implement:
1. A sans-I/O handshake driver for client and server over `rustls::quic`: input CRYPTO bytes per packet-number space (Initial, Handshake, 1-RTT), output CRYPTO bytes per space, key-change events that produce secrets in the form vesper-warp-crypto consumes, ALPN, and certificate verification with a test CA.
2. CRYPTO-frame offset reassembly per space with a bounded buffer, out-of-order and duplicate data, and typed errors on overflow or space misuse.
3. QUIC transport parameters (RFC 9000 §18): encode, decode and validate (duplicates, out-of-range values, missing required parameters, original_destination_connection_id checks) and carry them through rustls.
4. Handshake confirmation and key discard rules (RFC 9001 §4.9) as a state machine API; key update hand-off to vesper-warp-crypto's existing key-update path.
5. In-memory client/server tests that complete a handshake, derive matching 1-RTT keys in vesper-warp-crypto, and seal/open a packet with them; tests for bad certificates, ALPN mismatch, corrupted transport parameters and reordered CRYPTO data.

Acceptance:
- Full handshake in memory with both sides agreeing, negative tests, no heap allocation claim unless measured.
- No endpoint wiring here, no 0-RTT unless fully tested (report it).
~~~
