# Current-source definition navigation for intra-doc links

Claims189–191 precede writers. Base is own PR184 head8efc612, with existing
checked link parser and all landed main changes through5d0922e. Fresh open issues
and actual external PR filenames139–147,149–151,163,165,173 show no navigation
overlap. Claude121–136/main/native and own pending Forge/docs/CI paths stay
reserved. No source writer starts before this contract is published.

|Claim|Branch|Exclusive writes|
|---|---|---|
|189|docnav41/integration|this contract, integration report, review, protected PR, later CI coordination|
|190|docnav41/provider|new navigation/doc_links.rs, narrow navigation.rs fallback, tests/doc_navigation.rs, provider report|
|191|docnav41/acceptance|new doc_navigation_cli.rs, fixtures/doc_navigation/**, check-doc-navigation.py, acceptance report|

Separate worktrees/private caches; root target/parallel-next is coordinator-only.
No Session/database/sourceDoc/Forge/core/main/native/Cargo/build/workflow/wrapper
edits. No new public request, Error variant, flags or raised limits. Claims stay
reserved through reviewed merge. Freeze amendments before dependent changes.

## Private API and existing route

```
// src/lsp/navigation/doc_links.rs
pub(super) fn target(
    snapshot: &CheckedSource,
    tokens: &[lexer::Token],
    offset: usize,
    limits: navigation::Limits,
) -> Result<Option<Span>, navigation::Error>;
```

Existing definition first validates/builds its current Index and strict UTF16
offset as today. Preserve ordinary lexical target priority, byte-for-byte output
and all existing error handling. Only when no ordinary target exists, invoke the
private fallback. It returns an exact ORIGINAL declaration-name token span; the
parent uses its existing same-URI Location/LineIndex/output cap. Half-open
reference hit spans include brackets/backticks; positions at the end do not hit.
Return None early unless the offset lies in an actual /// (not ////) or //! token.
Do not change hover, document symbols, method-call dispatch or initialization.

## Actual source inventory and link semantics

Use the same retained CheckedSource text and borrowed actual World. Build bounded
parallel LinkTarget/Span inventories directly from original Module declarations;
do not construct a full Document, native layouts/typestate graph or clone World.
Source functions must match actual World function span and not be generated;
source ADTs/effects and trait names must match source/checked metadata; inherent
methods use ty.name()::method and matching checked function span, including
i64::source_method without inventing a builtin type target. Effect operations
use the actual source/checked op under its effect prefix and matching span.

Use the actual lexer name token within that declaration's real span, via the
existing bounded Index.name logic or equivalent exact scan. Never select a
same-spelled body reference, string, comment, method on another type or parameter.
Exclude builtin-only declarations, generated Pact/model helpers, trait methods,
variants/fields and external/generic/Self targets. Keep distinct kind/path
collisions so the existing parser reports actual ambiguity; no namespace guess.

Run existing vesper_doc::links::analyze on that SAME source and finite actual
inventory, reusing its exclusions/count/path/resource semantics. A selected
resolved occurrence maps only to its unique exact descriptor/span. Other broken
links may make the report Rejected while successful occurrences still navigate:
no accepted Document/proof artifact is minted. Selected unknown, ambiguous,
unsupported, escaped/code/Markdown-excluded or absent references return None.

## Bounds and semantic boundary

Existing navigation source/token/depth/symbol/URI/output limits remain in force.
Target count <=max_symbols; enforce existing link source/path HARD caps. Use a
separate explicit1MiB metadata/16Mi logical work stage, prepaid vector/path
growth/copies and all AST/name/World/target scans. The reused links parser has
its own finite declared stage; do not reset budgets to retry a refusal. Resource
maps to existing Error::Resource, lexical/inventory invariant to InvalidSpan or
InvalidLimits as appropriate. No new Session error mapping or public approval.

No URI filesystem reads, cross-file lookup, macro/component/Lean or user-program
execution. Normal original-source parser/semantic guards remain authoritative;
macro preview cannot authorize original navigation. The API accepts no client
World/target list/Document/approval. It claims navigation facts only, no current
ownership, proof or new source CheckedSource. Invalid/stale source-index pairs
retain existing explicit failures.

## Actual acceptance

Direct provider tests compare real AST/World declarations and exact source token
ranges, ordinary lexical precedence, function/type/trait/effect/method targets,
primitive owners, same-path ambiguity, unknown/unsupported/excluded references,
Unicode/CRLF/strict UTF16 boundaries, resource limits and source association.

Real framed stdio acceptance uses current unsaved buffers with deliberately
different on-disk text, edits/rename/current versions/rejected stale changes,
close/reopen and exact same-URI Locations. Preserve original parser/semantic
unavailable behavior and existing navigation. Rust override
VESPER_DOC_NAVIGATION_BINARY and Python --binary use an actual read-only binary.
No fabricated compiler/proof artifacts or guessed prefix CheckedSource.
