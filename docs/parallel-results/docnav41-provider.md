# Checked-source documentation definitions (#190)

The #189–191 contract was published before writes. Owned changes are the new
private navigation child, narrow fallback in `navigation::definition`, twelve new
direct provider tests and this report. No Session/database/sourceDoc/Forge/core,
CLI registration/main/native, dependency/build/workflow or test-wrapper changes.

Existing definition validation, current source/index association, strict UTF16
conversion and ordinary lexical-target priority remain unchanged. Only after an
ordinary target is absent does the fallback inspect the actual reused lexer
tokens. Positions outside real `///` or `//!` tokens return no new target.

The provider builds finite parallel target/name-token-span inventories from
original module declarations and borrowed actual World metadata. Source
functions, ADTs/effects, inherent methods and effect operations match real checked
spans; trait names match checked elaboration metadata. Declaration-name ranges
come from actual identifier lexer tokens inside each declaration's original
span. Inherent method targets include source methods on builtin or otherwise
undeclared namespace owners without inventing a type declaration. Nonlocal,
`Self` and tuple/function-type owner paths are excluded. Generated Pact/model
helpers, trait methods and builtin-only declarations are excluded.

The same retained source and actual finite inventory go through the existing
bounded link parser. A resolved occurrence maps to its unique descriptor/name
span even if another occurrence is rejected. Unknown, ambiguous, unsupported,
escaped/code/Markdown-excluded references return no target. No accepted Document,
layout, typestate graph, proof artifact or cloned World is constructed. The
existing parent returns the original declaration-name range as a same-URI
Location and retains its output cap. Hover and other navigation features are
unchanged.

A separate 1MiB metadata/16Mi logical work stage prepays path storage, geometric
vector growth/copies, AST and declaration-name scans, World lookups and result
matching. Target count stays bounded by existing max_symbols. The reused link
parser retains its own finite declared stage, with no resource retry/reset.
Resource errors use the existing navigation variant and Session mapping.

## Actual validation

With the released private `/tmp/tooling41-lint-target`, one Cargo job, debug0 and
incremental0:

- `cargo check --locked -p vesper --lib` passes.
- All 31 tests in three actual suites pass, zero failures or ignores: twelve
  new doc-navigation tests, fourteen existing navigation tests and five existing
  framed stdio tests.
- New tests verify exact original token ranges against actual AST/World metadata;
  source functions/types/traits/effects/ops/methods and builtin-owner methods;
  owner/namespace identity; genuine type/effect and method/operation ambiguity;
  successful references amid rejected ones; private-helper exclusion; escaped,
  inline/fenced, Markdown and string/comment exclusions; ordinary local shadowing;
  Unicode/CRLF/strict surrogate handling and half-open reference hits; current
  declaration edits and stale index rejection; inherited navigation limits; real
  1,025-reference parser Resource refusal; and the separate inventory work limit.
- The work-limit test uses an actual checked 128-function source with long valid
  names under explicitly allowed HARD source/identifier/snapshot/index limits.
  Its checked snapshot remains valid while navigation refuses before exceeding
  the fixed work stage. The first draft used 256 functions and correctly hit the
  existing 128-function preflight; no production limit was changed.
- `cargo clippy --locked -p vesper --lib --bin vesper --test doc_navigation
  --test lsp_navigation --test lsp_cli -- -D warnings` passes.
- Workspace formatting and whitespace checks pass.

The actual core checker permits an unused `impl Self` header and tuple-owner
methods. They cannot become descriptors in the local link grammar; a regression
ensures their presence does not disable unrelated valid navigation. A real
`Ghost::answer` source method remains navigable while no Ghost type is invented.

These local Linux checks do not claim hosted native-platform results, completion
for unfinished source, current ownership facts, cross-file lookup, a latency/RSS
bound or compiler/proof correctness. Root independently runs the new executable
acceptance and final combined checks. No URI file read, user-program/JIT,
component, macro or Lean execution occurs in the provider.
