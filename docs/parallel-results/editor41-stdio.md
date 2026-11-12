# #85 actual editor stdio acceptance

Owned slice on `editor41/stdio`, fresh base `bbd19af`: only
`tests/lsp_cli.rs`, `scripts/test-editor.sh`, `scripts/check-editor.py`,
`.github/workflows/editor.yml`, and this report. The dispatcher and session
implementation remain coordinator/Quarry-owned. Read the editor contracts and
actual span/protocol/navigation/action/query APIs before forming the traffic.

The three Rust integration tests launch the actual `CARGO_BIN_EXE_vesper lsp`
child, write real Content-Length frames, concurrently drain bounded stdout and
stderr, enforce a 45-second child timeout, and parse actual framed replies:

- Initialize capabilities, Unicode URI/astral comment/CRLF text, actual checked
  hover and definition, document/workspace symbols, accepted body-edit line
  movement, surrogate-interior edit rejection, stale-version rejection, a valid
  retry using the failed edit's version, close and closed-document refusal.
  Fresh symbol positions are compared to exact literal UTF-16 coordinates.
- Initialize-before-features, strict decoded duplicate envelope keys, malformed
  known-length JSON with continuation, unsupported method, missing query,
  truthful analysis-unavailable feature errors, shutdown-phase request refusal,
  and shutdown/exit completion.
- Fatal duplicate lengths, bad CRLF, truncated body, oversized header and exit
  before shutdown produce process failure.

The standalone Python harness independently parses response framing and rejects
JSON decoded duplicate keys. It sends actual requests and applies actual
returned UTF-16 edits. It covers capabilities/navigation/current positions,
formatting/token-comment sample/idempotence/four-space behavior, freshly checked
quick fixes and explicit fix-all with versioned documentChanges and SemVer/
source/candidate metadata, and sends returned candidates back through checked
server analysis. Forged client diagnostics/fix/verified payloads on lint-free
source must return no actions. Structural signature search must match only the
actual compatible return type, include source identity and unverified proof
status, and reject absent/null/unknown-field queries. Parse failures and guarded
recursive unsupported source cannot produce checked navigation. Framing failures
remain fatal; known-length malformed/duplicate bodies yield null-ID errors and
allow the next request. URI labels point at a disposable actual file containing
different code, demonstrating the opened buffer drives symbols and the disk
file stays unchanged. Only invocation-owned temporary directories/files are
created; tracked fixtures are untouched.

`scripts/test-editor.sh` builds the actual locked root binary, unconditionally
prefers its `.exe` if present (avoiding the MSYS extensionless alias problem),
runs disposable Python acceptance, and executes the self-checking `source_edits`
and `protocol_documents` leaf examples. A caller can set a private Cargo target;
the default shares normal CI cache. No JIT, native build from opened source,
proof subprocess, hardware or latency threshold is an acceptance claim.

The dedicated read-only-contents workflow runs Ubuntu/Windows/macOS with Python
3.13, stable Rust/rustfmt/Clippy, one build job, zero debug, incremental disabled,
normal target caching and a 45-minute job timeout. It selects span/protocol/
formatter/lint/doc leaves, root library/provider/database/stdio integration tests,
strict all-target selected Clippy, actual examples and black-box acceptance.
PR/main filters include all consumed source crates, root production paths,
matching tests, scripts, workflow and Cargo files. No new proof model/job.

Checks completed before the coordinated server integration:

- `rustfmt --edition 2024 --check tests/lsp_cli.rs`: pass.
- `bash -n scripts/test-editor.sh`: pass.
- `python3 -m py_compile scripts/check-editor.py` and `--help`: pass.
- Direct Python helper checks for real UTF-8 wire lengths, astral UTF-16 edit
  application, CRLF positions and decoded duplicate output rejection: pass.
- Workflow parses with installed PyYAML and has the three intended native hosts.
- Rust test typecheck/link with `rustc --test -D warnings`, reading existing
  root dependency artifacts but writing only `/tmp/vesper-editor41-stdio-tests`:
  pass. No Cargo/shared-target mutation or duplicate root dependency build.
- `git diff --check`: pass.

At this commit the coordinated dispatcher/session replacement is not linked;
the existing 13:21 binary is the older minimal server. Thus **no runtime pass of
these new server acceptance tests is claimed**. The coordinator must run
`cargo test --locked -p vesper --test lsp_cli`, strict root Clippy and
`scripts/test-editor.sh` after integrating the server, and run/observe the
three-host workflow. The tests intentionally enforce the frozen implemented
capabilities and explicit status/error schema; they must not be weakened to
accept the old minimal server. If integration reveals a real payload mismatch,
report it and update the owned acceptance path with the agreed final contract.
