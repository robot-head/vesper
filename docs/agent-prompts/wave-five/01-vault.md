# Vesper Vault: constant-time verifier, secrets and nonce-safe AEAD

Suggested branch: `wave5/vault`. Claim issue: #121.

~~~text
You are implementing one substantial milestone of the Vesper design (§23 (Vesper Vault)) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline af03122ea74d403f203170926945f9d35786b289 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/rfcs/wave-five-contracts.md, docs/agent-prompts/wave-five/README.md and the whole of §23 (Vesper Vault) in docs/specification.txt (the snapshot matches the Google Doc revision of 2026-11-12 09:18 UTC) before editing.

Claim: this work is claimed by GitHub issue #121. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #121 from your PR ("Closes #121").

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #121. The user has authorized merging this effort's PRs once all required CI checks (`check`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Vesper Vault (§23): constant-time cryptography, fenced zeroization and nonce-safe AEAD.

Exclusive ownership:
- NEW crates/vesper-vault/**, lean/VaultConstantTime.lean (plus a lean/VaultConstantTime/ directory if needed).
- docs/parallel-results/vault.md.
- Root registration patch for vesper-vault and its dependencies.
No internal dependencies. Read crates/vesper-warp-crypto (on main) for the Warp-only NonceTicket slice; do not edit it. Vault is the general library; integration may later re-base Warp on it.

Implement:
1. §23.5 constant-time SSA taint verifier over real `cranelift_codegen::ir::Function` (0.125). A `CtPolicy` names secret parameters (and secret loads by memory flag or address source). Propagate taint through data flow including block parameters; reject secret-dependent `brif`/`br_table`, secret-indexed loads/stores (address taint), variable-latency `udiv/sdiv/urem/srem` with a secret operand, and secret-dependent calls to unknown functions; allow `select`/bitwise masking and an explicit declassify marker. Return `VerifierFinding`s per the contract. Build test functions with cranelift-frontend, including loops and block params, and test both acceptance and each rejection.
2. Secret scalars `SecU8/SecU32/SecU64` and `SecVec` whose API has no data-dependent branching, comparison returning a secret mask, explicit `declassify`, and no Debug/Display of contents.
3. `SecretSlot[K]`: page-aligned storage with mlock and MADV_DONTDUMP where available (use a mature crate such as `memsec`/`region`/`libc` in a gated module; report RLIMIT_MEMLOCK failure as a typed error and as unavailable in the report), access only through closure projections, consumed only by `shred(sink)` which zeroizes with volatile writes plus a compiler fence; dropping without shred is detected.
4. Linear `AeadSealer[Alg]` minting `NonceTicket[Alg]` for strictly increasing counters per key, consumed exactly once by seal; burn API; exhaustion at the AEAD limit; AES-256-GCM and ChaCha20-Poly1305 via RustCrypto crates already in Cargo.lock. Doom-principle `open_in_place(sink CiphertextFrame) -> Result<VerifiedPlaintext, AuthError>` zeroizes the buffer on tag failure before returning.
5. Hybrid X25519MLKEM768 key establishment (FIPS 203 ML-KEM-768 via a mature crate such as `ml-kem`, X25519 via `x25519-dalek`), combiner per draft-ietf-tls-ecdhe-mlkem, with published test vectors if the crates provide them, round-trip and tamper tests.
6. `ConstTime::scope` that enables PSTATE.DIT on AArch64 when the CPU reports FEAT_DIT and restores it on exit (gated unsafe or `std::arch` asm in an allowed module); on x86_64 report DOITM as unavailable unless you can actually set it from user space. macOS AArch64 CI runs on Apple silicon, so test DIT there if supported.
7. lean/VaultConstantTime.lean: `verify_tag_16_ct_sound` via `bv_decide` (128-bit branchless compare equals equality) and a trace-independence theorem for a modelled branchless kernel with an explicit LeakageTrace semantics.

Acceptance:
- Verifier tests: at least one accepted constant-time kernel and a rejection test per rule, with instruction indices.
- Nonce reuse/regression is impossible through the API (tickets not Clone) and tested at the runtime boundary; tamper tests zeroize plaintext; zero heap allocations on seal/open hot path (counting allocator).
- No constant-time claim about compiled code beyond what the verifier checks; DIT/DOITM/mlock availability reported honestly.
~~~
