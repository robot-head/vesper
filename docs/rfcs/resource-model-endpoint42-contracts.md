# Executable finite resource-model endpoint (#478–#482)

Frozen base: main `23528ac37145b5389d54b64198edb0daf3633224`.
Before source, all 72 open issues and actual file lists of all 20 open PRs
were reviewed. The specification revision remains 2026-11-13T04:46:25.948Z.
Historical #309 documents are planning checkpoints. This is a new, narrower
implementation contract. Claude #453 owns ReferenceEval/source lockstep;
#454 owns MetatheoryFull extensions. Their paths remain read-only here.

The endpoint exposes the EXISTING finite model, not a new checker/evaluator:
`validate → checkProgram → validateInvocation → evalSteps`. It does not claim
source adaptation, native observation, a 100,000-program campaign, whole-checker
verification or compiler refinement. Existing proof-audit status is unchanged.

## Ownership

- #479: NEW `lean/ResourceOracle42.lean` and matching `*-worker.md` report.
- #480: NEW `src/bin/resource_oracle42/protocol.rs` and `*-protocol.md`.
- #481: NEW `src/bin/resource_oracle42/process.rs` and `*-process.md`.
- #482: NEW `tests/resource_oracle42.rs`, `tests/fixtures/resource_oracle42/**`
  and `*-acceptance.md`.
- Root: this contract, NEW `src/bin/resource_oracle42.rs`, module glue
  `src/bin/resource_oracle42/mod.rs`, integration report, and one BUILD
  `copy_test_cwd` label for the new strict-source test. Reports have prefix
  `docs/rfcs/resource-model-endpoint42-`.

No existing model/metatheory/proof, parser/checker/runtime/native, extractor,
kernel/profile/auditor/projector, Cargo manifest/lock or workflow source edits.
Generated Lean uses AST plus one render; this fixed handwritten IO worker is
allowed. No sorry/admit/project axioms/unsafe/partial/native_decide.

## Framing, lexical envelope and quotas

One persistent worker reads four-byte little-endian length-prefixed JSON.
Length excludes the header; zero or oversized lengths are fatal before payload
allocation. EOF at a frame boundary retires normally with BYE; partial header
or payload EOF is fatal. No newline-delimited alternative or stdout noise.

| Fixed transport bound | Value |
| --- | ---: |
| Request payload | 4,194,304 bytes |
| Response payload | 262,144 bytes |
| JSON nesting, counting outer array as one | 8 |
| Lexical tokens | 1,000,000 |
| Total decoded constructor rows per request | 131,072 |
| Rows in any one type/value/source code | 2,048 |
| Input types or input values, independently | 65 |
| Constructed tree depth | 256 |
| Unsigned JSON control maximum | 9,999,999,999 |
| Canonical natural decimal string length | 20 |
| Other string length | 128 ASCII bytes |
| Requests per session | 4,096 |
| Total framed bytes, both directions | 67,108,864 |

Before Json.parse/serde_json, scan bytes: only arrays, commas, JSON whitespace,
canonical unsigned digit tokens, and unescaped printable ASCII strings. No
objects, booleans, null, escapes, negative/fraction/exponent numbers or Unicode.
Each opening array, string and number counts as one token; closing brackets
and commas do not. Controls obey the fixed maximum. No leading numeric zero
except `0`. Natural strings obey `[0-9]+`, no leading zero except `0`, at most
20 digits; DO NOT impose the model's u64 range in the transport.

Compute all input/code row counts before model-tree construction. Postfix
stacks consume children once, with exact arities, bounded depth and exactly
one final value. No node-index DAGs or hidden expansion. Do not silently clamp
zero/over-hard MODEL limits or reject representable semantic negatives here.
Transport bounds are separate from model admission. Before evaluating a
request, prepay its complete frame plus one maximum response and a 512-byte
complete BYE-frame reserve against the remaining session wire budget. The
reserve is a fixed schema constant, not a second maximum response. Output
overflow is fatal, never truncation.

## Request and constructor codes

The ONLY request is:

```text
[1,"eval",sequence,limits14,inputTypeCodes,sourceCode,inputValueCodes,fuel]
```

Sequences start at zero and increase by exactly one. `limits14` order is:
rawNodes, context, rawDepth, typeNodes, typeDepth, valueNodes, valueDepth,
initialValueNodes, inputs, expressionNodes, runtimeDepth, resources, events,
fuel. Default vector is `[256,64,64,255,16,255,16,4096,16,65536,256,64,128,4096]`.
The last request `fuel` is the actual invocation fuel, independently admitted.

Modes: 0 copy, 1 affine, 2 linear. Floors: 0 copy, 1 affine. Children occur
left-to-right before their parent; pop rightmost child first.

