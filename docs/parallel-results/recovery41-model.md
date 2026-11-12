# #95 sealed AST-free recovery syntax model

Owned paths: new syntax `src/recovery.rs`, its `src/recovery/model_tests.rs`,
syntax Cargo.toml's existing blake31.8 dependency, and this report. Fresh branch
`recovery41/model` from `fc75413`. Read issue #95, recovery41 contracts and actual
lexer/parser guard/CST APIs. Core AST/checker/IR/interface/proof APIs and existing
batch parsing remain unchanged. Root owns lib registration and root lock update.

`RecoverySyntax` has private fields and no public constructor. It retains exact
Arc<str>, source blake3 bytes, actual lossless GreenNode, actual syntax diagnostics,
declarations and sites. Public immutable getters are `source()->&str`,
`source_arc()->&Arc<str>`, `source_hash()->[u8;32]`, `syntax()->SyntaxNode`,
`diagnostics()->&[Diagnostic]`, `declarations()->&[Declaration]`, and
`sites()->&[RecoverySite]`. No AST, World, checked-program flag, semantic binding,
ownership/liveness fact, fix or proof is stored/exposed. A compile-fail doctest
checks external construction is unavailable.

Frozen descriptors are Clone/Debug/Eq. Declaration has kind, optional name_span,
span/header_span, optional body_span/body_green_span, status and a pub(crate)
body_context. Kinds are Function/Struct/Enum/Effect/Trait/Impl/Model/Use/Other;
statuses Complete/Recovered/Opaque. RecoverySite has span/kind/optional owner index
and optional Receiver { root, projections }, containing token byte ranges only.
Site kinds are MissingMember/MissingDelimiter/Unexpected/OpaqueLexical/OpaqueMacro.
All three enums expose snake_case `as_str()` for stable host inventories.
Completeness/context classification belongs to the trusted parser, never a global
diagnostic-overlap heuristic or public caller assertion.

The pub(crate) free `assemble(Arc<str>,GreenNode,Vec<Diagnostic>,Vec<Declaration>,
Vec<RecoverySite>,Limits)` validates the complete aggregate before publication:

- Config/source caps before hashing, exact token/source byte equality, MODULE
  root, finite CST depth/token/event work. Nothing pads or reconstructs source.
- Every primary/secondary/declaration/header/name/body/site/receiver UTF-8 range;
  reversed, out-of-bounds and CRLF-interior endpoints fail explicitly.
- Declaration order/nonoverlap/containment, exact actual name tokens, body subtree
  presence and exact body_green_span/body_span equality. A sealed eligible body
  additionally requires the actual agreed editor BLOCK kind205, confirmed with
  the parser owner, not an arbitrary node sharing the same range.
- Internal BlockContext contains bounded row_names/bound_values/no_struct; Some
  requires Complete Function with actual name/body/BLOCK. Only the parser can
  classify top-level nongeneric eligibility. It stays inside the source-bound
  snapshot; incremental reuse must additionally prove the header is unchanged.
- Missing-member/delimiter sites are zero width. Owner indices are bounded, but
  a missing cursor may legitimately be at a following sibling anchor. Receiver
  root/projection spans must be real allowed tokens in order. Connecting-dot/
  whole-expression classification remains trusted parser evidence, not a claim
  that the assembler resolves or independently proves receiver syntax.
- All diagnostic related/help/context/receiver strings and element records count
  toward aggregate metadata/work. Errors have bounded static Display messages.

`Limits { parse:ParseLimits,max_sites,max_declarations,max_metadata_bytes,max_work,
max_identifier_bytes }` has public validate()/preflight(&str). Defaults are
source128KiB/tokens16384/diagnostics64/depth48/sites256/declarations256/
metadata1MiB/work16MiB/identifier256B. HARD is source1MiB/tokens65536/
diagnostics128/depth128/sites4096/declarations4096/metadata16MiB/work128MiB/
identifier1024B. Depth0 and fields above HARD fail configuration. Counts and
arithmetic fail rather than wrap. Model CST depth includes root/ITEM/BLOCK wrapper
nodes and can differ from parser descent depth at a tight cap; both are enforced.

Metadata accounting is logical live descriptor/diagnostic/String-record payload
and text; BTree allocation headers/capacity slack are not a process-memory claim.
Already producer-owned GreenNode/token/parser staging allocation is separately
bounded by parse source/token/depth/work profiles and is not counted as new model
inventory storage. The assembler moves supplied values without cloning their
text, but cannot retroactively prevent allocations already made by its trusted
producer. Those producers must apply preflight and identifier/context bounds
before their clones. Work units count token-comparison/hash bytes, CST events,
span checks, text bytes, bounded token lookups and visited body-search nodes;
fixed container/index implementation overhead additionally has finite hard
source/token/depth/count bounds. These are logical units, not CPU instructions,
RSS, OOM, latency or real-time guarantees. Internal assembly-only dead-code lint
allowances explain independent model compilation before parser/incremental
integration; no public parser stub or unchecked public constructor is provided.

Validation using reused private `/tmp/vesper-target-editor41-span`, one job,
dev/test debug0 and incremental0:

- 12 model unit tests PASS: exact Unicode/CRLF/Arc/hash/CST bytes, actual subtree
  and BLOCK/context checks, zero-width original sites, next-anchor ownership,
  invalid CST/root/spans/identifier/receiver/context/order, all direct caps and
  rich diagnostic/help metadata accounting.
- 10 existing syntax limits integration tests PASS, preserving ordinary batch
  behavior and actual hostile-input/balanced lossless guards.
- One private-construction compile-fail doctest PASS.
- Strict syntax all-target Clippy (`-D warnings`) PASS, rustfmt and diff checks PASS.

For isolated validation, syntax lib registration and standalone workspace/dependency
manifest wiring were temporary and restored before commit; no leaf lock remains.
Root source/lib/Cargo/lock/cache were not written. Committed syntax manifest only
adds `blake3 = "1.8"`, reusing the existing registry dependency. Root registers
`pub mod recovery;` and updates the existing root lock package dependency edge.
No standalone executable is meaningful for this sealed model; the parser and
incremental lanes provide real original-source entry points and oracle tests.
