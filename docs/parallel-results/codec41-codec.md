# Canonical logical-value codec (#187)

The published codec41 contract preceded writes. This lane owns only
`src/grammar/codec.rs`, `tests/codec.rs` and this report. The parser dependency
`1ec2df6` was copied as `bd2c2d9` for actual compilation; that copy is not owned
codec work and must not be integrated again. Coordinator clarification
`bae6fc4` was read before final resource-profile tests. No API, dependency,
grammar registration, build, workflow or unrelated test changes were made.

The public frozen Value enum contains semantic unit/numeric/borrowed-byte,
sequence, branch, frame and repetition values. It retains neither an encoded
transcript nor source offsets. Equality compares payload contents. The actual
decoder traverses the validated Grammar, uses its FIRST metadata and canonical
number decoder, and confines each frame to its own exact payload endpoint.
Blob slices borrow only their actual input payload. It constructs no Capture
vector: grammar max_events still bounds visits, while the separate codec
metadata cap bounds the allocated value vectors and boxes.

The encoder accepts independently constructed values. It checks rule/variant,
branch identity and sequence/repetition arity while measuring all count and
frame-prefix lengths from those values. A private plan borrows grammar literals
and semantic byte payloads and stores canonical number writes with measured
lengths. Nested frame prefixes are deferred in that plan until their child
payload length is known. The complete plan and scratch buffer are checked
before the final caller copy. All returned failures preserve every caller output
byte; success preserves its unwritten suffix.

Grammar and codec input/frame/count/depth/work/visit bounds remain applicable.
Child depth and mandatory visit counts are checked before vector/box reservation.
Plan growth, copy envelopes, scratch storage/initialization, rendering and final
copy receive finite prepaid metadata/work charges. Values outside the depth
profile remain borrowed; the codec never consumes caller-owned invalid trees.
Fallible vector reservations return AllocationFailed. Box allocations use safe
stable Rust after logical storage admission; no allocator-fault-injection,
physical-OOM recovery, allocator bookkeeping or RSS guarantee is claimed.

## Actual validation

Used the exclusive `/tmp/codec41-codec-target` cache, one Cargo job, debug0 and
incremental0. Final actual results:

- `cargo test --locked -p vesper-grammar --all-targets`: 78 tests across12suites,
  zero failures/ignores. This is65existing tests plus13new codec tests; the parser
  lane's separately owned new repetition tests were not imported into this lane.
- Four existing Rustdoc tests pass, including the nonclone remainder and
  structured Protobuf compile-fail contracts.
- Strict `cargo clippy --locked -p vesper-grammar --all-targets -- -D warnings`,
  workspace formatting and whitespace checks pass.
- Actual existing `grammar`, `protobuf`, `der` and `ipc` examples run and pass
  their self-checks. Grammar reports its real nested decoded42; Protobuf retains
  borrowed/nested/packed/transactional checks; DER matches its OpenSSL fixture;
  IPC checks actual13rows across three batches.

New tests use fixed independent canonical number vectors, including u64::MAX;
borrowed payload pointer/content checks; independently constructed nested frame,
sequence and counted values; exact nested and outer EOF errors; all truncations
and undersized outputs for a tagged frame; real FIRST branch selection and wrong
branch/child/arity failures; nullable fixed/counted repetitions and huge counts;
all zero/above-HARD codec settings; grammar input/frame/visit/work limits; value
storage/depth/work/output/scratch failures; complete output preservation/suffixes.

An independent division/remainder oracle builds nested envelopes bottom-up at
payload lengths around127 and16383. It checks measured prefix-width changes
without using the production framing encoder or deferred-write planner.

## Directional admission and remaining scope

As frozen in `bae6fc4`, max_output_bytes is encoder-only. Decoding obeys the
grammar's input and frame caps and pays different storage/work costs. A genuine
regression decodes128 with a one-byte output allowance and then requires an
OutputLimit refusal from measurement/encoding, with unchanged caller bytes.
Other regressions measure a valid plan and then require scratch metadata/work
refusal from encoding. Measurement is not an approval token or reserved scratch
allocation. Semantic roundtrips/canonical uniqueness concern successful
operations with sufficient compatible directional budgets, not unconditional
same-budget totality or enlarged-input HARD-output encodability.

Independent acceptance and root integration remain separate evidence. This
runtime codec does not implement source @wire_grammar, general PEG/recursive
grammars, SIMD, constant-time parsing, source linearity, cryptographic
authentication or a Rust-refinement proof. No Lean model or proof badge was added.
