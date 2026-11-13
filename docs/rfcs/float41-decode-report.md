# Finite binary64 JSON decoding (#201)

Read the published #200–203 contract before source writes. This lane changes
only the narrow `index.rs` Node/Cursor additions, new `tests/float_decode.rs` and
this report. The shared FloatLimits/preflight module is coordinator-owned base
`e30008d`, not decoder-owned work. No encoder/helper, library registration,
README, dependency, Cargo/BUILD/workflow, core or Forge files changed.

`Node::as_f64(FloatLimits)` accepts only an already validated Number node. It
checks the shared byte and checked quadratic logical-work admission before Rust's
scalar binary64 conversion, relocates every conversion/admission error to the
original number token's UTF8 byte offset, and rejects nonfinite results with the
existing Number error. Finite IEEE rounding, subnormals and signed-zero underflow
are retained. The raw spelling remains borrowed and unchanged.

`Cursor::read_f64` advances to the actual token's precomputed subtree successor
only after success. Type, End, limit/configuration, conversion and overflow-range
failures preserve the exact cursor position; the same value can still be read raw
or retried under an explicitly chosen admission. Existing integer, string,
whole-document validation and raw-forwarding behavior remain unchanged. This
does not publish views of partially validated JSON or construct linear resources.

## Actual validation

In the exclusive `/tmp/float41-decode-target`, one Cargo job, debug0 and
incremental0:

- `cargo test --locked -p vesper-serde --all-targets` passes 53 tests: seventeen
  existing binary/archive tests, twenty-five existing JSON tests and eleven new
  decoder tests. Zero failures or ignored tests. Both existing example targets
  compile in this run.
- New tests use fixed expected binary64 bit patterns for both zeros, ordinary
  fractions/exponents, smallest and largest subnormals, minimum normal and maximum
  finite values. Exact halfway decimal cases verify ties-to-even; large integers
  verify finite rounding; extreme negative exponents verify signed-zero underflow.
- Positive overflow and both signs of infinity-producing decimal conversion
  reject at exact original offsets inside Unicode-keyed nested arrays. Every
  tested failed cursor read preserves its original position and raw value.
- Type/End precedence, exact byte/work boundaries, zero/above-HARD settings and
  a real 8,192-byte decimal's independent byte/work refusals and successful
  signed-zero retry are covered.
- A valid 5,001-digit JSON integer remains raw-forwardable with exact bytes and
  untouched output suffix while default float admission refuses it and larger
  admission reports its nonfinite range. Integer APIs and subsequent borrowed
  Unicode strings retain their previous behavior.
- Malformed unknown subtrees, decoded duplicate keys, invalid surrogates and
  non-JSON numeric spellings still fail whole-document parsing before any Node.
- `cargo clippy --locked -p vesper-serde --all-targets -- -D warnings` passes.
- `cargo test --locked -p vesper-serde --doc` passes both executable doctests.
- `cargo fmt --all -- --check` and `git diff --check` pass.

This lane uses Rust standard scalar conversion; it does not implement or claim
Eisel-Lemire, SIMD, source Region typing, native/backend DetFloat behavior,
zero-total-heap parsing, instruction/time/RSS bounds or proof certificates. These
are local Linux results. The separate encoder and independent executable/Python
acceptance lanes, root combined checks and hosted platform gates remain separate
evidence.