| Code | Rows |
| --- | --- |
| Type | `[0]` unit; `[1]` nat; `[2]` ticket; `[3,floor,leftMode,rightMode]` pops left/right types |
| Runtime value | `[0]` unit; `[1,"decimal"]` nat; `[2,id]` handle; `[3]` pops left/right values |
| Source | `[0,site]` unit atom; `[1,site,"decimal"]` nat atom; `[2,site,index]` variable atom; `[3,site,floor,leftMode,rightMode]` pair; `[4,site]` let; `[5,site]` unpack; `[6,site]` ifz; `[7,site]` mint; `[8,site]` consume |

Pair pops two atoms. Let pops initializer/body expressions. Unpack pops
atom/body. Ifz pops atom/zero/nonzero. Consume pops one atom. Atoms coerce to
SourceExpr.atom when an expression is needed, with no extra site. Non-atom
operands are wire errors. Handles remain representable as invocation values
so actual validateInvocation can reject them; source handles have no tag.

Requests contain no inferred type, usage mask, acceptance Boolean, proof,
expected outcome/trace or callback/module/source text.

## Server messages and actual payloads

READY is emitted once:

```text
[1,"ready","vesper.resource-model42/1","4.24.0",
 "797c613eb9b6d4ec95db23e3e00af9ac6657f24b",bounds12]
```

`bounds12` is the table's first nine bounds (request through control), then
natural digits, requests/session, total framed bytes. Other-string limit 128
and the 512-byte BYE-frame reserve are fixed by this schema and omitted from
that vector.

Every actual response is `[1,"result",sequence,stage,payload]`.

| Stage | Payload |
| --- | --- |
| `validation_error` | `[ValidationError]` |
| `check_semantic` | `[CheckError,sourceStats]` |
| `check_profile` | `[CheckProfile,sourceStats]` |
| `invocation_error` | `[checkedInfo,InvocationError]` |
| `eval_admission_error` | `[checkedInfo,invocationStats,InvocationError]` |
| `evaluation` | `[checkedInfo,invocationStats,status,events,residual,ledger,transitionCount]` |

`sourceStats=[nodes,depth,maxContext,mints,consumes]` from actual validation.
`checkedInfo=[resultTypeCode,usesFlags,sourceStats]` from the actual checked
wrapper; usage flags are 0/1. `invocationStats=[nodes,depth,maxValueNodes,
maxValueDepth,valueStats]`, where valueStats is `[]` for none or `[nodes,depth]`.

All diagnostic enum values are arrays `["constructor",payloads...]`, using
the EXACT existing Lean constructor spelling and declaration-order payloads.
Nested enum arguments use this same encoding; no extra type-name wrapper.
CheckError is `["kind",site,slot]`, where the first string is the actual kind
constructor (not the literal `kind`), with slot `[]` or `[index]`. Its closed
kind set is unknownVariable/typeMismatch/duplicateUse/unusedLinear/
fieldModality/literalOutOfRange/branchOwnership. TypeError, ValidationError,
CheckProfile, InputError, InvocationError, CapacityError, ResourceRefusal and
InvariantError encode ALL actual constructors from the frozen model. Unknown
constructors/extra payloads are wire errors, not a generic semantic error.

Status is `["completed",valueCode]`, `["fuelExhausted"]`,
`["resourceLimit",ResourceRefusal]`, or `["invariantFailure",InvariantError]`.
Events are actual chronological `[action,site,id]` rows (0 mint/1 consume).
Residual is actual `[id,mintSite]` rows in ledger order. Full ledger is actual
`[mintSite,liveFlag]` rows, including tombstones; index is identity. Transition
count is `result.actions.length`, including silent actions, NOT event count.
Stopped expression snapshots are outside this wire.

No execution data exists on any pre-execution refusal. No fabricated empty
trace/residual. Returned owners can legitimately remain live. No automatic
consume/cleanup event. Invocation data never affects static inference.

After stdin EOF at a complete frame boundary, emit:

```text
[1,"bye",nextSequence,
 [requests,validationErrors,checkSemantic,checkProfile,
  invocationErrors,evalAdmissionErrors,evaluations]]
```

Counters count actual completed responses, partition requests exactly, and
must match the host. Flush, exit zero and close stdout. Host requires exact
BYE, EOF without trailing bytes and successful exit. Malformed input, quota,
sequence or IO failure emits no model result and exits nonzero with bounded
static stderr reason. Truncation cannot retire successfully.

## Frozen Rust surface

Protocol is self-contained; no `use super::*` or process dependency. Public:

```text
Natural::parse(&str) -> Result<Natural,WireError>; Natural::from_u64(u64)
Mode::{Copy,Affine,Linear}; Floor::{Copy,Affine}
TypeRow::{Unit,Nat,Ticket,Pair{floor,left_mode,right_mode}}
ValueRow::{Unit,Nat(Natural),Handle(u64),Pair}
SourceRow::{Unit{site},Nat{site,value},Var{site,index},
 Pair{site,floor,left_mode,right_mode},Let{site},Unpack{site},Ifz{site},
 Mint{site},Consume{site}}
TypeCode=Vec<TypeRow>; ValueCode=Vec<ValueRow>; SourceCode=Vec<SourceRow>
Request{limits:[u64;14],input_types:Vec<TypeCode>,source:SourceCode,
 input_values:Vec<ValueCode>,fuel:u64}
encode_request(sequence:u64,&Request) -> Result<Vec<u8>,WireError>
decode_request(&[u8]) -> Result<(u64,Request),WireError>
decode_message(&[u8]) -> Result<ServerMessage,WireError>
read_frame(&mut impl Read,cap:usize) -> Result<Option<Vec<u8>>,WireError>
write_frame(&mut impl Write,&[u8],cap:usize) -> Result<(),WireError>
ServerMessage::{Ready(Ready),Result(ModelResponse),Bye(Bye)}
ModelResponse{sequence:u64,outcome:Outcome}; ModelResponse::stage()->Stage
Stage::{ValidationError,CheckSemantic,CheckProfile,InvocationError,
 EvalAdmissionError,Evaluation}
```

Typed successful/check/diagnostic DTOs follow the exact schema above and
derive Debug/Clone/Eq/Serialize where useful; no unvalidated Value catch-all.
Protocol owner publishes precise DTO fields before dependent writers compile.

Process exports `Config{lean:PathBuf,repo_root:PathBuf,artifact_dir:Option<PathBuf>,
limits:HarnessLimits}`, default HarnessLimits, non-Clone `#[must_use] Session`,
`start(&Config)->Result<Session,HarnessError>`,
`Session::evaluate(&mut self,&Request)->Result<ModelResponse,HarnessError>` and
consuming `Session::finish()->Result<SessionEvidence,HarnessError>`.
HarnessError exposes typed `kind` and bounded `detail`. Hard outer deadlines
are compile 180s/module, request 60s, shutdown 15s; defaults 120s/30s/5s.
Default requests/session 64, maximum 4096; stderr default256KiB/hard1MiB.
Caller limits may lower resource/time bounds, never weaken semantic/schema
checks. Detect abandoned sessions; kill/reap for hygiene never means model
consumption or a completed report. IO writers/readers and queues are bounded;
a blocked stdin write, flood, crash or stall cannot bypass the deadline.

## Genuine execution, evidence and completion

Embed the exact fixed project sources; compare regular on-disk sources before
copying into a fresh private root. No arbitrary model/proof/module selection.
Build original executable objects in order MetatheoryFull, ResourceModel42.Syntax,
ResourceModel42.Checker, ResourceModel42.Evaluator, ResourceOracle42. Use real
pinned Lean 4.24, one thread, `-j1 -M4096`, explicit private LEAN_PATH and clean
environment with necessary Windows system variables. Do not substitute a
kernel projection. Manifest every actual source/object/companion needed by
execution, byte counts/BLAKE3, compiler/version/commit, paths, commands/exits
and direction+length+payload transcript hashes. Verify identities before and
after. Finite compiler output and artifact limits are mandatory. Record the
worker as transport code, not an externally verified implementation proof.

Root CLI accepts one bounded complete request JSON (sequence zero), a Lean
path and optional artifacts. It calls the same production Session and emits
a model-only observation/evidence report. No native/source/campaign badge.

Positive acceptance uses the actual built worker. Cover real return values,
static categories, input substitution, mint/consume/return-live owner, ID
freshness after consumption, both branch values, unpack order, zero/exact fuel,
and genuine limited prefixes. Distinguish all stages and preserve actual
counts. Same syntax with different inputs must retain static checked data.
Hostile frame/schema/decimal/sequence/truncation/flood/stall/crash and artifact
mutation controls must exercise production paths. Synthetic peers are solely
negative/transport controls and cannot supply a successful mathematical case.

At most two Lean compiler leases; one Cargo/shared-target lease. Each writer
uses an isolated worktree and genuine immutable versioned dependencies.
Root integrates and runs real endpoint/CLI/transport tests, strict Clippy,
fmt/BUILD/whitespace/source checks, then pushes a protected-main PR. Do not
claim hosted CI or a larger resource campaign passed before observation.
