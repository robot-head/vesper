# Canonical protocol and hardware measurement contracts

Baseline: main `965b600408d148b01559624914097477da6691ca` ([PR #71](https://github.com/robot-head/vesper/pull/71)).
The specification snapshot remains sections 1–41 and Appendix A, modified
2026-11-12T09:18:30.204Z. The coordinator reread §§38–39 and the current runtime
APIs, checked open issues, and inspected the actual file lists of every open
external Warp PR before creating the following ownership claims.

| Issue | Branch | Exclusive implementation |
| --- | --- | --- |
| [#72](https://github.com/robot-head/vesper/issues/72) | `protocol41/protobuf` | New Grammar `protobuf.rs`, matching tests/example, optional `lean/GrammarCanonical.lean`, own report |
| [#73](https://github.com/robot-head/vesper/issues/73) | `protocol41/der` | New Grammar `der.rs`, matching tests/example, own report |
| [#74](https://github.com/robot-head/vesper/issues/74) | `protocol41/ipc` | New Grammar `ipc.rs`, matching tests/example/reference fixtures, own report |
| [#75](https://github.com/robot-head/vesper/issues/75) | `protocol41/pmu` | New `crates/vesper-bench-pmu/**`, existing bench `execution.rs` plus new observation tests, own report |
| [#76](https://github.com/robot-head/vesper/issues/76) | `protocol41/lint-policy` | New lint `policy.rs`/tests/example, lint lib/manifest registration, source lint adapter/CLI tests, own report |
| [#77](https://github.com/robot-head/vesper/issues/77) | `protocol41/integration` | Root workspace/lock/global CLI registration, Grammar lib/README registration, new Grammar CLI/tests, source benchmark PMU adapter/tests, acceptance workflow/scripts and integration report |

Reports use `docs/parallel-results/protocol41-<lane>.md`; lint policy uses
`protocol41-lint-policy.md`. All other production paths are read-only. External
Warp #36–#45 remains reserved, including its root Cargo/lock registration
patches. Those approved shared root patches remain allowed and must be preserved.
No source AST/checker/Place IR/interface/native ABI schema changes, arithmetic
changes or Lean extractor string generation are part of this batch. Preserve
`vesper.arith.checked/1`, interface/native ABI v2 and `k_i ⊑ m_i`.

## Codec APIs and claims

Grammar stays a root-free safe Rust crate. Each new codec lives in its own
module. Send its concrete public API/schema/value shapes to the coordinator
early, before CLI integration. Do not edit another codec or shared lib/README.
Use existing errors/helpers if appropriate; otherwise define module-local
typed errors with Display, offsets and clear unsupported/noncanonical cases.

All byte, schema, work, depth, count, metadata allocation and output limits
apply to direct typed APIs as well as serialized inputs. Do not recursively
clone an unvalidated schema or allocate from untrusted lengths before bounds.
Serialization failure must preserve the supplied output. Prefix APIs must
expose the exact consumed extent/tail; exact APIs reject trailing input.
Borrow actual payload bytes. Metadata allocations are bounded and reported;
there is no blanket zero-allocation or constant-time claim.

Protobuf uses an explicit named/versioned canonical profile. Standard protobuf
has no universal canonical encoding. Presence/default omission, repeated/packed
forms, ordering and unknown-field rules must be defined so accepted encodings
are unique for the actual logical values. Independent standard wire fixtures
must demonstrate compatibility of the supported profile.

DER must distinguish ASN.1 SET from SET OF and reject unsupported schema/types
explicitly. Minimal tag/length/primitive encodings and recursive constraints
must be validated before claiming any accepted-byte re-encoding identity.
No secret/PKCS#8/X.509 semantic validation is inferred from generic DER parsing.

Arrow IPC parses actual stream framing and FlatBuffer metadata, not just caller
descriptors. Primitive nullable i64 is the initial explicit profile. Schema,
field nodes, buffers and body bytes must agree, including actual validity/null
counts. Standard IPC alignment and optional 64-byte SIMD suitability are
separate properties. Reference Arrow-produced fixture provenance is required.

## PMU observation boundary

Unsafe OS access lives only in the new platform crate. `vesper-bench` retains
`forbid(unsafe_code)`. The first provider may use Linux perf-event group reads;
its identity must say so. This is not an RDPMC/12-cycle implementation. No
sysctl, sudo, global affinity or permission changes are allowed to obtain data.

Provider-produced snapshots/deltas own their event identity, thread/lifetime
and scheduling state; caller booleans or arbitrary counters are not hardware
evidence. Check event IDs, read format, enabled/running metadata, counter deltas,
wrap assumptions and errors, and close every partially acquired resource.
Unsupported platforms, permissions or multiplexed/unusable evidence remain
structured unavailable/inconclusive outcomes.

Freeze the safe observer/provider APIs before root integration. Observation
hooks bracket actual ABBA body windows outside Instant timing and outside
setup/teardown. Hardware totals still include the documented observer/clock/
dispatch boundary overhead. Nanoseconds and hardware counts remain distinct.
Every observer error must clean all available prepared inputs/outputs. Preserve
the existing `run_abba` API/behavior and its ordinary acceptance tests.

## Policy and CLI boundary

Lint configuration follows `[lints.vesper]` allow/warn/deny and leaves formatting
zero-configuration. Parse bounded actual TOML with strict policy validation;
unrelated manifest sections may coexist. Explicit provenance identifies the
policy snapshot. Allowed findings cannot trigger fixes; remaining denied
findings fail the command. Defaults preserve current behavior. Unsupported
plugin packs or requested guarantees cannot silently pass as analyzed.

The root Grammar command reads bounded regular files without writes. Protobuf
requires an explicit bounded schema; schema JSON must reject decoded duplicate
keys and unknown fields before values are overwritten. Summaries must bound
output rather than expand entire untrusted payloads. The PMU benchmark option
must use actual observed finite JIT windows and distinguish unavailable hardware
from successful measurements. Neither CLI can infer proof/zero-allocation/SIMD
badges from parsed metadata or caller assertions.

## Delivery

Use separate worktrees from this committed contract, private Cargo targets,
one build job, zero dev/test debug information and no incremental compilation.
Temporary local registration is permitted for testing but final lane commits
contain only owned paths. The coordinator reviews APIs/implementations, runs
actual independent protocol/CLI examples, full workspace checks, strict Clippy,
pinned Lean for new models and native Windows/macOS/Linux acceptance before a
normal protected-branch PR. Record supported profiles and remaining source
macros/Region/SIMD/RDPMC/proof integrations honestly.
