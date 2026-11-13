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

Status before implementation dependencies are imported: Rustfmt and diff checks
pass. Actual compile/runtime acceptance is pending the real parser and codec
implementations, whose owned commits are supplied by the coordinator. No stub or
fake checker is used, and no runtime success is claimed from the development
namespace alone. This report will be supplemented after actual execution.

No Lean model or conditional/echo theorem was added. Source macros, recursive PEG,
SIMD, constant-time/cryptographic/source-linearity and Rust-refinement proof,
timing/RSS and other-platform execution remain outside this lane's evidence.
