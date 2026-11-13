# Checked local intra-doc links (§40.2)

Claims #177–180 precede writers. Base is tuple PR176 head
ce231aecc86fe3fd01b0be906f39dbb642f3b95f. The live Google Doc was reread and
its revision matches `docs/specification-source.json` (41 sections/Appendix A).
Fresh open issues and actual external PR files139–151,163,165,171,173,175
were checked. External158/171 owns every workflow, build/Bazel file and
scripts/test-*.sh, explicitly including crates/vesper-doc/BUILD.bazel. README174,
domain/main/native121–136 and existing Codex stack claims remain reserved.

## Exclusive ownership

| Claim | Branch | Writes |
| --- | --- | --- |
| #177 | links41/integration | this contract, integration report, review and protected stack integration |
| #178 | links41/library | new vesper-doc/src/links.rs, lib.rs narrow exports/Document metadata/typed rendering, new tests/links.rs, library report |
| #179 | links41/adapter | tooling/doc.rs narrow integration, new tooling/doc/links.rs, new tests/doc_links.rs, doc_commands.rs narrow help/render wiring, adapter report |
| #180 | links41/acceptance | new tests/doc_links_cli.rs, fixtures/doc_links/**, scripts/check-doc-links.py, acceptance report |

No other existing tests, Cargo/dependencies, core checker/AST/IR/native, Forge,
LSP session, CLI registration, proof emitter, workflow/build or wrapper edits.
Separate worktrees/private existing caches, one Cargo job/debug0/incremental0.
Root target/parallel-next is coordinator-only. Claims remain reserved through
reviewed merge. Publish amendments before dependent interface changes.

## Frozen data and APIs

Public module `vesper_doc::links` provides:

```
TargetKind { Function, Type, InherentMethod, Effect, EffectOperation, Trait }
LinkTarget { path: String, kind: TargetKind }
LinkReference { span: Span, display: String, target: LinkTarget, anchor: String }
LinkDiagnostic { code: String, span: Span, message: String }
LinkStatus { NotAnalyzed, Resolved, Rejected }
LinkReport {
    status: LinkStatus,
    source_bytes: usize,
    targets: Vec<LinkTarget>,
    references: Vec<LinkReference>,
    diagnostics: Vec<LinkDiagnostic>,
}
analyze(source: &str, targets: &[LinkTarget], limits: LinkLimits)
    -> Result<LinkReport, LinkError>
anchor_id(kind: TargetKind, path: &str) -> String
document_targets(document: &Document, limits: LinkLimits)
    -> Result<Vec<LinkTarget>, LinkError>
validate_report(report: &LinkReport, targets: &[LinkTarget], limits: LinkLimits)
    -> Result<(), LinkError>
```

Use snake_case serde enum representations, deny unknown DTO fields, and stable
`unknown`, `ambiguous`, `unsupported`, `resource` diagnostic/error codes where
applicable. LinkError implements Display/Error; implementation may add bounded
resource/internal error detail. Semantic link failures yield a Rejected report;
allocation/lexical/resource/configuration failures return Err. Resolved has no
diagnostics. NotAnalyzed defaults to no source bytes, targets, references or
diagnostics; it makes no source-analysis claim.

Document gains `#[serde(default)] pub intra_doc_links: links::LinkReport`.
The root adapter updates its existing Document literal. No new dependency or
cryptographic source-hash field is needed: actual source association comes from
the checked adapter's retained original text and exact lexical spans. These
public inspection DTOs never certify checked source, proof or authority.

Targets are ordered deterministically by path/kind. Distinct declaration kinds
with one path remain distinct and make that reference ambiguous. Do not guess
namespace preference. Duplicate identical descriptors are invalid inventories.
Anchor IDs use a fixed kind tag and complete UTF8 path bytes encoded in ASCII
hex, never a caller-supplied URL. Reference spans include the original brackets,
are absolute exclusive-end UTF8 byte ranges, and retain every repeated occurrence
in source order. Display is the exact bracket content, including optional
backticks; target.path is its parsed canonical local path.

