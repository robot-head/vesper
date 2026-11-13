# Checked local intra-doc link integration (§40.2)

Claims #177–180 and the frozen `docs/rfcs/links41-contracts.md` preceded source
writers. Fresh open issues and actual external PR filenames were checked first,
including Warp integration173 and README175. Separate library, checked-adapter
and executable-acceptance worktrees used exclusive paths and private caches.
This source batch changes no workflow, build/Bazel file, dependency, test wrapper,
core checker, Forge, native ABI or Lean emitter.

`vesper doc` now resolves supported references such as `[Tx::commit]` only after
real complete-source checking, or from a genuine retained CheckedSource. It
uses the actual documented local function/type/effect/trait inventory, explicit
inherent methods and effect operations. Source-defined methods on builtin types
are included without inventing builtin type declarations. Private generated
Pact helpers, builtin-only declarations and trait/model implementation helpers
are excluded. Genuine same-path type/effect declarations produce ambiguity;
there is no guessed namespace preference or external package lookup.

The lexer-based finite parser retains absolute original UTF8 byte spans for
every resolved occurrence, including repeated and backticked references. It
excludes ordinary comments, strings, escaped brackets, matching inline code,
fenced code, images and explicit/reference-style Markdown constructs. Group
state resets across blank source lines, ordinary comments and code. Resource
and configuration guards precede allocations and scans; no existing document
or output limit is raised.

Unknown, ambiguous and explicitly unsupported references prevent successful
document return. The bounded CLI error includes the first diagnostic code and
original byte range. Actual parser, type and linear errors retain precedence.
Direct source and sealed snapshot paths produce identical metadata for the
same actual retained text; declaration edits recompute link resolution.

Typed HTML rendering recomputes the Document's target inventory and validates
status, source length, exact retained comment/display bytes, occurrence ordering
and anchors before emitting links. Anchors encode kind and complete path bytes
as ASCII hex. Every emitted fragment has exactly one real local declaration
element, including nested effect operations. A linked-source section preserves
the original escaped comment text; existing overview/complete JSON remain.
Required anchors do not depend on serde JSON Value parsing succeeding for deep
metadata. Generic render_html retains its legacy escaped behavior.

## Review and actual evidence

Independent review checked source association, UTF8 arithmetic, malformed public
DTOs, anchors, escaping, deep JSON fallback and logical resource accounting.
Corrections include prepaid temporary effect-operation paths, allocation-free
inventory sorting and a separate typed-render work/anchor envelope. Concrete
Markdown regressions cover multiline destinations/titles/reference labels,
apostrophes inside bare URLs and parenthesized titles, code-span closing brackets
inside labels, and continued reference-definition titles. An outside unknown
reference must remain visible after each excluded construct. Final review found
no remaining actionable issue within the frozen profile.

Final local Linux validation on Rust1.99.1:

- 111 tests across eight integrated suites, zero failures/ignores: root library,
  existing doc CLI/doctests, new checked-adapter/CLI, signature database, stdio and
  navigation tests. All nine new adapter and five new executable tests pass.
- 38 pure documentation-library tests pass with zero failures/ignores, including
  24 new parser, inventory, hostile metadata, HTML and resource cases and all
  14 existing contracts. Strict library all-target Clippy passes.
- Final `cargo clippy --workspace --locked --all-targets -- -D warnings` passes;
  formatting and whitespace checks pass. Targeted strict adapter checks pass.
- Actual `check-doc-links.py --binary REAL_VESPER` passes49commands on the final
  hardened executable. The independent five-test Rust executable harness also
  passes on that same binary. These overlap the integrated CLI tests; they are
  not a claimed count of distinct programs or execution of documented examples.

The earlier reference-cap corpus used adjacent `[name] [name]`, which is an
excluded Markdown reference-style construct. Comma/semicolon-separated1025real
references now exercise the unchanged actual resource refusal, preserving strict
diagnostic/span and no-partial-document assertions. No production cap or expected
failure was weakened to accommodate that corpus correction.

## Boundaries and protected integration

These public DTOs are inspection data, not checked-source or proof certificates.
The checked adapter supplies actual original lexical association. A renderer
receiving a deserialized DTO can validate retained comment bytes and safe HTML
consistency, but cannot reconstruct all original inter-comment source gaps.
No proof badge, compiler authority or complete Markdown validation follows from
public DTO validation. Existing unverified proof status is unchanged.

The profile supports local ASCII identifier paths and explicit inherent/effect
operation links. Package paths, Self, generic instantiation, trait methods,
variants and fields remain outside this batch. General Markdown rendering,
audited proof anchors, other §40 capabilities and the broader design remain
unfinished. No latency, RSS, RBE or other-platform execution is claimed.

This draft stacks on tuple PR176. External Bazel171 landed while implementation
was underway; its source changes and owner README are preserved. Separate claim
#181 reconciles the already-published implementation stack and real acceptance
wrappers with that landed CI. Source writers here did not edit those paths.
New Rust targets `vesper-doc/tests/links.rs`, `doc_links` and `doc_links_cli` are
Cargo-metadata discoverable. Python takes --binary; Rust executable tests accept
VESPER_DOC_LINKS_BINARY. Hosted current-head checks remain pending, including
owner coordination of the migrated required Linux contexts. Claims stay reserved
through reviewed merge; no required check is bypassed.
