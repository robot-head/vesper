# Finite binary64 JSON independent acceptance (#203)

The published `docs/rfcs/float41-contracts.md` contract and ownership issue #203
preceded writes. This lane owns only the new roundtrip test, finite example,
Python harness, narrow README profile and this report. No codec source,
dependency, Cargo, BUILD, workflow or wrapper changes are owned here.

Dependency-only cherry-picks are decoder `fa4e02d` → `5065cac` and encoder
`651badf` → `620f55e`; those copies are not acceptance work.

Six independent Rust tests exercise actual `Value::F64`, `encode_into`,
`IndexedJson`/Node conversion and transactional Cursor reads:

- 8,209 encode/decode cases: 21 explicit IEEE boundary cases and 8,188 finite
  values from 8,192 deterministic arbitrary bit-pattern steps. Both Node and
  Cursor conversion must preserve exact bits, including negative zero. The
  successful encoded prefix must fit the declared 512-byte finite ceiling and
  leave every suffix byte unchanged.
- Known decimal rounding ties, integer precision boundaries, normal/subnormal
  boundaries, maximum finite values, and correctly signed underflow. Integer
  APIs retain exact u64/i64 contracts independently of f64 rounding.
- Type, numeric overflow, finite byte/work limits and invalid zero/above-HARD
  settings retain exact original token offsets and unchanged cursor position;
  successful retries and End remain transactional.
- A valid 5,001-digit raw number remains byte-exactly forwardable while default
  f64 limits refuse it and larger valid conversion limits reject its Infinity
  result. A separate valid number crosses the declared quadratic work budget.
- Both Infinities and positive/negative quiet/signaling NaN representatives,
  including minimum/maximum payload forms, fail during nested complete preflight.
  All output bytes remain sentinel values on these late failures and on work,
  output/capacity, duplicate-key, token/depth and string-limit errors.
- Mixed nested raw Unicode, finite floats and exact integer values retain their
  independent contracts and output suffixes.

`finite_float_json` emits one small actual encoded object per operation, keeping
the encoder's unchanged default work allowance. Each record contains an exact
bit string and an actual JSON numeric value, checked against the real decoder
before writing. Its corpus has 24 explicit edges plus 4,096 deterministic steps;
nonfinite generated patterns are excluded. No large float array bypass or raised
default is used.

The independent Python consumer launches this executable twice with finite
process/output allowances. It requires deterministic bytes, exact record count
and order, unique keys, JSON numeric values rather than strings, and finite
results. Both `parse_int=float` and `parse_float=float` preserve valid integer
spellings such as `-0`; `parse_constant` refuses non-JSON constants. Actual
`struct.pack` binary64 bits must match each emitted bit string.

Actual Linux validation using the lane's exclusive
`/tmp/float41-acceptance-target` cache, one Cargo job, debug0/incremental0:

- All crate targets pass 48 tests across seven suites, zero failures or ignores:
  six new acceptance tests and 42 existing JSON/binary tests. The final expanded
  cursor-limit cases separately rerun all six successfully.
- Both README Rust doctests pass.
- Strict crate all-target Clippy with `-D warnings`, workspace formatting,
  Python syntax and whitespace checks pass.
- The actual example and Python interoperability pass twice: 4,118 finite values
  and 825,962 encoded bytes per process, including negative zero, extreme finite
  values, subnormals and the deterministic generated corpus.

No corpus assertion, error expectation, production limit or default needed to
change after actual execution. Tests and the example use real codec APIs without
serde_json or new dependencies. These are finite scalar host results, without
Eisel-Lemire/Dragonbox/SIMD, source derive/Region typing, linear construction,
DetFloat arithmetic/backend verification, whole-process allocation, throughput,
RSS, formal proof, BuildBuddy or other-platform execution claims. Hosted evidence
is separate from this local validation.