## Grammar and finite accounting

Only actual lexer `///` (excluding `////`) and `//!` comment tokens participate;
string contents, ordinary comments and code do not. Supported references are
`[Type]`, `[function]`, `[Type::method]`, `[Effect::operation]` and those exact
forms with a single inline-code delimiter inside the brackets, such as
`[\`Type::method\`]`. Paths consist of lexer-compatible ASCII identifier segments
joined by `::`. A syntactically supported but unknown or multiply matching path
is an error, retaining its exact occurrence span. Explicit `Self`, package paths
and generic paths receive unsupported diagnostics instead of guessed resolution.
Other bracketed prose, including `[T-CallProj]`, remains literal.

Escaped brackets, ordinary inline code spans, fenced code, images, explicit
Markdown URLs and reference-style Markdown links remain literal. Recognize
matching backtick runs for inline code and backtick/tilde fenced blocks with at
least three delimiters. Fence/inline state never crosses a noncontiguous doc
comment group (a blank source line, ordinary comment or code breaks the group).
Ignore entire excluded constructs so an inner bracket cannot become a link.
This is a deliberately bounded intra-doc profile, not a general Markdown engine.

LinkLimits defaults/HARD: source bytes128KiB/256KiB, targets1024/4096,
references1024/4096, path bytes1024/2048, path segments8/16,
metadata1MiB/4MiB, work16Mi/64Mi. Fields are public with corresponding max_ names;
all zero or above-HARD settings fail before allocation. Exceeding path limits is
a Resource failure, not prose. Prepay lexer storage, vector growth/copies,
target/path copies, bounded scans/resolution work and diagnostics before their
allocation; checked arithmetic throughout. Retain existing document source,
AST, nominal/layout, type and serialized-output guards. These logical guards
make no timing or RSS guarantee.

## Checked adapter and rendering

Both document_source and document_checked keep their actual parser/checker or
sealed-snapshot boundary. Only after successful checking and actual Document
metadata extraction, build the finite local target inventory and analyze the
same retained source. A non-Resolved result prevents successful document return.
The CLI's bounded failure includes at least first diagnostic code and absolute
byte span. Actual syntax/type/linear errors still take precedence over links.

document_targets enumerates documented source functions, source ADTs/effects/
traits, explicit inherent methods from the documented function inventory and
effect operations under their effect prefix. Exclude builtin-only declarations,
private generated Pact helpers and trait/model implementation helpers. Trait
names may link; trait methods, variants, fields, generic instantiation, Self and
external packages are outside this batch. No invented declarations or deep World
clone. Root tests compare this inventory with real compiler metadata.

Preserve generic render_html's existing escaped-output behavior. Add typed
`render_document_html(document: &Document, report: &impl Serialize, limits: Limits)`
for Document.html and CLI report HTML. It validates link metadata against targets
recomputed from that actual Document before emitting any href/id. It must not
depend on a deep JSON Value parse succeeding to render required link targets.
Legacy NotAnalyzed documents remain safely escaped. Rejected, malformed, foreign
target or altered-anchor metadata refuses typed rendering. Every accepted
fragment href has exactly one actual target element, including nested effect
operations. The linked-doc section may show original comment tokens with links
while the existing overview stays literal; preserve exact source text/escaping.
All other text and attributes are escaped, no script/external URL/network or
HTML supplied by a reference. Unverified proof status and original CLI readonly,
create-new output and failure cleanup semantics stay unchanged.

## Actual acceptance

Use real checked source, JSON and HTML commands, independently verify every
fragment target, exact UTF8/CRLF occurrence bytes, determinism, forward/repeated
references, inherent methods/effect ops, source edits, ambiguity and invalid
source precedence. Cover exclusions, hostile labels/serialized metadata and all
limits without weakened expectations. Existing doc/search/doctest tests remain.
Executable tests support VESPER_DOC_LINKS_BINARY; Python takes --binary actual
read-only executable. Coordinate targets with external158, no workflow edits.
