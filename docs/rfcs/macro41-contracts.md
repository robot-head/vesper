# Actual source expression macros and structured quotation boundary

Baseline main `47a093ea9464af47d8cb4cebb43d5b15e001b11a` is clean and synced.
PR102 passed all 23 jobs; Linux workspace 2,234 tests/286 suites, macOS 2,182,
Windows 2,111 (three existing platform-specific ignores). The live Google document
still has 41 sections/Appendix A, modified 2026-11-12T09:18:30.204Z. Section 16.4
and the actual Forge/compiler/sandbox APIs were reread before claims. Fresh open
board and changed-file checks reserve external Warp #101/#45; no Forge/syntax/
frontend collision. Shared Cargo registrations remain coordinated, not exclusive.

| Claim | Branch / owner | Exclusive production paths |
| --- | --- | --- |
| #103 | macro41/syntax / Beacon | New syntax macro_source.rs/private parser child, parser.rs/lexer.rs narrow actual-header/token adapters, syntax tests |
| #104 | macro41/typed / Flux | New Forge typed_quote.rs/children, host.rs/syntax.rs/token.rs narrow adapters, private resolve.rs preparsed adapter, typed tests |
| #105 | macro41/authority / Tensor | New src/forge_frontend/authority.rs and authority tests |
| #106 | macro41/lower / Aether | New src/forge_frontend/lower.rs/children and lowering tests |
| #107 | macro41/structured / Serde | New sandbox quote_transport/quote_host + wit/forge-quote + new real fixture/tests; host.rs shared-runtime adapters; new runner quote_* modules/tests/fixture and minimal runner.rs crate-private engine accessors; Forge quote_transport adapter |
| #108 | macro41/acceptance / Quarry | New forge_cli.rs, source macro fixtures, check/test-source-macros scripts and source-macros.yml |
| #109 | macro41/integration / root | forge_frontend/mod.rs, CLI adapter/main routing, lib/module/Cargo/lock registration, workflow consumed-path filters, contract/integration report |

Each owner also owns docs/parallel-results/macro41-<lane>.md. Separate NEW worktrees
from this contract commit, private caches only, one Cargo job/debug0/inc0. Root
target/parallel-next stays coordinator-only. Temporary dependency wiring is restored
before owned-only commits. Freeze concrete types/signatures with dependent owners
before code. Delivered paths stay reserved until reviewed merge. Check actual open
claims/PR files before assigning any further implementation. No work on Warp paths.

## Deliverable and compatibility

This batch produces a real Tier 1 source compiler path for fixed expression
quotations, and separately a real structured WASI quotation/cache foundation.
Source Tier 2 execution is NOT integrated until its compiler adapter exists.
Existing check/run/LSP/Lean do not silently start executing macros/components.
Default root Forge dependency uses default-features=false; pure Tier1 must not
force Wasmtime into the default CLI dependency path. Existing flat sandbox@0.1.0,
flat runner/cache/session APIs and fixtures remain compatible and generated-only.

Preserve core AST/IR schemas, ordinary checker acceptance/exhaustion, k_i⊑m_i,
arithmetic, interface/native ABI and AST-based Lean emission. No repair Unit,
wildcard, fake function signature or empty body establishes checked original source.
No guest scope/handle or caller World/graph/approved flag establishes authority.
Logical budgets do not prove RSS bounds, termination of user code, or latency.

The first source syntax is:

```vesper
macro keep($value: expr) -> expr { quote { $value } }
fn main() -> i64 { let n = 42; keep!(n) }
```

Fixed positional expression parameters, one quotation per definition, no recursive
or nested invocations/repetitions/pattern categories/attributes/derives/generated
items. Macro-bearing functions are top-level, nongeneric and have no parameters.
Ordinary support functions may have parameters. Caller bodies/quotations use the
documented Forge/Vesper expression intersection: names/literals/calls/let/blocks,
with explicit rejection of incompatible handlers and other unsupported forms.
Default macros emit no effects. A bounded optional policy file can authorize
specific grants/ceilings; its content is configuration, never compiler proof.
Initially one explicitly associated source package plus the compiler prelude;
reserve @vesper/builtin. No cross-package linker/registry-provenance claim.

