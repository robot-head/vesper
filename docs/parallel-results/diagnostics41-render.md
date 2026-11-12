# §21.4 bounded shared rendering — issue #90

Delivered exclusively in new `crates/vesper-diagnostics/**` and this report, from
contract b4d1df8846d29afac7133774d00e39362b66deff, branch diagnostics41/render.
Read issue90, diagnostics41-contracts.md and applicable specification §21.4/21.5.
Document metadata remains 2026-11-12T09:18:30.204Z, snapshot
8e0f05ae32a6c87a8ce25fb339f66c760fe5d5f3fd3cb05e58236d291fa62207.
The core diagnostic model dependency 4ec73e3f030371758f99d7d6030afcd0efa6de25 was
cherry-picked privately as c1fd2b0. No dependency/root production edits belong to
this lane's source commit. The old protocol branches remain unchanged.

## Frozen API and behavior

```rust,ignore
pub struct Limits { pub span:vesper_span::Limits, pub max_name_bytes:usize }
pub fn prepare<'a>(index:&'a LineIndex, diagnostics:&[Diagnostic], limits:Limits)
    -> Result<Prepared<'a>,Error>;
impl Prepared<'_> {
    pub fn to_lsp(&self,uri:&str)->Result<Vec<serde_json::Value>,Error>;
    pub fn terminal(&self,file:&str)->Result<Vec<u8>,Error>;
    pub fn source(&self)->&str;
    pub fn data(&self)->&[vesper_span::DiagnosticData];
}
```

Limits::default/HARD/validate are public. Errors implement Display/stdError and
are InvalidLimits, Span(vesper_span::Error), Name, Allocation, Output and
UnsupportedLineEnding. Root can classify invalid compiler spans distinctly.
Prepared construction is private, borrows one immutable exact LineIndex and owns
checked DTOs; caller diagnostic mutation cannot change it. All aggregates and
primary/secondary/help locations are checked before metadata copying, followed
by the existing span::Diagnostics validation. DTOs are supplied evidence, never
proof of semantic causality. No fixes are minted.

LSP emits original primary code/message, severity1/source=vesper, exact UTF16
range, standard optional relatedInformation URI/range/message for each actual
related label in order, and optional data.help. Empty legacy metadata adds no
keys. Output bytes are counted exactly as compact JSON of the returned array,
including Unicode, string escaping, help, URI repetition and range digits,
before DOM copying; envelope/framing is the host's additional responsibility.
Full aggregate work/metadata preflights precede range scanning/DOM copying.

Terminal renders all actual labels/help through already-locked Ariadne0.5.1,
explicit IndexType::Byte, deterministic order, color(false), a private bounded
writer and an in-memory supplied Source. No file/cache/compiler/JIT/proof I/O.
Invalid byte/UTF8/CRLF-interior/reversed spans fail; no clamping, guessed causal
locations or partial outputs. Standard UTF16 LSP and terminal character columns
have their own correct conventions. Color disablement is not arbitrary escape
sanitization of preserved producer messages.

Ariadne normally omits the real empty EOF line after a trailing terminator. One
display-only blank preserves all original bytes/offsets while representing that
actual final empty line. This does not modify source identity or publish edits.
Empty source0..0, ordinary zero-width EOF and related zero-width EOF labels are
also supported/tested. Ariadne's VT/FF/NEL/U+2028/U+2029 separator conventions
differ from shared LineIndex; terminal explicitly refuses those sources instead
of wrong coordinates. LSP conversion still succeeds. Root handles any refusal
with truthful bounded raw code/message/byte-location unavailable fallback.

## Limits and honesty

Default: source128KiB, lines32769, metadata1MiB, work64MiB, output256KiB,
diagnostics256, total labels1024 including primary, aggregate text128KiB,
no edits/fixes, opaque URI/file name4096 UTF8 bytes without controls. HARD uses
span HARD: source1MiB/lines262145/metadata16MiB/work64MiB/output2MiB/
diagnostics4096/labels16384/text2MiB, name4096. Configuration validates every span
field and hard name cap; source line count is checked even when an existing
LineIndex was constructed with a larger profile. Zero budgets remain explicit
operational refusals where needed. No fixes/edits are synthesized regardless of
validated span fix/edit field settings.

Prepare charges both staging/final DTO metadata, source EOF/line lookup, all
endpoint scans and text copies before cloning. LSP work includes repeated
endpoint conversion, URI/escaped text and DOM shape; exact encoded output and
logical metadata are bounded before building DOM. Terminal bounds source/line
metadata, text and a conservative source/label layout score before Ariadne.
Nonlinear label layout means peaks can fail even inside individual count/source
ceilings; explicit Resource/Output failures are expected. The profile does not
promise every combination fits. Vec/String reserve is fallible; serde_json and
Ariadne bounded internal allocations follow their normal allocator behavior.
Charges bound logical/requested structures, not exact heap rounding/RSS/time.
No language-level ownership/Region, proof evidence or latency claim is added.

## Validation

Private target /tmp/vesper-editor41-protocol-target; every Cargo build used
CARGO_BUILD_JOBS=1, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0,
CARGO_INCREMENTAL=0. No new third-party package/version: Ariadne0.5.1 and all
standalone resolved registry versions already occur in the root lock. Temporary
standalone leaf workspace wiring was restored before delivery, leaf Cargo.lock
omitted and no root manifest/lock changed.

Executed successfully:

```text
cargo fmt --manifest-path crates/vesper-diagnostics/Cargo.toml -- --check
cargo test --manifest-path crates/vesper-diagnostics/Cargo.toml --all-targets
cargo clippy --manifest-path crates/vesper-diagnostics/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path crates/vesper-diagnostics/Cargo.toml --example causal_diagnostics
```

14 integration tests exercise actual astral/CJK/combining UTF8→UTF16 locations,
all actual related labels/help, deterministic terminal/LSP outputs, empty legacy
metadata, immutable prepared copies, every accepted byte-boundary pair against
LineIndex, invalid primary/related UTF8/interiorCRLF/reversed/out-of-range spans,
empty0..0 and all EOF forms, related zero-width EOF, exact JSON escaped byte
limits (all control bytes, quotes/backslashes, Unicode), exact terminal byte
limits, count/label/text/help/metadata/work/source/line/name ceilings, invalid
limits, default peak-work refusal and 1MiB CLI source with bounded output refusal.
The executable causal_diagnostics example checks real terminal/LSP Unicode/CRLF,
actual causal/help strings and final empty EOF line. Independent Beacon static
review found no renderer blocker; draft-test compile/Clippy issues were fixed.
Linux was executed; coordinator owns real CLI/framed-LSP, workspace and native
Linux/Windows/macOS integration/CI. This completes a bounded host renderer, not
the whole §21 recovery/PCG/completion/proof-lens design.

## Registration supplied to coordinator

Add crates/vesper-diagnostics to workspace members/default-members, workspace dep
`vesper-diagnostics = { path = "crates/vesper-diagnostics" }`, and root package dep
`vesper-diagnostics = { workspace = true }`. The delivered leaf manifest has no
standalone workspace block. Its only dependencies are vesper-core, vesper-span,
serde_json1 and already-locked ariadne0.5. Root owns all registration/lock and
CLI/LSP routing changes.
