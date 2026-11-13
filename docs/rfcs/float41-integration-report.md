# Finite binary64 JSON integration (#200–203)

The public claims and frozen contract preceded the three exclusive writers.
Shared FloatLimits admission was published before Node/Cursor and encoder work;
independent acceptance used the frozen interfaces. Integration imported landed
owner main e2b8bba (including Resonance and SimSoftNIC) without source conflicts.
No Cargo, dependencies, BUILD, workflow, compiler, native or Forge changes are
introduced by this feature.

Validated JSON Number nodes now convert to finite binary64 after checked lexical
and quadratic logical-work admission. Number errors retain the original token
offset, and Cursor commits only after success. Existing integer conversion and
raw-number forwarding remain available. IEEE rounding, subnormals and correctly
signed zero underflow are supported; overflow to Infinity rejects.

Value::F64 uses a fixed 512-byte stack formatter and rejects every NaN/Infinity
during complete recursive preflight. Both deterministic formatting passes and
output writes are prepaid. All existing raw/string/duplicate/depth/token/output
guards remain; every returned error preserves the whole output, and success
preserves its suffix. The conservative formatting allowance is 262,656 logical
units per pass, so unchanged default work admits 31-element scalar float arrays
and refuses 32. JsonSizeBound::F64 is the finite host ceiling Bounded(512).

Actual coordinator Linux validation on the complete integrated feature:

- `cargo test -p vesper-serde --all-targets`: 67 tests across nine targets, zero
  failures or ignores. Two README doctests also pass.
- Strict all-target crate Clippy, workspace formatting and whitespace checks
  pass. The real finite_float_json example builds and self-checks its output.
- `python3 scripts/check-float-json.py --binary .../finite_float_json` launches
  two processes; each produces 4,118 finite values and 825,962 bytes. Independent
  Python JSON parsing and struct packing match exact emitted binary64 bits,
  including negative zero. Output is deterministic and bounded.
- Independently authored Rust acceptance covers 8,209 finite roundtrip cases,
  explicit IEEE boundaries/ties, nested Unicode/raw values, actual original
  offsets, retryable cursors, every nonfinite class, huge raw-number forwarding,
  and full output/suffix preservation under errors and limits.
- A separate read-only reviewer found no actionable source issue. Its private
  host probe covered all 2,048 exponent fields with five fraction edges and both
  signs plus 20,000 deterministic random patterns; a counting allocator observed
  zero encoding allocations. This is sampled host evidence, not a universal
  formal proof or a claim about parser allocation.

Owned leaf commits: fa4e02d/203ac9d/37d8d38 (decode), 651badf/c6cffef
(encode), fcd41ef (independent acceptance). Dependency-only copies are not
separate authored work. Coordinator logs are /tmp/float41-root-{final,doc,
clippy,example,python}.log; the combined final exit marker is zero. The shared
root cache was not used for this final run.

These APIs are a scalar host prerequisite from §20. They do not implement the
document's Eisel-Lemire/Dragonbox algorithms, SIMD/source derives/Region typing,
linear-resource decoding, DetFloat backend proofs or speed/RSS guarantees.
Logical admission is not an elapsed-time proof, decimal input may round, and
JSON decimal bytes are not canonicalized. Hosted checks and protected merge
remain separate from the local results recorded here.
