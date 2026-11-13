# Bounded finite binary64 JSON (§20)

Claims200–203 precede writers. Base main5d0922e. Fresh issues/actual external PR
filenames show no vesper-serde/CAS overlap. Claude121–136/main/native and193Bazel
work, plus own pending grammar/editor/CI paths, stay reserved.

|Claim|Branch|Exclusive writes|
|---|---|---|
|200|float41/integration|new float.rs limits/preflight, lib.rs narrow module/reexport, contract/integration report/review|
|201|float41/decode|index.rs narrow Node/Cursor additions, tests/float_decode.rs, report|
|202|float41/encode|encode.rs/helpers.rs narrow Float/size/stack formatter additions, tests/float_encode.rs, report|
|203|float41/acceptance|tests/float_roundtrip.rs, examples/finite_float_json.rs, scripts/check-float-json.py, README float profile/report|

Separate worktrees/small private caches, one Cargo job/debug0/incremental0. Root
cache coordinator-only. No Cargo/dependency/core/Forge/Strata/main/native/BUILD/
workflow/wrapper edits or unrelated existing test rewrites. Freeze amendments
before dependent changes; claims remain reserved through reviewed merge.

## Frozen APIs and numeric policy

Coordinator registers private float module and public FloatLimits:

```
FloatLimits { pub max_number_bytes: usize, pub max_work: usize }
Default = {4096,32Mi}; HARD = {65536,128Mi}
FloatLimits::validate(self) -> Result<(),Error>
// crate-private preflight(bytes:usize) -> Result<(),Error>
Node::as_f64(self, limits: FloatLimits) -> Result<f64,Error>
Cursor::read_f64(&mut self, limits: FloatLimits) -> Result<f64,Error>
Value::F64(f64)
JsonSizeBound::F64 = JsonSizeBound::Bounded(512) // associated constant
```

Reject zero/above-HARD number settings with InputLimit, invalid work settings
with WorkLimit; checked arithmetic failures Overflow. Before standard scalar
parsing, prepay checked `number_bytes² +512` logical work and max_number_bytes.
These are explicit logical admission units, not CPU instruction/time proofs.
Type failures retain Type at original token start; empty cursors retain End;
number conversion errors retain the actual original Number token offset.

The existing IndexedJson still validates ALL syntax/UTF8/Unicode/duplicate keys
and nested unknown fields before publishing Nodes. Conversion only accepts a
Number node. Use Rust standard scalar binary64 conversion; finite IEEE rounding,
subnormals and underflow to correctly signed zero are supported. Overflow to
Infinity and every nonfinite result reject existing Number. Cursor advances only
after successful conversion; every error leaves its position unchanged.

Existing as_u64/as_i64 and raw numbers stay unchanged. A lexically valid enormous
number can remain raw-forwardable while f64 conversion refuses it. No claim of
arbitrary-precision numerical equality, decimal canonicalization or JSON byte
uniqueness. Finite encode->as_f64 roundtrips preserve exact binary64 bits,
including negative zero; decimal input conversion may round by IEEE rules.

## Allocation-free encoding and complete failure atomicity

Value::F64 accepts finite values only. Reject every sign/payload NaN and both
Infinities in complete recursive preflight before any caller byte is written.
Use fixed512byte stack formatting via core::fmt::Write/format_args!, not
format!, to_string, heap strings or a new formatting dependency. Preserve -0.
Coordinator does not own encoder/helper logic; writer202 implements it there.

Prepay deterministic work for BOTH formatting passes plus existing output writes;
use a declared conservative constant per bounded binary64 formatting pass, not
elapsed-time guesses. The existing exact encoded_size/output capacity checks,
duplicate keys, strings/raw-node nested depth/tokens and every work/output cap
remain enabled. No new larger default or bypass. A second deterministic stack
format pass writes only after complete preflight. Every returned Err preserves
ALL output; successful writes preserve suffix. JsonSizeBound::F64 is a finite
host value ceiling, not permission to encode NaN/Infinity.

The JSON parser retains its bounded token Vec; only encoding/formatting claims
no new dynamic allocation. Rust standard conversion/formatting is not an
Eisel-Lemire/Dragonbox/SIMD implementation, source derive/Region typing,
linear-resource construction, DetFloat arithmetic/backend proof or speed/RSS
guarantee. No generated Lean or proof claim.

## Independent actual acceptance

Real parsed numbers, exact offsets and transactional cursor failures; positive/
negative zero, minimum/largest subnormals, normal boundary, maximum finite,
rounding/exponents, huge valid raw numbers with bounded refusal, every nonfinite
encoding pattern class, and complete nested output/limit/suffix preservation.
Deterministic arbitrary finite bit patterns encode then decode bit-exactly.
Exercise mixed nested/raw Unicode objects and unchanged integer APIs.

An actual executable example emits bit strings alongside actual encoded numeric
values. Independent Python json uses parse_int=float/parse_float=float and a
rejecting parse_constant, then struct.pack to compare bits. Default Python integer
handling would lose the sign of valid JSON -0; handle that explicitly, never
weaken the encoder/corpus assertion. No serde_json dependency is added. Bound
example corpus/output/process lifetime. All old serde suites, actual example,
strict all-target Clippy/fmt and real Python interoperability are required before
claiming completion; hosted platform evidence remains separate.