## Source parsing and category semantics

MacroSource is privately constructed from actual original bytes/hash/lexer tokens
and read-only source-bound definition/function/header/body/invocation/argument
inventory. Retain actual source spans and token bytes; a lossless CST may be added
where the parser naturally constructs it. Do not claim a CST where only tokens
exist. Parse FunctionHeader separately from body; no placeholder core Function.
Complete support items use actual full-source token ranges with absolute spans.
They are syntax, not a checked original program. Strings/comments never provide
invocation anchors; malformed headers/templates/unknown contexts fail explicitly.
Legacy valid batch parsing and recovery remain compatible. A Dollar token, if
added, must not reorder existing token discriminants or admit macros in the old
batch language. Freeze exact parser/helper APIs with root/typed/authority/lower.

Expr denotes a grammar category, not successful typing/effects/ownership.
Source argument category validation is strict. Typed quotation slots are Expr
atoms; an input may not occupy a binder/path/type/delimiter position. Host grouping
and a final structural parse-versus-substituted-tree comparison prevent boundary
drift such as `$x(2)` for x=`let a=1;a`. Preserve real block trailing semicolon/unit
semantics; the old Forge Term::Block alone loses that information. Keep compatible
old untyped APIs while exposing a richer sealed shape for the compiler bridge.

HygieneHost owns session/scope/expansion/slot identities. New Expr inputs, slots,
expansions and resolved-body wrappers have private construction and no unchecked
serde constructors. Import real Vesper tokens/spans; the old fixture lexer alone
cannot attest source identity. Grouping/generated display spans do not become
original-source evidence. Every repeated slot charges its full copied token/text/
scope cost before growth. All failures close the expansion; counters never wrap.
Resolve the WHOLE expanded function body, including its original let bindings,
not each invocation independently. The binder excludes its own initializer.
The narrow private resolve_preparsed adapter may reuse existing resolution for a
Term built from the sealed validated rich shape. It does not expose a public
arbitrary-Term authority path or change legacy resolve grammar; empty Block only
represents actual trailing-let/unit syntax, never parser repair.

## Identity lowering and the final checked artifact

Root orchestration owns every authoritative input. A syntax-derived declaration
registry can establish name/DefId identity before expansion, but is NOT authority.
Internally derive/validate DefIds; bind package, definition names/kinds and actual
source. Keep global source names and reject duplicate/reserved/ambiguous identities.
Macro definitions are removed from the normal expanded artifact, not replaced by
dummy values. Preserve actual source headers/support text; replace only actual
macro-bearing body ranges with lowered output. No source padding/wrapping.

Lower through resolved BinderId/DefId, never plaintext name matching as hygiene.
Alpha-rename every supported local BinderId to a fresh collision-free Vesper name;
global DefIds map through the trusted declaration table. Build the supported real
core AST, then serialize it with explicit precedence/block semantics. Retain its
expected shape so root can compare the full parsed candidate modulo source spans
against the lowered AST. Merely printing hygienic tokens and reparsing ordinary
spelling is insufficient. Unknown/forged/unsupported identities fail closed.

Every serialized occurrence gets unique real expanded-source byte positions.
Repeated antiquotes may map to the same original argument but must not share core
TypeMap/call span keys. Bounded source maps track real source-origin ranges and
generated origins through trusted invocation/definition metadata. Guest reported
spans remain display-only. Map offsets account for every actual source edit.

Only after full expanded-source check_source, expected-shape verification, actual
compiler-derived authority gating and normal IR validation may root construct
ExpandedChecked. Suggested public entry is expand_source(source, config, limits);
result getters distinguish original_source/hash from expanded_source/hash and
expose only the actual final checked snapshot/IR, bounded provenance and current
conditional macro audits. No public raw constructor/deserialization, no original
source checked flag, no partial accepted output on a late failure.

