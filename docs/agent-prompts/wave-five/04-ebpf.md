# Vesper eBPF: pre-flight verifier and XDP programs

Suggested branch: `wave5/ebpf`. Claim issue: #124.

~~~text
You are implementing one substantial milestone of the Vesper design (§27 (Vesper eBPF)) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline af03122ea74d403f203170926945f9d35786b289 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/rfcs/wave-five-contracts.md, docs/agent-prompts/wave-five/README.md and the whole of §27 (Vesper eBPF) in docs/specification.txt (the snapshot matches the Google Doc revision of 2026-11-12 09:18 UTC) before editing.

Claim: this work is claimed by GitHub issue #124. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #124 from your PR ("Closes #124").

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #124. The user has authorized merging this effort's PRs once all required CI checks (`check`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Vesper eBPF (§27): a sound pre-flight eBPF verifier, ELF emission and the XDP SYN-flood mitigator.

Exclusive ownership:
- NEW crates/vesper-ebpf/**, lean/EbpfFirewall.lean (plus lean/EbpfFirewall/ if needed).
- docs/parallel-results/ebpf.md.
- Root registration patch for vesper-ebpf.
No internal dependencies. Lane 3 builds a Cranelift stack analyzer concurrently; implement your own eBPF-bytecode stack check here using the contract's CallGraph shape (integration B dedupes).

Implement:
1. eBPF instruction decoder/encoder (all ALU/ALU64, JMP/JMP32, LD/LDX/ST/STX incl. atomics, LD_IMM64 with map fd relocation, CALL helper/BPF-to-BPF, EXIT), with an assembler API used by tests.
2. §27.5 abstract interpreter modelling the kernel verifier: per-register type (scalar with signed/unsigned intervals and tnum, PTR_TO_CTX, PTR_TO_PACKET with checked range against PTR_TO_PACKET_END, PTR_TO_STACK, PTR_TO_MAP_VALUE_OR_NULL), path exploration with state pruning, bounded loops (reject when no progress bound is found), 512-byte stack across BPF-to-BPF calls, acquired reference IDs for `bpf_ringbuf_reserve` and `bpf_sk_lookup_tcp` that must be submitted/discarded/released on every path, and helper signatures for the helpers you support. Unsupported instructions or helpers are rejections, not accepts.
3. Linear host-side API: `BpfRingSlot` and `BpfSocketRef` builders that make it impossible to emit a program path that forgets release.
4. BPF ELF emission (use the `object` crate) with sections, license and maps (BTF can be minimal; full CO-RE `@btf_core` is out of scope) that `aya`-style loaders accept.
5. The §27.3 XDP SYN-flood mitigator (per-source SYN counting map, drop above threshold, ringbuf event) built with the API, verified by your verifier, and executed over packet fixtures in a userspace interpreter you write for testing (or `rbpf` if suitable).
6. Kernel load evidence: a test that attempts `BPF_PROG_LOAD` (via `aya` or libc in the gated module) and, if the host permits, asserts the kernel verifier agrees with yours on the accepted and a set of rejected programs; otherwise reports unavailable without failing.
7. lean/EbpfFirewall.lean: for the modelled filter, packet accesses are within checked bounds and the drop decision matches the specification.

Acceptance:
- Verifier tests with accepted programs and at least one rejection per rule; differential agreement with the kernel when available; mitigator behaviour over fixtures.
- No compiler eBPF backend from Vesper source (later wave).
~~~
