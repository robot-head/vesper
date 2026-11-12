# Parameter-aware source macro acceptance

Claim [#162](https://github.com/robot-head/vesper/issues/162) owns only its new
Rust executable test, fixtures, Python acceptance and this report, plus two
obsolete unsupported-parameter negative fixtures. The contract is
`docs/rfcs/params41-contracts.md`. CI/Bazel files and `scripts/test-*.sh` remain
exclusively reserved to external #158; none is edited here.

Five independent Rust tests exercise actual CLI source checking/expansion,
normal expanded-source checking and separate execution to42. Positive fixtures
cover parameter/global-name collision, both capture directions, initializer
shadowing, repeated argument provenance, in/inout/sink passes, actual linear
consumption and a SelfKw parameter. Exact BLAKE3 hashes and UTF8 byte ranges are
validated from real output. Header parameter renames must have actual source
anchors, preserve the original comment payload and agree with repeated body
references. Real linear duplication/leaks must retain their checker errors;
duplicate original names refuse before alpha-renaming. Generic, projected,
malformed and over-count parameter inputs cannot publish accepted artifacts or
partial expansion output. Same-header helper body drift must rerun authority.

The actual stdio test checks current unsaved parameter-bearing programs, expected
versions, updated helper authority, linear refusal and unchanged original-source
parse diagnostics/hover unavailability. Successful preview fields are compared
with separate actual Forge checks. No accepted preview grants entry execution.

The Python harness independently exercises all positive fixtures through actual
source check/expand and explicit normal check/run, plus unsaved stdio previews
with source-map/audit comparison. A real file deliberately differs from current
unsaved text and must remain untouched. Negative compilation never emits partial
expanded source. Formatting of original parameter syntax must preserve ordered
comment/token payloads and be byte-idempotent on the next check; expansion and
execution remain separate operations. The independent lexical inventory is
restricted to this closed fixture corpus, which has no nested comments.

Validation against the actual integrated executable
`/workspace/vesper/target/parallel-next/debug/vesper` at root checkpoint b77cce7
passed:

- Five independent Rust tests, zero failures and zero ignored tests. Eight
  positive expanded programs were checked and separately executed to42.
- Sixty-five Python commands, nine stdio sessions,47 JSON-RPC requests and nine
  explicit normal expanded-source runs to42. All accepted previews matched the
  actual separately compiled Forge artifact, maps and audits.
- The existing source-macro harness passed115commands and11explicit expanded
  runs after replacing only its obsolete unsupported-parameter fixture.
- Standalone Rust compilation and strict Clippy against existing read-only
  serde_json/blake3 artifacts, rustfmt, Python compilation and whitespace checks.

Initial executable validation caught a test fixture using unsupported Rust
`let mut` syntax. The fixture now uses actual Vesper `let` syntax and retains its
real inout argument passing. No production grammar was changed. All parameter
capture, header provenance, SelfKw, linearity, authority and formatter assertions
remain enabled.

No compiler dependencies, Wasmtime or additional toolchain were built by this
acceptance lane. Native Linux/macOS/Windows workflow coverage is coordinated
with #158 and remains separate pending evidence; these local actual Linux
results do not claim hosted execution on other platforms.