Proposed explicit commands: `vesper forge check SOURCE` (bounded JSON summary)
and `vesper forge expand SOURCE` (actual fully checked expanded source). Optional
package/policy flags are validated before copies; no implicit native/WASI execution.
Acceptance runs emitted normal source explicitly to verify expected result 42.
Root and acceptance owner freeze exact wire/CLI shape before fixture assertions.
The unannotated integer let retains the existing i64 default; expansion does not
insert casts to satisfy a different return annotation.

Frozen CLI: `forge check SOURCE [--package @local/source] [--policy FILE] [--json]`
returns one JSON object on stdout (success exit0, rejection exit1). `forge expand`
returns the EXACT fully checked expanded bytes, without an added newline; any
failure leaves stdout empty and exits1. Check JSON schema is
`vesper.forge.source/1`, status `expanded_checked`, scope `tier1_source`, with
package, original `source_blake3`, `expanded_blake3`, source_bytes, expanded_bytes,
macro_definitions, invocations, expanded_source, source_map and audits. Maps expose
expanded/original byte objects `{start,end}`, kind source/generated and macro name
or null. Audits name macro and original invocation span plus conditional evidence.
Rejection JSON uses status rejected, bounded stage/message/diagnostics and optional
expanded_source/source_map/source_blake3; no World or partial accepted artifact.
Stages are parse/expand/resolve/lower/check/authority/ir/resource/unsupported/config/io.

Policy files are at most64KiB, deny unknown/duplicate fields and use
`{"schema":"vesper.forge.policy/1","macro_emits":[{"name":"m","effects":["Storage"]}],"caller_ceiling":["Storage","builtin::Fail"]}`.
Lists default empty; unlisted macros remain pure. Effect names grant any arguments
for that exact known source effect or builtin::Fail; unknown/duplicate entries
reject. Package labels are at most256 bytes. These are explicit user configuration,
not authority derivation. Generic effect operations emitted directly remain denied.

## Compiler-derived authority

CheckedAuthorityBundle has private fields, no raw-World/graph constructor or serde
authority path. Derive from actual fully CheckedSource, validated package identity
and trusted declaration bindings; validate that all actual checked definitions
match. Guard AST/types/nominal expansion/metadata/work BEFORE ir::lower clones.
Use Op::Call.callee for actual static targets. World.calls contains substitutions,
not target identities. Ordinary nongeneric function calls produce graph edges;
effect operations use actual concrete checked substitutions. Declared/residual
effect rows and World.moves cannot establish purity or authority.

Independently account for World.partial_ops -> Fail[RuntimeError] and overflow_ops
-> Fail[ArithOverflow]. Instruction.fallible merges them and is insufficient.
If supporting Try, require actual closed Result error types. Reject handlers,
closures, indirect calls, function-valued dispatch, traits/models/impl elaboration
and unsupported generic contexts before traversal. Recursive call graphs may use
the existing bounded fixpoint; recursive nominal data remains unsupported.

The graph has no per-edge substitution arguments. Nongeneric helpers may call
generic effects with fully concrete checked substitutions accumulated into that
helper's direct requirements. Direct MACRO-GENERATED generic effect operations
remain unsupported until a per-call instance audit exists; never union all effect
instances into one global operation node or choose the first. Concrete type
arguments use versioned bounded structural qualified encodings, not Type::Display.
Compiler builtin wrapping intrinsics may have exact known pure nodes; they do not
authorize unknown builtins. Every expansion-contributed operation must satisfy
macro emits AND caller ceiling with current actual graph, even when source headers
or user version labels are unchanged. The audit is conditional macro evidence,
not a whole-package Loom confinement/signature/registry certificate.

## Structured WASI boundary (standalone foundation)

