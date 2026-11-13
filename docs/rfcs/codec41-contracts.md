# Canonical logical-value codecs for executable grammars (§38)

Claims185–188 precede writers. Base is landed main5d0922e. Fresh open issues and
actual external PR files139–147,149–151,163,165,173 show no grammar overlap.
Claude121–136/main/native/domain paths and Codex Forge/docs/CI claims remain
reserved. This batch is independent of the pending editor stack.

## Ownership

| Claim | Branch | Writes |
| --- | --- | --- |
|185|codec41/integration|this contract, module scaffolding/registration, integration report, review, protected PR|
|186|codec41/parser|grammar.rs repetition rules/metadata/execution/limits, new tests/repetition.rs, parser report|
|187|codec41/codec|new src/grammar/codec.rs, new tests/codec.rs, codec report|
|188|codec41/acceptance|new tests/codec_acceptance.rs, examples/codec.rs, fixtures if needed, README codec section, acceptance report|

Coordinator registers the codec child module before writers, with an empty
development scaffold. Parser preserves that line; codec owns its file. No writer
shares an owned implementation file. Separate worktrees and small private leaf
caches; one Cargo job/debug0/incremental0, root cache coordinator-only. No core,
compiler/Forge/CLI/native, Cargo/dependency, BUILD/workflow or test-wrapper edits.
Existing unrelated tests remain intact. Claims stay reserved through merge.

## Frozen grammar additions

```
Rule::Repeat { rule: usize, count: usize }
Rule::Counted(usize)
Limits::max_repetitions // default1024; maximum4096, zero permits only zero counts
pub const HARD_MAX_REPETITIONS: usize = 4096;
```

Repeat executes exactly count children, without a wire count. Counted reads a
canonical unsigned ULEB128 element count, stores it in Capture.number, then
executes that many children. Counted's own payload/branch fields stay None.
Capture ranges include its prefix and all children. Check representability,
count, visit/event/work budgets before iteration or reservation. Every nullable
child still consumes a visit/event/work unit. Failures publish no partial result.

Repeat count0 has empty FIRST and nullable=true; positive Repeat inherits child
FIRST/nullability. Counted always consumes its prefix: nullable=false and
conservative FIRST=all256. Existing nonnullable disjoint Choice requirements stay
unchanged. Validate every reference/cycle/height, including unreachable and
zero-repeat children. Preserve min(caller max_depth,64) and every existing bound;
reject max_repetitions above4096 before allocation. Do not silently raise a cap.

## Frozen logical codec API

Public child module `vesper_grammar::grammar::codec` can access only its actual
validated parent Grammar's private fields. No unchecked constructor/boolean or
raw-rules bypass API is introduced.

```
pub enum Value<'a> {
    Unit,
    Number(u64),
    Bytes(&'a [u8]),
    Sequence(Vec<Value<'a>>),
    Choice { branch: u16, value: Box<Value<'a>> },
    Frame(Box<Value<'a>>),
    Repeat(Vec<Value<'a>>),
}
pub fn decode_exact<'a>(grammar: &Grammar<'_>, input: &'a [u8], limits: CodecLimits)
    -> Result<Value<'a>>;
pub fn encoded_len(grammar: &Grammar<'_>, value: &Value<'_>, limits: CodecLimits)
    -> Result<usize>;
pub fn encode_into(grammar: &Grammar<'_>, value: &Value<'_>, output: &mut [u8], limits: CodecLimits)
    -> Result<usize>;
```

Derive Debug/PartialEq/Eq for Value. Equality compares semantic byte contents,
not original offsets or encoded bytes. Empty/Byte/Literal use Unit; Uleb128 uses
Number; Blob uses borrowed Bytes; Frame wraps its child; Sequence preserves every
child and arity; Choice retains actual branch ID; both repetition forms use
Repeat values. Values independently constructed by callers must serialize.

CodecLimits fields/defaults/HARD:

|Field|Default|HARD|
|---|---:|---:|
|max_values|4096|16384|
|max_metadata_bytes|512KiB|4MiB|
|max_output_bytes|1MiB|4MiB|
|max_depth|32|64|
|max_work|8Mi|64Mi|

All are positive; invalid codec settings return existing ErrorKind::Rule at0.
Use existing Error/ErrorKind otherwise; no new crate error schema is needed.
Retain applicable grammar input/frame/count/depth/work/visit bounds, including
max_events as a visit bound. max_capture_bytes governs actual Capture storage;
if decoding directly into logical values without Capture allocation, their
separate metadata envelope governs that storage. Explain the distinction.

## Actual algorithms and transactional output

Decode actual input through the validated rules, canonical primitive decoder,
FIRST dispatch and exact nested-frame/outer EOF. It must return logical values,
not a hidden original-byte transcript or guest/caller assertion. Blob values
borrow only the actual validated payload. Shared DAG visits and zero-byte nodes
each cost work/visits; bound value vectors/boxes before allocation. Work is
finite logical accounting, not time/RSS or allocator-failure guarantees.

Encode traverses semantic values and actual rules, validating every shape,
branch identity, sequence/repetition arity and field count. Measure every frame
payload and canonical count from the values, never captured source bytes. The
complete encoded output must fit both codec output cap and grammar input cap,
and every nested frame must fit grammar frame cap. Bounded recursion applies
before borrowing children; caller-owned invalid/deep values are never consumed.

Build a private finite encoding plan, then a bounded scratch output and final
copy. Prepay all value/plan/vector/box/scratch growth, copied bytes, repeat visits,
length/count arithmetic, output work and final copy before allocation/writes.
encoded_len may use the same bounded planner without an output buffer. Every
returned Err leaves ALL caller output bytes unchanged; success changes only its
written prefix and preserves the suffix. Allocation failures handled by fallible
reservations must preserve that invariant. No raw-input echo masquerading as a
serializer, public approval or validation bypass.

## Independent acceptance and claim boundaries

Execute a real tagged batch protocol combining fixed/counted repetition,
Choice, Frame, numeric and blob fields. Construct values independently, encode,
decode and compare, then reencode accepted bytes identically. Use independent
varint/count/length oracles and exhaustive short byte strings, including all
truncations, alternate encodings, trailing/nested-frame bytes, empty/nullable
repeats, shared DAG expansion and every count/depth/storage/work/output boundary.
Check wrong shape/branch/arity and complete output preservation/suffixes.

Keep all existing grammar/framing/Arrow/DER/Protobuf/IPC suites and examples.
Run actual leaf tests, examples and strict all-target Clippy/fmt. Prioritize the
complete runtime codec. A formal model requires a separate frozen scope before
adding it; conditional/definitional echo lemmas are not a Rust proof. This batch
does not claim source @wire_grammar, general PEG/recursive grammars, SIMD,
constant-time parsing, source linearity, cryptographic authenticity, timing/RSS
or Rust-refinement proof. Canonical uniqueness is scoped to one validated grammar
and this exact codec profile, not structural IPC or all wire formats.
