# Actual causal diagnostic acceptance — issue #92

Branch `diagnostics41/acceptance`, base
`b4d1df8846d29afac7133774d00e39362b66deff`. This lane owns only
`tests/diagnostics_cli.rs`, `scripts/test-diagnostics.sh`,
`scripts/check-diagnostics.py`, `.github/workflows/diagnostics.yml` and this
report. No production source, manifest, lockfile, server or renderer edits.
The causal model/producers/guard/renderer and host routing belong to #87–#91/#93.

## Actual fixtures and preserved contracts

The acceptance suite invokes the real `vesper check FILE --json`, plain stderr
renderer, and `vesper lsp` over actual Content-Length frames. It uses no compiler,
server or renderer mock. Expected causes come from the actual source/IR owners:

| Original diagnostic | Actual primary | Required actual secondary evidence |
| --- | --- | --- |
| E0303, `use of moved place 'tx'` | second ownership-taking initializer | binding parameter and earlier ownership-taking initializer |
| E0301, overlapping arguments violate alias XOR mutability | later conflicting argument | earlier overlapping argument evaluation |
| E0402, potentially abortable Ask may discard live linear tx | actual `Ask::ask()` call | live linear parameter declaration |
| E0410, abortable effect may abandon linear temporary `%0` | actual `Fail::raise(1)` call | the preceding `Tx {}` temporary definition |

The first three fixtures pin the source owner's exact invariant help and ordered
label wording. All four pin unchanged original codes/messages/primary byte spans;
IR help remains actual nonempty producer metadata and must survive conversion.
Shadowing asserts that causes identify the inner binding and inner consumption,
excluding the already-consumed outer parameter and its unrelated earlier move.
There are no fabricated abort/commit method calls, unchecked rewrites or fixes.

Same-line emoji block comments and CRLF make UTF-8 byte columns differ from
UTF-16 columns. Expected source spans are computed independently from fixed
fixture text. Framed assertions compare the actual CLI diagnostic with standard
LSP `relatedInformation:[{location:{uri,range},message}]`, preserving primary
code/message/range and ordered label messages. Help is exactly
`diagnostic.data.help`. No top-level secondary URI/range substitute is accepted.
No edit/fix object may be minted from a diagnostic.

Full buffer replacements add Unicode/CRLF lines before declarations. Tests check
fresh primary/secondary positions, version and source hash after the edit, then
verify actual semantic recovery clears diagnostics. Rust asserts exact BLAKE3 of
each source revision; Python independently checks hash shape and changed-byte
identity freshness. The Python suite uses real existing file URIs and verifies
buffer edits/recovery do not write those source files.

Unknown-name failures retain the original three-key JSON (`code`, `message`, `span`)
with empty optional metadata omitted. A trailing-CRLF syntax failure retains its
zero-width EOF byte span and exact current UTF-16 EOF location. A U+2028 inside
a block comment exercises the renderer's explicit terminal-only line-profile
refusal: plain stderr must say rendering is unavailable, then preserve the raw
E0201 message and exact `UTF-8 bytes start..end`. LSP still maps the source
correctly through the shared index rather than clamping or guessing a new line.

A flat bounded source with 70 ownership attempts produces more than the editor's
64-diagnostic limit. It must publish one explicit severity-2 unavailable warning
with current source hash/version and validated empty file-level origin range,
without partial E0303 causes or relatedInformation. This is a bounded diagnostic
batch, not an exponentially growing inference/checker fixture.

Forged client diagnostics carry invented causes, help, secret URIs, replacement
text and verification flags. A real code-action request over valid source must
still return no action; no forged text/cause may appear in server responses.
The client negotiates literal actions and versioned documentChanges normally.
The suite neither applies returned actions nor invokes `--fix`, JIT, run,
native/WASM builds or proof commands.

## Scripts and native workflow

`scripts/check-diagnostics.py --binary PATH [--work DIR]` uses only Python's
standard library. It creates and cleans an owned temporary child directory;
caller-provided work directories are not removed. CLI commands and stdio
sessions have 45-second subprocess timeouts and bounded captured-output checks.
The frame reader checks exact byte lengths, bounded headers/replies, unique
decoded output keys, UTF-8, and complete framing. Unicode/newline conversions
are independent from the production LineIndex.

`scripts/test-diagnostics.sh` builds the real locked binary, runs the disposable
Python acceptance, then executes the actual renderer's self-checking
`causal_diagnostics` example. Shell, Python and Rust binary selection all prefer
an existing `.exe` candidate before relying on an extensionless alias, including
MSYS/native-Python Windows behavior. Rust tests normally use Cargo's actual
binary; optional `VESPER_DIAGNOSTICS_BINARY` selects a read-only integrated
binary for isolated acceptance without copying production into this worktree.

`.github/workflows/diagnostics.yml` selects Ubuntu, Windows and macOS. It checks
formatting, core/span/renderer tests and targets, actual source/IR causes,
frontend diagnostic bounds, the new CLI/stdio tests, strict all-target Clippy,
the disposable acceptance script and renderer example. It has read-only content
permissions, one Cargo job, debug information off, incremental compilation off
and a bounded job timeout. This workflow configures future native observations;
local Linux results do not claim native Windows/macOS success.

## Verification

Private target `/tmp/tooling41-lint-target`, one build job, debug0/incremental off.
The unchanged base binary independently confirmed original codes, messages and
primary byte spans before richer metadata assertions were written. New tests
deliberately require genuine metadata and cannot pass on the old minimal
three-field-only implementation.

Static checks passed:

```text
cargo test -p vesper --test diagnostics_cli --no-run
cargo clippy -p vesper --test diagnostics_cli -- -D warnings
rustfmt --edition 2024 --check tests/diagnostics_cli.rs
bash -n scripts/test-diagnostics.sh
Python source compilation; native workflow YAML parsing
git diff --check
```

Runtime validation passed against the coordinator's final read-only binary
`/workspace/vesper/target/parallel-next/debug/vesper`, incorporating final source
causal captions (`30dcf4b...`), IR producers (`4d6059a...`) and the actual shared
renderer. No root target writes or production copies were used:

```text
python3 scripts/check-diagnostics.py --binary ROOT_READONLY_BINARY
  passed: 16 actual CLI checks, 10 actual framed stdio sessions
VESPER_DIAGNOSTICS_BINARY=ROOT_READONLY_BINARY cargo test -p vesper --test diagnostics_cli
  all 7 tests passed
cargo clippy -p vesper --test diagnostics_cli -- -D warnings
  passed
```

The shell wrapper is syntax-checked locally; its locked build plus renderer
example run execute from the integrated workspace/selected CI, where the new
renderer package exists. The renderer owner separately passed its actual
`causal_diagnostics` example and tests. This lane's old-base compilation is only
a test-harness compile check; runtime results above come from the final actual
integrated implementation, not that old minimal binary.

No full §21 completion, ownership recovery/completion, automatic fix, proof,
constant-time/RSS or sub-2ms guarantee is implied by these acceptance tests.
