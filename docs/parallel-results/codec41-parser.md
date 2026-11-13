# Bounded fixed and count-prefixed repetition

Claim [#186](https://github.com/robot-head/vesper/issues/186) owns only the
executable grammar parser changes, new repetition tests and this report. The
codec child module registration remains the coordinator's; the logical codec
implementation belongs to #187. No Cargo, build, workflow, core or compiler
files were changed.

`Rule::Repeat { rule, count }` executes exactly the configured number of child
rules without count bytes. `Rule::Counted(rule)` reads a canonical unsigned
ULEB128 count and captures that actual number before executing its children.
The parent capture includes the entire prefix/children range; it has no invented
branch or payload. Existing exact frame and outer EOF behavior remains enabled.

`max_repetitions` defaults to 1024 and has an explicit 4096 hard ceiling. Zero
allows only zero counts. Invalid settings and oversized fixed counts refuse
before metadata allocation. Every child reference is validated, including
zero-count and unreachable rules; cycles and full DAG heights still reject.
The caller's depth cap remains additionally limited to 64.

Zero fixed repetition has empty FIRST and is nullable. Positive fixed repetition
inherits its actual child's FIRST/nullability. Counted repetition is nonnullable
and conservatively has all 256 FIRST bytes because its prefix consumes input.
Disjoint, nonnullable choice requirements are unchanged.

Runtime count handling checks usize representability, the configured count cap,
minimum immediate child visits against remaining work/events, and available
capture storage before iteration. Every actual child/descendant still consumes
the same cumulative rule/byte budgets, even when it consumes zero input bytes.
Expanded shared DAG visits cannot reuse a previous visit's allowance. Failure
returns no partial capture vector and mutates neither source nor definitions.

## Actual local validation

The source implementation is commit 1ec2df6. A new private leaf cache
`/tmp/codec41-parser-target` used one job, debug0 and incremental0; no root cache
or other agent's cache was written.

- All 76 tests in 12 Cargo all-target suites passed, zero failures/ignores. This
  includes all 65 existing grammar/framing/Arrow/DER/Protobuf/IPC tests and 11 new
  repetition tests; zero-test example targets are included in the suite count.
- All 4 Rustdoc cases passed, including both compile-fail ownership cases.
- Strict grammar all-target Clippy, workspace formatting and whitespace checks
  passed.
- The actual grammar, Protobuf, DER and IPC examples executed successfully,
  retaining their normal independent vectors, borrowed views and failure checks.

The new tests exercise real count decoding with an independent division/remainder
prefix oracle; exact child ranges and recovered prefix tails; nullable and
zero-count rules; FIRST dispatch; zero-count policy and the 4096 hard boundary;
canonical/overflow/truncated prefixes; upfront count/event/storage/work refusal
before an intentionally mismatching child; nested frame EOF; shared expanded
visits; and reference/cycle/height checks even when repetition is zero.

This parser lane supplies no serializer, checked-code certificate or formal
proof. The codec and its independent roundtrip acceptance remain separate
integration requirements. No source macro, general PEG/recursive grammar, SIMD,
constant-time, source linearity, timing or RSS claim is made.
