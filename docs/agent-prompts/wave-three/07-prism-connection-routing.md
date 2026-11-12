# Bounded Prism connection codec and routing

Suggested branch: `wave3/prism-connection-routing`.

~~~text
You are implementing one substantial milestone in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline 02b3a1d175efb5d2561b7fa5b0aaf80121ae1bc6 or a descendant; record the actual SHA. Read AGENTS.md if present, docs/specification.txt, docs/implementation.md, docs/parallel-results/wave-two-integration.md, docs/rfcs/parallel-contracts.md and docs/rfcs/wave-three-contracts.md before editing.

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; explicitly reject unsupported cases.
- Edit only the exclusive paths below. Root Cargo.toml/Cargo.lock, src/main.rs/src/lib.rs, CI and global docs belong to the coordinator. Supply a precise registration/dependency patch separately. Temporary UNCOMMITTED root dependency/module edits for local testing are allowed; do not include them in your commits.
- Existing crates stay registered. Only a NEW unregistered crate may temporarily declare its own [workspace]/Cargo.lock; integration removes those. Preserve the separate sandbox guest workspace and current validated dependency versions.
- Use an isolated target directory per checkout/worktree. Do NOT share CARGO_TARGET_DIR between source-divergent compiler worktrees. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Integer arithmetic is vesper.arith.checked/1; possible +, -, *, negation and MIN/-1 overflow uses typed Fail[ArithOverflow]. Explicit wrapping_* calls are total. RuntimeError remains separate. Interface semantics version is 2. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics and native ABI v2 are frozen for this wave. Lower new surface constructs to existing core nodes before backend consumption. Required schema changes go to the coordinator; do not silently add variants or patch another lane.
- src/lean.rs, src/lean_ir.rs, src/lean_nodes.rs and crates/vesper-lean-syntax/** are coordinator-owned. Lean emission now builds a full AST and renders once; never reintroduce raw Lean fragments. crates/vesper-loom-tuf/** is also coordinator-owned.
- Preserve owners on failed consuming operations. Unsupported, ambiguous, invalid evidence, overflow and exhausted budgets are errors, never success. Do not weaken safety checks to fit an example.
- Content hashes are not provenance, public residual effect rows are not authority audits, replay is not DPOR, and executable Lean is not a preservation/termination theorem. Report actual hardware/kernel/toolchain evidence and unavailable environments accurately.
- Read other lanes' APIs without editing them. Keep new tests/examples/report files in your boundary. Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run formatting, focused tests and strict all-target Clippy. For changed execution paths, use real execution; compilation/object generation alone is a narrower result. Record exact commands/results and limitations.
- Commit and push YOUR branch only; do not merge main. Return base/commit/PR, public API, checks, coordinator patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Bounded Prism connection codec and routing.

Exclusive ownership:
- crates/vesper-prism/**.
- docs/parallel-results/prism-connection-routing.md.
Compiler/runtime/native, SIMD codegen, FFI, allocator and root CLI/CI are read-only. Preserve existing strict parser/response behavior and tests; add compatible inspection metadata where needed.

Baseline: dependency-free bounded incremental HTTP/1.1 request decoder and deterministic consuming fixed-length response builder. It rejects CL/TE ambiguity, duplicate Host/CL, alternate numeric hosts, chunked/upgrade/CONNECT/Expect; response supports HEAD, 204/304/205 rules and controls framing headers. There is no connection state machine, router, socket server or source linear responder.

Implement:
1. A bounded socket-independent connection state machine composing current decoder and response encoder. Track request ordering, pipelined remainder, application-owned requests/responses, outbound cursor and partial writes. Never reserialize a suffix incorrectly or lose/duplicate a response after a short write/error. Return retained owners/unsent bytes on failures/shutdown.
2. Explicit opaque request/response association and close policy: stale/foreign/duplicate/out-of-order submissions cannot corrupt wire ordering. Handle Connection: close, EOF, parse poison and queued pipeline work deterministically. Respect HEAD automatically from the associated request and retain existing body-forbidden/framing checks.
3. Bounded static/parameter routing (no regex engine required) with deterministic precedence and ambiguity rejection. One central URI/path/query interpretation is shared by route matching and middleware; decode exactly once. Define percent escapes, plus handling, UTF-8/bytes, encoded slash/backslash/NUL, dot segments, empty/trailing segments and duplicate query keys explicitly. Expose typed/validated target data instead of encouraging repeated ad hoc decoding.
4. Bound queued requests, retained input, representations, outbound bytes, routes, segments, params/query entries and processing work. Validate arithmetic before allocations. Caller-retained outputs remain explicit caller responsibility.
5. Keep fixed-length scope. Do not silently add chunked/trailers, HTTP2/3, WebSocket, TLS, real sockets, async scheduler integration or source-language linearity in this milestone. Rust consuming APIs still permit Drop; report that distinction.

Acceptance:
- Feed real raw requests at every split; ordered responses survive all partial-write sizes and pipelining, with exact binary bytes.
- Tests cover stale/duplicate associations, out-of-order application completion, close in a pipeline, malformed later requests, EOF and error recovery retaining output owners.
- Route/target tests cover static-versus-parameter precedence, ambiguous routes, duplicate query keys, invalid UTF-8/escapes, encoded separators/NUL, double-decode probes and HEAD/body rules.
- Existing parser and response tests remain green; no HTTP desynchronization rescan after parse errors and no unbounded buffering.

~~~
