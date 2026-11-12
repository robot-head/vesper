# Canonical protocols, PMU windows and lint policy (#77)

This batch follows main `965b600408d148b01559624914097477da6691ca` (PR #71).
The coordinator checked the current issue board and actual file lists of every
open external Warp PR before claiming #72–#77. External Warp #36–#45 remains
separately owned. The Google document still has the recorded §§1–41/Appendix A
revision, modified 2026-11-12T09:18:30.204Z; its metadata was checked again during
integration. Ownership and shared interfaces are in
[`protocol41-contracts.md`](../rfcs/protocol41-contracts.md).

## Real binary formats

`vesper grammar check FORMAT FILE` validates actual binary bytes. Protobuf
requires `--schema FILE`; DER optionally takes `--set-of`; Arrow IPC optionally
takes `--simd-alignment`. `--json` produces one structured success/error report.
The command only reads bounded regular files, rejects final symlinks, validates
opened handles and limits the actual bytes read. Input is capped at 8 MiB,
schema at 64 KiB and serialized output at 1 MiB; stricter codec budgets also
apply. These are logical limits, not hard deadlines or allocator/RSS guarantees.

The Protobuf schema is flat JSON with `profile`, `root` and `messages`; each
message contains typed numbered fields. A message field references another
message index, and the validated graph must be acyclic. The existing indexed
JSON parser rejects duplicate **decoded** keys and bounds all subtrees before
typed Serde deserialization rejects unknown fields. Present null indices,
inactive scalar indices, illegal field numbers, cycles and malformed schemas
cannot silently disappear during parsing.

`vesper.protobuf.explicit-presence/1` preserves singular presence/defaults and
ordered repeated values. Numeric repeats use one nonempty packed segment.
Unknown fields, duplicate singulars, alternate numeric encodings and unsorted
field numbers fail. This is an explicit interoperable Protobuf profile, not a
claim that the Protobuf standard defines universal canonical serialization.

DER supports bounded universal primitive encodings, sequences and explicitly
typed SET OF. ASN.1 SET, arbitrary ASN.1 schemas and unsupported string/time/
context tags are rejected. The default CLI schema is Any; `--set-of` selects
only a root SET OF supported values. Library callers can provide richer typed
schemas. Canonical tag/length/primitive rules and complete child-TLV ordering
are validated. OpenSSL-produced fixture bytes independently exercise the codec.

For both Protobuf and DER, the CLI parses, invokes the actual canonical encoder
into a private bounded buffer, then compares the entire result to the original
bytes before reporting `roundtrip_bytes_equal`. This is a runtime check under
the codec budgets, not a proof certificate or a generic source-language theorem.
Protobuf exact parsing consumes one full message interpretation; the unframed
format does not inherently distinguish concatenation of optional-field messages
from another valid message. Framing remains a separate API.

Arrow IPC validation reads actual stream framing and FlatBuffer Schema and
RecordBatch metadata, then validates borrowed signed-i64 bodies and validity
bitmaps. Independent PyArrow 20.0.0 fixtures cover V4/V5, legacy/continuation
framing, nullability, empty/all-null columns and unsupported real stream types.
Root reports distinguish the Arrow metadata enum discriminants from version
names (3 is V4, 4 is V5). Standard relative alignment is eight bytes. Optional
64-byte buffer-offset validation does not certify host addresses, DMA or SIMD.
IPC structural validity is explicitly **not** canonical serialization.

CLI projections cap field/child counts, nesting, text/byte previews and total
nodes. Elided payloads set truncation flags; projections are not full or signable
decoded messages. Input/schema hashes cover the actual raw bytes read. A checked
i64 sum overflow is a projection limitation, and does not turn a valid IPC
stream into malformed input. Empty/all-null sums remain null, distinct from zero.

## Actual PMU provider and observed windows

The new `vesper-bench-pmu` package keeps OS-specific unsafe access outside the
safe `vesper-bench` core. The workspace now has 43 packages. Linux x86_64/aarch64
little-endian hosts can request a current-thread perf-event group for hardware
cycles and retired instructions. The provider uses kernel group-read syscalls,
not RDPMC. Other platforms return typed unavailability. No sysctl, privilege or
global affinity setting is changed.

Owned FDs close on every partial failure. Private nonclone snapshots/deltas
carry provider origin and generation; Rust thread restrictions and actual OS
PID/TID checks reject cross-thread/fork misuse. Event IDs, exact read shape,
enabled/running metadata, monotonic counter/time deltas and full scheduling are
validated. Uncertain transition errors poison/close provider state. Counter
accounting assumes no full u64 wrap or external reset/reconfiguration during a
window; it does not guess a physical 48-bit counter width or scale multiplexing.

Safe observation hooks bracket the actual existing ABBA body windows outside
Instant timing and setup/teardown. Every before/after error cleans available
prepared inputs and outputs, retaining primary callback/clock failures and
secondary observer errors. Ordered records preserve explicit invocation,
warmup and actual operation-completion metadata. Ordinary `run_abba` keeps its
API/error priority and a no-observation-vector Noop path.

`vesper bench FILE --pmu` requires actual counter observations while retaining
time as its default selected metric. `--metric time|cycles|instructions` selects
the paired samples and confidence gate; hardware metrics imply PMU. Top-level
provider, units, pairs and analysis consistently describe that selected metric.
Nested clock nanoseconds and PMU windows/cycle/instruction pairs retain both
evidence streams separately. Warmups remain explicit and are excluded from
measured pairs. Every invocation/completion, operation count, clock window,
provider/unit and enabled/running delta is checked against the actual run.

Both source arms compile before provider acquisition and all observations.
Opening or mid-run hardware unavailability returns one inconclusive report and
nonzero status, with typed reason/operation/errno and no fabricated sample,
analysis or clock fallback. Primary body/cleanup errors remain errors even when
secondary observation also fails. Checked A0+A3/B1+B2 aggregation and decimal
integer serialization preserve actual event counts. The unchanged exact
population-median confidence method retains its IID ratio assumptions; PMU does
not imply deterministic costs, cancellation of drift or a fixed sensitivity.

Hardware counts include documented enable/disable return paths, Instant reads,
dispatch and observer boundaries. Availability does not prove zero allocation,
vectorization, precision, scheduler isolation or a fixed cycle cost. Actual
positive PMU coverage is **not available on this cloud host**: the live probe
returns PermissionDenied at perf_event_open, OS errno 1, exit 2. That is an
explicit unavailable result, never a successful zero-count measurement.

## Configured lint outcomes

`vesper lint FILE --config PATH` loads one explicit bounded Loom TOML snapshot;
there is no ancestor/adjacent discovery and no formatter configuration. Policy
records path, byte length, BLAKE3, effective severities and unavailable rules.
Unrelated manifest sections can coexist with `[lints.vesper]`; malformed,
duplicate or unknown policy settings and unsupported plugin packs fail closed.

Allow suppresses both a finding and its edits. Warn preserves current behavior.
Remaining denies return one denied report and nonzero status, including truthful
`changed` data if other validated fixes were applied. Full candidate checking,
actual SemVer compatibility and staging ownership protections remain intact.
Explicit future warn/allow rules report unavailable; future deny requirements
cannot pass as if analyzed.

Review found and fixed a coverage hole: generated trait-method bodies were
excluded from ordinary source-owned lint facts without an ordinary Skipped
entry. Configured deny now also reports excluded generated bodies and fails
closed when either those bodies or conservative skips prevent full coverage.
Real trait-implementation regressions exercise both supported deny rules.
The actual documented policy fixture permits a checked SemVer-safe demo fix,
reports the remaining projection warning and three unavailable future rules,
and the resulting source executes to 42.

## Review and validation

Independent read-only reviews found no additional blockers in Protobuf/DER
canonicality, Arrow framing/metadata/aliasing, PMU lifetime/error cleanup, policy
bounds/outcomes, or the root Grammar CLI. Reviews distinguished requested metadata
capacity from actual allocator usage and runtime roundtrip checks from source
proofs. The generated-method coverage issue was reproduced before its fix.

Integrated validation completed locally:

- `cargo test --workspace --locked --offline --no-fail-fast`, with explicit
  `VESPER_LEAN=/home/agent/.elan/bin/lean`: **1,754 passing tests across 212
  suites**, zero failures or ignored tests. Conditional live PMU tests reported
  actual permission denial, not positive hardware evidence.
- `cargo clippy --workspace --locked --offline --all-targets -- -D warnings`:
  passed. Formatting and diff whitespace checks passed.
- The full `scripts/test-canonical-protocols.sh` Cargo/helper/example sequence
  passed: canonical Protobuf/DER, actual PyArrow versions/nulls/sums/overflow,
  deny/fix/SemVer/run42, and all three PMU metric modes. Hardware requests were
  explicitly unavailable on this host. All three codec examples executed; the
  real PMU probe returned the documented unavailable exit 2.
- The existing `scripts/test-source-tooling.sh` also passed unchanged after the
  new policy/metric integration, including default clock-only benchmarking,
  formatting, source lint fixes, documentation and shared-JIT doctests.
- All four protocol/policy/benchmark libraries passed Windows GNU all-targets
  cross-compilation. Owner checks also compiled Linux AArch64 and macOS AArch64
  PMU paths/stubs. These are compilation evidence; actual native acceptance is
  delegated to the PR workflows.
- The recorded specification inventory and its 14 Python regressions passed.
- Final benchmark CLI tests passed all six cases after removing a cross-process
  availability assumption: each child command's actual measurement/unavailable
  result is authoritative, since a preceding probe can fail transiently.

Each lane's report records its independent examples, reference fixtures, direct
API cases and precise target limitations. The native Ubuntu/Windows/macOS
acceptance workflow remains a normal PR/main check with read-only permissions.
It never requires a noisy speedup or host PMU availability in order to pass.

No registry dependency versions changed: the new PMU crate reuses libc and lint
policy reuses the already locked TOML 0.8.23. Existing source AST/checker/Place IR,
arithmetic, interface/native ABI and the AST-based Lean extractor stay unchanged.
The approved `k_i ⊑ m_i` field restriction remains in force.

## Remaining specification scope

This is a bounded codec/provider/policy implementation batch. Source
`@wire_grammar`/`@secret_grammar` Forge generation, general PEG/ASN.1/Protobuf/Arrow
coverage, source Region/linear protocol integration, SIMD projection,
constant-time parsing, extracted Rust/compiler proofs, RDPMC/macOS counters,
deterministic virtual-cost gates, code-layout randomization and causal profiling
remain separate work. No whole-design completion or hardware speedup is claimed.