Use separate self-contained `vesper:forge-quote@0.1.0`, world
`sandboxed-quotation`, interface quotation, payload1. Actual Wasmtime runtime reuses
the bounded determinism/fuel/memory/table/stdout/declared-input controls without
changing flat exports. Inputs contain ordered Expr slots with kind/text/reported
span only, no host scope/nonces/DefId. Outputs are packed Generated token claims
or InputSlot(u32), diagnostics and text tables; no scope table. Guest DefIds are
claims; all reported spans are display-only. Failed outcomes carry no items.

Validate counts, table sizes, slice arithmetic/UTF8 and SUM of all aliased copied
lengths before allocations/copies. Slot indexes refer only to current declared
inputs, never SpliceHandles. The new runner/cache uses separate domain-separated
keys, VFRQ/schema1 raw records and protocol/profile-bound component bindings.
Key exact component/entry/kind/args/WIT/record schema, ordered slot categories/
boundaries/visible contents, observable limits/determinism, declared snapshots and
measured host identity. Cap canonical encoded invocation size before publication.
Use a bounded codec that checks counts before Vec allocations on decode.
New quote preparation may reuse the flat runner's verified component/input
snapshots and host identity through narrow crate-private engine accessors. It must
replace the invocation artifact and bind its own domain/version/WIT/profile; a
flat invocation/key cannot authorize quotation execution. Shared compiled-component
bytes still require exact quotation export validation at new-world instantiation.

Cache RAW generated claims/slot indexes/diagnostics/metering only, never HostToken,
scope/slot nonce/resolution/approval/audit. Every use imports into the current typed
host arena and freshly resolves/approves its current context. This foundation may
return raw execution plus explicit conditional fragment results; it cannot mint
compiler ExpandedChecked or claim source Tier2 integration. Build a real new
wasm32-wasip2 guest twice with pinned inputs, commit component+SHA, instantiate/run
it; keep legacy flat fixtures unchanged. Read runtime package/network guidance
before installing the missing target; use only private heavy caches and watch disk.

## Budgets and verification

Source defaults: source128KiB/tokens16384/macros32/functions128/invocations256/
parameters8/depth48/output32768 tokens+256KiB/metadata1MiB/work16MiB. Direct limits
and finite HARD ceilings reject overflow before traversal/clone/allocation. Failed
stages consume the shared budget, never reset it; maxima need not all fit together.
Normal frontend/IR guards still apply and may explicitly refuse larger expansions.
Owners freeze exact lower/authority stage budgets before writing dependent code.

Standalone structured defaults: slots64, input65536 tokens+1MiB, output65536 items,
slot occurrences4096, copied output1MiB, diagnostics256+64KiB, expanded65536 tokens+
1MiB+262144 scope entries, depth128, raw record8MiB, encoded invocation4MiB,
work16MiB/HARD128MiB. Root source profile remains tighter. Charge repeated copies,
host grouping and aliasing; no guest-declared source identity is trusted.

Tests must cover capture both directions/initializer scope/global pre-resolution,
actual linear duplication, real transitive/implicit authority, unchanged-header body
edits, unique repeated spans/current provenance, structural slot drift and trailing
semicolons, malformed/unsupported original syntax, unknown graphs/generic emitted
ops, stale/foreign/fabricated slots, resource failures without partial acceptance,
real WASI hostile components/raw cache freshness, and unchanged legacy flat/batch/
recovery/editor/protocol behavior. Observe actual native3OS CI and full workspace
Lean/strict Clippy. Root creates a linked protected PR and returns to clean main.
Full six syntax categories, source Tier2/derive/package linking, incremental macro
queries, semantic completion/proof lenses and latency guarantees remain future work;
the overall design implementation goal remains active.
## Structured adapter helper reservation

Before structured adapter implementation, #107 additionally reserves only two
narrow methods in `vesper-forge/src/typed_quote.rs`: crate-private
`TypedBudget::charge_metadata`, forwarding to the existing cumulative metadata
guard, and public read-only `ExprExpansion::validate_host`, checking the private
per-host Arc identity and current live expansion. No constructor, identity
mutation, scope weakening or other typed host changes are permitted by this
reservation. Raw serialized claims acquire no authority from these helpers.

