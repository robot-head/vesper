# Finite binary64 JSON encoding (#202)

The issue and published `float41-contracts.md` preceded source writes. Owned
changes are limited to `vesper-serde/src/{encode,helpers}.rs`, the new
`tests/float_encode.rs` and this report. No dependencies, Cargo/build/CI, shared
float/index/lib modules or existing unrelated tests were changed by this lane.

`Value::F64` accepts finite binary64 values, including both signed zeros, through
the same complete recursive preflight as every existing encoder value. All NaN
sign/payload classes and both infinities reject with Number before caller output
is touched. Nested arrays/objects retain duplicate-key, string, raw-node, depth,
token, work, output-size and capacity guards. Validated raw enormous JSON numbers
remain raw-forwardable; they are not implicitly converted to binary64.

Finite formatting uses `core::fmt::Write` and `format_args!` with a fixed512byte
stack buffer. The new path has no heap String/Vec, `format!`, `to_string` or
formatting dependency. Standard scalar formatting preserves negative zero and
roundtrips finite binary64 bits; it does not define a unique decimal spelling or
arbitrary-precision mathematical equality. `JsonSizeBound::F64` is Bounded(512),
not permission to serialize a nonfinite value.

Before the first formatter call, preflight charges two declared conservative
logical formatting allowances, each512²+512 =262656units. The existing final
encoded-size charge covers output writes. This is an admission policy, not a
CPU-instruction/time claim. The second deterministic formatting pass sees the
same immutable finite bits and fixed sink; its already-established invariant is
infallible at the Writer API boundary. No returned error occurs after caller
output begins. Successful encoding changes only its exact measured prefix.

With unchanged defaults, standalone arrays of31finite floats fit the formatting
allowance; an array of32zeros refuses WorkLimit. For zero arrays, total work is
525315N+2units including array/value tokens and encoded output. Added tests also
admit31negative minimum-subnormal values with their longer actual text. Mixed
values may consume additional existing work and other caps independently; no
default was enlarged to conceal this conservative bound.

Validation in exclusive `/tmp/float41-encode-target`, one Cargo job,
debug0/incremental0:

- All52tests across four nonempty suites pass: eight new encoder tests,
  25 existing JSON tests,17existing binary/archive tests and two doctests;
  zero failures or ignores.
- New actual encoder→IndexedJson→as_f64 tests cover every finite exponent class
  with multiple signs/fractions, over4000deterministic arbitrary finite bit
  patterns, boundary neighbors, signed zero, subnormals and maximum finite values.
  More than10000scalar roundtrip cases check exact bits, grammar acceptance,
  measured size, repeat determinism and suffix preservation.
- Nonfinite sign/payload classes fail in scalar and deeply nested positions,
  including undersized outputs; all caller bytes remain unchanged. Tests also
  cover both formatting-work passes, exact output/work boundaries, mixed raw
  Unicode/integer extremes, old failure categories and the finite size ceiling.
- `cargo clippy -p vesper-serde --locked --all-targets -- -D warnings`, package
  formatting and whitespace checks pass.

Actual decoder dependency `fa4e02d` was imported as dependency-only cherry-pick
`a509a6c` so the new tests exercise the real `Node::as_f64` and strict checks use
the real float preflight. It is not encoder-owned work and must not be imported
again as an encoder change. Root owns combined decoder/independent Python
interoperability validation and hosted-platform evidence. No source derives,
region typing, SIMD, allocation-failure/RSS/timing or formal refinement proof is
claimed. The claim remains reserved through reviewed merge.
