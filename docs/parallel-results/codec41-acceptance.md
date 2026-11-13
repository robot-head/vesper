# Independent canonical codec acceptance (#188)

The published frozen contract `docs/rfcs/codec41-contracts.md` and issue188 were
read before writes. Owned files are new `tests/codec_acceptance.rs`, executable
`examples/codec.rs`, the README codec profile/necessary old-profile corrections,
and this report. No production, dependency, build, workflow, wrapper or compiler
file is changed by this lane. Parser/codec dependency copies are excluded from
owned work.

The acceptance corpus independently constructs a tagged batch format with two
fixed magic bytes, a canonical exact frame and counted numeric/blob records.
Expected bytes come from division/remainder varint, count and frame-length
oracles which never call production codec/framing helpers. Semantic values are
constructed separately, including changed values that must produce new bytes;
this prevents retained-input echo from masquerading as serialization.

Tests cover actual capture-parser agreement and count/range fields, borrowed
payload identity with content-based semantic equality, canonical prefix boundary
lengths and2048generated numbers, all zero/one/two-byte numeric and counted streams,
overlong count/number/blob/frame encodings, huge/overflowing counts, outer and
nested EOF, nullable zero-byte repeats, shared DAG expansion, deep frame trees,
invalid shapes/branch identities/arities, codec settings/limits and applicable
grammar limits. All encoder errors retain complete caller output; every successful
helper checks both prefix/suffix regions outside the written range.

Actual validation used parser source1ec2df6 and codec sourceda7dc01 supplied by
the coordinator. Their dependency-only copies in this worktree are5c17d3e and
3e6e425; they are not acceptance-owned changes and must not be reimported as
owned work. Initial acceptance-owned commit is9eb138e.

With exclusive `/tmp/codec41-acceptance-target`, one Cargo job, debug0 and
incremental0:

- `cargo test --locked -p vesper-grammar --all-targets` passes78tests across
  13suites, zero failures/ignores. This includes all13independent acceptance tests
  and65existing leaf tests. It does not include other lanes' not-yet-imported
  new parser/codec unit-test files.
- The zero/one/two-byte numeric corpus accepts exactly16,384canonical inputs;
  the independent counted-number corpus accepts129. Every accepted input
  produces the independently expected semantic value and identical reencoding.
- All2048generated u64batch values and payload/count/frame boundary cases pass,
  as do actual nullable/DAG/depth/count/resource and complete output-preservation
  failures. Borrowed blob views point into the validated actual payload.
- `cargo run --locked -p vesper-grammar --example codec` passes all assertions and
  prints `Canonical batch: 13 bytes, two logical records; changed value300 -> ac02`.
  The changed-value serialization is independent of any decoded original bytes.
- `cargo clippy --locked -p vesper-grammar --all-targets -- -D warnings`,
  standalone Rustfmt and `git diff --check` pass. The environment's existing Rust
  toolchain is used; no dependency, toolchain or build file was added.

These are actual Linux runtime results for those dependency heads, not a claim
that a development namespace implements the feature or that future source
follow-ups have already passed. Root performs final integrated validation after
merging the owned lanes and any review fixes.

No Lean model or conditional/echo theorem was added. Source macros, recursive PEG,
SIMD, constant-time/cryptographic/source-linearity and Rust-refinement proof,
timing/RSS and other-platform execution remain outside this lane's evidence.
