# Unit and tuple expression macro acceptance

Claim [#170](https://github.com/robot-head/vesper/issues/170) owns only the new
Rust executable test, tuple fixtures, Python harness and this report. The frozen
contract is `docs/rfcs/tuple41-contracts.md`. External #158 retains every workflow,
Bazel/build file and `scripts/test-*.sh`; none is edited here.

Six independent Rust tests exercise the actual source compiler and explicit
stdio preview. Twenty positive fixtures cover unit, grouping, singleton,
multiple and nested tuples; tuple-valued input slots; plain function parameters;
nine-element arity independent of macro parameter count; both capture directions;
parameter/global collision; initializer shadowing; tuple-element block isolation;
real ordered inout state updates; UTF8 comments/string punctuation; repeated
source anchors and actual linear product consumption. Every positive artifact
must expand byte-for-byte from the accepted report, pass ordinary source
checking and separately execute to42. These runs are explicit normal CLI actions.

Negative cases require actual linear product duplication/leak errors and field
type errors from the ordinary checker. Both singleton/group mismatches are
checked, preventing a singleton from silently becoming grouping or vice versa.
Malformed commas/delimiters, unchanged call/invocation comma rules, deep tuples
and large metadata/map inputs cannot publish partial expanded source. Actual
BLAKE3 hashes, source/expanded UTF8 ranges and distinct repeated argument anchors
are validated independently from output bytes.

Two different generated helpers occur at different nested tuple positions. Each
helper's unchanged signature with a newly fallible body must independently fail
the real authority gate under empty grants. A separate caller-owned helper call
inside an input slot retains its source origin and real declared Fail row; it
may compile but cannot execute as an unhandled entry. This distinction must not
be erased by tuple traversal or guest/cached approval.

The Python harness repeats source compilation and actual current-unsaved stdio
checks, compares all accepted artifacts/maps/audits with separate Forge results,
and leaves a deliberately different on-disk program untouched. Same-text version
updates, stale expected versions, changed helper authority and real linear/map
refusals retain current source association. Original macro source still publishes
parse errors and hover remains unavailable after a successful preview. Formatting
must conserve ordered comments/nontrivia tokens, reparse and remain byte-idempotent
for ordinary nested, repeated and punctuation-rich tuple fixtures.

Actual validation against the integrated tuple-aware executable at
`/workspace/vesper/target/parallel-next/debug/vesper` passed:

- Six independent Rust tests, zero failures and zero ignored tests. All twenty
  positive artifacts separately passed ordinary checking and executed to42.
- The Python harness passed157commands,21stdio sessions and108JSON-RPC requests,
  including23explicit normal expanded-source runs to42 and one intentional
  unhandled-entry refusal. All accepted previews matched separately compiled
  actual Forge artifacts, maps and audits.
- Generated-helper body drift was rejected independently for both nested calls;
  the caller-origin effect case remained accepted with its real declared row.
- Standalone Rust compilation and strict Clippy using existing read-only
  serde_json/blake3 artifacts; rustfmt, Python compilation and whitespace checks.

The first executable Python pass reached its formatter checks and truthfully
refused the full repeated fixture with WorkLimit. The existing formatter syntax
gate prepays a source-dependent stage of tokens²*4+bytes*16+64KiB, capped16Mi,
which is distinct from the compiler profile. Three compact actual tuple programs
now exercise successful formatting, ordered comments/nontrivia conservation,
reparse/idempotence and separate normal execution under that unchanged allowance.
The larger support-function fixture remains a conservative formatting refusal;
no formatter cap or production code was changed to conceal that limitation.

Before integration, six independent ordinary nonmacro programs also checked and
ran to42, verifying the fixtures' underlying unit/singleton/nested, inout order,
string punctuation and linear-product semantics. These were ordinary language
baselines, not evidence of macro expansion; the actual macro results above were
run against the real integrated compiler afterward.

No compiler dependencies or heavy toolchain were rebuilt by this lane. Native
Linux/macOS/Windows coverage is coordinated with #158 and remains separate pending
evidence; local Linux results do not claim other-platform execution. The Rust
target accepts `VESPER_SOURCE_MACRO_TUPLES_BINARY`; the Python script takes
`--binary REALPATH` for actual Bazel/Cargo-produced executables. Root runs the
integrated Cargo and workspace checks separately.
