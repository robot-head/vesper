# Canonical logical-value grammar codecs (§38)

Claims185–188 and `docs/rfcs/codec41-contracts.md` preceded source writers.
Fresh open claims and actual external PR filenames showed no grammar overlap;
Claude's domain/main/native lanes and the separate Codex editor stack remain
reserved. Three exclusive worktrees implemented repetition, codecs and independent
acceptance. This PR is based on main5d0922e and changes no Cargo/dependency,
compiler/Forge/core/native, BUILD/workflow or test-wrapper files.

The existing validated byte-FIRST grammar now executes fixed and canonical
ULEB128-counted repetitions, including nullable children. Zero-count fixed
repetition has empty FIRST/nullable=true; counted repetition consumes its prefix
and has conservative FIRST=all256. Every reference/cycle/height is validated,
even through zero-count or unreachable rules. Actual count, minimum child visits,
capture capacity and work are checked before iteration. Existing disjoint,
nonnullable Choice requirements and the depth64 ceiling remain enabled.

`grammar::codec` decodes actual bytes into semantic values: numbers, borrowed
blob slices, ordered sequences, explicit choice IDs, frames and repetitions.
Values contain no hidden encoded-input transcript. Callers construct values
independently and encode them against the actual validated Grammar. Encoding
checks shape/branch/arity/counts, measures canonical prefixes from semantic values,
creates a bounded private write plan and scratch buffer, then copies once into
caller output. Every returned Err preserves the complete caller buffer; success
preserves its suffix. Nested frame lengths include inner prefix widths and every
decoder frame requires exact EOF.

Independent review found no remaining actionable defect in the final frozen
profile. Follow-up tests cover depth checks before container reservation and
independent nested-frame prefix-width transitions at127/16383. No public raw-rule
bypass, approval boolean or proof certificate is introduced.

## Actual validation

On Rust1.99.1, final combined leaf validation passes102tests across15all-target
suites, zero failures/ignores, plus all four Rustdoc cases. This includes all
65existing framing/grammar/Arrow/DER/Protobuf/IPC tests,11new repetition tests,
13codec tests and13independent acceptance tests. Strict all-target leaf Clippy,
workspace formatting and whitespace checks pass.

All five real executable examples pass. The new example constructs a tagged
framed batch independently, produces13canonical bytes, decodes two logical
records with a borrowed blob, and separately serializes changed Number300 to
`ac02`. Existing framing/Arrow, DER fixture/SET OF, IPC and Protobuf examples
also execute successfully.

Independent short-input oracles enumerate all zero/one/two-byte inputs, accepting
16,384numeric and129counted streams and requiring semantic agreement plus byte-
identical reencoding for each accepted corpus value. Another2,048generated u64
values and a composed tagged/frame/count protocol use separate division/remainder
varint and framing oracles. Tests cover every truncation, alternate count/number/
length forms, trailing and nested-frame bytes, borrowed identity, changed values,
wrong shapes/branches/arities, nullable/shared DAGs, huge counts and resource
failures, with complete output/suffix assertions.

The independent lane reran its13tests and example on the final reviewed source;
its report distinguishes source dependencies from owned changes. No substitute
parser or fabricated checked/proof artifact is used.

## Resource and scope boundaries

Codec values, plan/scratch metadata, output, depth and logical work have explicit
default/HARD caps, in addition to applicable actual grammar limits. Direct value
decoding allocates no Capture vector: old capture-storage limits govern actual
captures, while semantic vectors/boxes use their separate declared envelope.
Borrowed payloads do not imply allocation-free metadata or encoding.

`max_output_bytes` is encoder-only. Decode retains the grammar's input/frame
caps; borrowed decoding and plan/scratch/copy encoding have different costs.
Roundtrip semantics/byte uniqueness concern successful operations with sufficient
compatible directional budgets for one validated grammar. They are not promises
of unconditional same-budget totality, or that every enlarged-grammar input fits
the codec's HARD output cap. A genuine directional refusal test preserves output.

This completes a runtime codec slice, not source `@wire_grammar`, general PEG or
recursive grammars, FOLLOW-dependent nullable choices, SIMD/constant-time parsing,
source linearity, cryptographic authenticity or Rust refinement of a formal model.
No formal theorem, performance or RSS claim is supplied. Hosted current-head
checks are pending publication; local evidence does not claim other-platform or
RBE execution. Claims stay reserved through reviewed merge.
