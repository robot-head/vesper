# Review of sections 38–41 and Appendix A

A fresh native Google Docs read returned revision
`AHj4eMRg35hVwN07Ww4ah8QktN80KS_xgnyZypmazCIeUl-0B2N9N-ucp4P_nKYdwQMnGLpzGw3DjIaQE3liPaE18kxN9vW5Y-9PERTVdhw`,
modified `2026-11-12T09:18:30.204Z`. The full snapshot has 41 numbered
sections and Appendix A, with original heading order preserved. The upstream
document was not edited. Metadata schema 2 records the explicit section count
and appendix digest; the checker also understands the historical 37-section
schema 1 inventory.

Compared with the prior snapshot, sections 1–34 and 36 are unchanged. The
section 35 digest changes because its document-order span includes the master
map, now updated to 41 sections. Section 37 loses a trailing decorative glyph.
Neither changes the six active library requirements. The coordinator read all
four new sections and Appendix A; these findings extend the previous
[whole-section review](expansion-37-review.md).

| Addition | Implementation dependencies and acceptance boundary |
| --- | --- |
| §38 Grammar | Canonical codecs need an explicit accepted wire profile and exact-frame policy. Arrow flatbuffers, validity/offset buffers, child arrays, compression and dictionary identity require real validation; descriptors alone are insufficient. The current Beacon adapter exports standard IPC, and the Serde lane implements StreamVByte/tree archives. Neither is a general Grammar compiler. |
| §39 Stylus/Lint | A formatter needs lossless source trivia and exact non-trivia-token/comment conservation. Semantic fixes require actual checked call/effect/escape/field information and a successful recheck of the edited package. These compiler/tooling paths are not assigned to the current host-library batch. |
| §40 Codex | API search needs complete typed signatures, capability identities and field footprints. Typestate reachability is useful documentation analysis, not an all-path exhaustion theorem. Doctest planning is separate from actually checking and executing a single JIT module. Verified badges require audited certificates bound to the current artifact. |
| §41 Metronome | Hardware PMU access needs real platform permission, counter-width/seqlock/scaling and ordering logic. Static cost estimates are not hardware measurements. Paired medians, confidence intervals and causal profiling are separate algorithms and experiments. No throughput or latency claims follow from the illustrative scalar helpers. |
| Appendix A | The text provides a small one-optional-binding calculus and finite modal/projection lemmas. The quoted artifact does not compile unchanged on pinned Lean 4.24; repairs and an axiom audit are tracked in [#54](https://github.com/robot-head/vesper/issues/54). Its scope must remain visible. |

## Concrete scaffold corrections

- §38's `rows * 8` must be checked, negative validity ranges rejected before
  conversion and `u64 -> usize` representability checked on 32-bit targets.
  Nullable sums must use and validate the actual bitmap and null count. The
  64-byte alignment profile is stricter than general Arrow IPC interoperability;
  relative buffer offsets also do not establish absolute DMA alignment.
- Prefix parsing does not establish byte uniqueness for a whole authenticated
  frame unless the remainder is included in the value or exact EOF is enforced.
  Standard Protobuf permits multiple equivalent wire forms, so a canonical
  profile needs explicit field ordering, duplicate/unknown-field and encoding
  rules. Single-byte FIRST-set disjointness is not a complete LL(k)/PEG analysis.
- §39's repeated recursive `flat_width` can take quadratic work on nested groups.
  A line-comment node followed by text needs a lexical line boundary; equality of
  abstract comment nodes alone does not prove the next token was not commented
  out. Byte length and visual columns also need a stated width convention.
- An allocation not escaping the return value does not prove it stays local:
  it may escape through a call, field, callback or captured state. Likewise,
  declared/performed effect-name sets do not establish transitive authority or
  cancellation safety. Missing compiler evidence must prevent an autofix.
- §40's sample Hoogle entry contains modes but no full parameter/return types;
  mode matches alone can return a different API type. `has_lean_proof_cert: bool`
  cannot authorize a proof badge. Validate state/edge identities, visibility and
  all supported transitions before interpreting a typestate graph.
- §41's sample cycle subtraction may overflow signed i64; saturating the scaled
  difference distorts ratios. Reject invalid/nonpositive counters and empty
  observations rather than treating missing evidence as a passing regression.
  A sorted median is not the stated Mann–Whitney/Hodges–Lehmann confidence test.
  A fixed 48-bit mask is a selected counter model, not a universal PMU width.
- The Appendix A probe `/tmp/vesper-spec41-appendix-lean.log` reports, among
  other issues, application of the proposition `List.mem_cons_self` as a function,
  unresolved `List.res` field notation and dependent-environment proof errors.
  Even after elaboration repairs, `tLet` requires `gamma2 = none`, and the final
  result is equality of minted/consumed list lengths conditional on an evaluation
  derivation. It does not formalize all surface constructs, general live frames,
  production PCG checking or compiler correctness.

No new performance/security/compiler theorem is inferred from these examples.
The [issue-backed contracts](continuation37-contracts.md) identify the current
implementation owners; other new sections remain available for separately
claimed work after checking the issue board.
