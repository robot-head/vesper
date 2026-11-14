# Wave seven, lane 30: source capability effects, the rest (§23.2, §32.1-§32.2, §36)

Claim: #353. Branch `claude/project-thread-s63ywb`, based on `main` at
`283c8115d2203232d0e9b473cc4bb0312c3a8509` (a descendant of the required
baseline `c025b9a`). Root package only; no new dependencies, no unsafe code.

Lane 15 (#257, PR #278) and its follow-up (#310, PR #311) built `ConstTime`,
`Realtime` and `DetFloat` and listed what was left. This lane delivers the
four items of the wave-seven prompt:

1. the built-in secret operations (`ct_select`, `bit_xor`, `load_sec_u64le`
   and the rest of §23.2's opaque-scalar API) and the `ConstDenorm` and
   `RtBus` capabilities;
2. interprocedural secret-parameter inference;
3. taint through the values a handler's `resume` passes and returns;
4. `lean/CapabilityEffects.lean`: the taint rules as a model with a proved
   non-interference theorem about the model.

## What was built

| Path | Content |
| --- | --- |
| `src/capability_effects.rs` | All rules, the secret prelude, the bitwise intrinsic table and semantics, the interprocedural fixpoint, the new policy fields |
| `tests/capability_effects_more.rs` (new) | 14 tests |
| `tests/fixtures/capability_effects/more/{accept,reject}/` (new) | 6 accepted and 19 rejected `.ves` fixtures, canonically formatted |
| `tests/fixtures/capability_effects/reject/consttime_public_param.ves` | Rewritten: its old program is now accepted by inference (see Decisions) |
| `lean/CapabilityEffects.lean` (new) | The model and its theorems |
| `src/check.rs`, `src/ir.rs`, `src/runtime.rs`, `src/native.rs`, `src/native_verifiers.rs` | Narrow hunks in their own commit, listed below |

### 1. Secret operations, ConstDenorm and RtBus

**Bitwise intrinsics.** The core language has no bitwise operators, and the
secret operations cannot be written with `+ - *` alone (an xor is not a
polynomial function modulo 2^64). So this lane adds five total intrinsics,
`bit_and`, `bit_or`, `bit_xor`, `bit_shl` and `bit_shr`, typed like
`wrapping_add` (two operands of one integer kind, same kind out; shift
amounts modulo the width; `bit_shr` logical on unsigned kinds and arithmetic
on `i64`), and `widen_u64`, the zero extension of a `u8`, `u32` or `u64`.
Their table and reference semantics live in `capability_effects`
(`BIT_INTRINSICS`, `WIDEN_INTRINSIC`, `bit_intrinsic`, `bitwise`); they are
separate from the wrapping intrinsics of `vesper.arith.checked/1`, whose
table, contract text and version are unchanged. A user function of the same
name takes precedence everywhere. They lower to `Op::Binary`/`Op::Unary`
with the intrinsic's name in the Place IR, are evaluated by the evaluator and
compiled to `band`/`bor`/`bxor`/`ishl`/`ushr`/`sshr` (amount masked to the
kind's width, narrow results reduced as for wrapping operations) by the
Cranelift backend. Lean extraction rejects them by name ("unknown Lean
callee bit_xor"), as before for any unknown callee.

**Secret operations.** §23.2 declares the secret scalars as ordinary
`copy struct`s, so (as in lane 15) programs declare them; the core language
has no tuple structs, so the canonical shape is a single named field over
the payload kind (`copy struct SecU64 { value: u64 }`, `SecU8`/`u8`,
`SecU32`/`u32`, `SecMask64`/`u64`). When a program declares them and mentions
an operation's name, the checker adds the operation as an `eff [ConstTime]`
function written with the intrinsics (`SECRET_PRELUDE`):

- `SecU8`, `SecU32`, `SecU64`: `bit_and`, `bit_or`, `bit_xor`,
  `wrapping_add`, `wrapping_sub`, `wrapping_mul`; `ct_eq` and `ct_is_zero`
  returning a `SecMask64` (all ones or zero, branch-free:
  `((x | -x) >> 63) - 1`);
- `SecMask64`: `bit_and`, `bit_or`, `bit_xor`, `ct_not`, `ct_select(a, b)`
  (`b ^ (mask & (a ^ b))`) and the gate `declassify_bool` (true when the
  mask is nonzero);
- free functions `ct_select(mask, a, b)` and `load_sec_u64le(bytes)`, eight
  little-endian bytes as a `SecU64`.

A user definition of the same name wins, and nothing is added to programs
that do not use the operations, so their worlds and every emitted artifact
are unchanged. The added functions are parsed from generated source padded
past the module's last span, because the checker keys types by span; no
diagnostic points into them (tested). They pass Vault's Stage 5
constant-time verifier when `vesper emit --emit object` runs it
automatically.

**ConstDenorm** is a fourth built-in capability (`Capability::ConstDenorm`):
reserved (E0100), unhandleable (E0701), no type arguments (E0400), never
requested from callers, and its callees must carry it (E0700), like the
others. New rule E0732: a row or closure combining `DetFloat` and
`ConstDenorm` is rejected, because flushing subnormals changes IEEE 754
results. Both backends apply it in software: inside a `ConstDenorm` function
or closure, every `f64` unary, binary and compound-assignment operation
flushes subnormal operands and results to a zero of the same sign
(`const_denorm_spans`, `flush_subnormal`; the evaluator's AST and IR paths,
and a `fabs`/`fcmp`/`fcopysign`/`select` sequence in Cranelift). The policy
input lists the `ConstDenorm` functions (`CapabilityPolicies::const_denorm`)
for a backend that sets hardware FTZ/DAZ instead; it has no Stage 5 verifier
(`cap_verifier` returns `None`).

**RtBus** is not a capability: §32.2 declares it as a `linear effect` with
operations the host implements. The program declares it; the checker adds:

- E0724: `RtBus` must be declared `linear`;
- E0725: an `RtBus` operation performed outside `eff [Realtime]` code;
- the operation clauses of an `RtBus` handler run inside the realtime
  callback that performed the operation, so they are checked as realtime
  code: their callees must carry `Realtime` (E0700), they may not perform
  or handle forbidden effects (E0720) or install resumable handlers (E0723),
  and they are nodes of the realtime call graph (E0722).

A realtime body still cannot handle `RtBus` itself (E0723, it is linear).

### 2. Interprocedural secret-parameter inference

Lane 15 required a secret to cross every call through a secret-typed
parameter (E0705). Now, when ConstTime code passes a secret-derived value to
a parameter of a ConstTime function that is not secret by type, the
parameter is *inferred* secret: the callee is checked with it secret, and the
call's result is secret-derived in the caller (as before, when any argument
is). An `inout` parameter that a ConstTime function writes a secret into is
an inferred *output*: its callers' argument places are secret after the call.
Forwarding the place to such a callee (`store(inout out, x)` inside a
function whose own `out` is `inout`) makes the forwarding parameter an
inferred output as well.
The facts form a monotone fixpoint over the whole program (`summarise`),
computed before the rules are checked.

Each function is then walked twice. The first walk starts with only the
secret-typed parameters secret and decides the result boundary: E0706 still
rejects returning, as public, a secret the function did not receive through
an inferred parameter. The second walk starts with the inferred parameters
secret too and applies every other rule, so a branch, division, index or
checked operation on a value some caller passes secretly is reported in the
callee (E0702-E0704). Deliberately not inferred:

- calls from plain code (secret payloads can only be read inside ConstTime);
- calls through function values (their targets are unknown): a secret passed
  to a function value's public parameter is still E0705;
- outputs of functions used as values: writing a secret into their public
  `inout` parameter is still E0705;
- effect operations, whose handlers are not checked as constant time: still
  E0705.

`ConstTimeInput::secret_params` now includes the inferred parameters, which
`inferred_secret_params` lists separately, so Vault's verifier treats them as
secret sources.

### 3. Taint through `resume`

- A handler clause that calls its continuation with a secret-derived value
  marks that operation: every perform site of it, in any function, yields a
  secret (`Summaries::resumed`). With handler state, the continuation's last
  argument is the next state, whose taint the state tracks as before.
  Resume arguments are still coerced to the continuation's parameter types,
  so a closure without the capabilities they require is E0708.
- Calling the continuation returns the handled computation's result through
  the return clause, so its value is secret when that result is. The
  clauses, the state and the result depend on each other; the walk iterates
  them to a fixpoint (at most four rounds of one-bit facts).

### 4. Lean model

`lean/CapabilityEffects.lean` (Lean 4.24.0, no `sorry`, no `admit`, no
axioms declared) models a first-order language with total operations, a
constant-time select, branches, variable-latency operations, two-parameter
calls whose callee is checked under its callers' labels, perform/handle with
a resumed value, and declassification. Evaluation returns a value and an
observation trace (branch decisions and variable-latency operands).

- `noninterference`: for a well-labelled (`ok`), declassify-free expression,
  two runs whose environments agree on public variables, and on the resumed
  value when it is public, have equal traces, and equal values when the
  result's taint is public. This is the constant-time and public-result
  property of the modelled rules.
- `taint_mono`, `ok_mono`: checking under more secret labels is stricter.
- `call_ok_of_summary`: a callee checked once under summary labels that
  cover a call site's labels is well labelled at that call site, which is
  why one inferred label set per function (the union over its callers) is
  sound in the model.
- Four `example`s show the rules are not vacuous (secret branch, branch on
  a secret resume, a callee branching on an inferred parameter, and an
  accepted secret select).

These are theorems about the model only, not about the Rust checker, the
evaluator, the backends or the machine code; declassification is excluded
from the theorem's hypotheses rather than modelled as delimited release.
`tests/capability_effects_more.rs` checks the file with `VESPER_LEAN`.

### Public API (additions)

- `capability_effects::{CONST_DENORM, RT_BUS, BIT_INTRINSICS,
  WIDEN_INTRINSIC, SECRET_PRELUDE, bit_intrinsic, bitwise}`;
  `Capability::ConstDenorm` (`Capability::ALL` has four entries).
- `CapabilityPolicies::const_denorm: Vec<ConstDenormInput { function, span }>`;
  `ConstTimeInput::inferred_secret_params`. The schema string stays
  `vesper.capability-policies/1`: fields were only added.
- Diagnostics E0724, E0725, E0732.

### Hunks outside the lane's paths (own commit)

| File | Hunk |
| --- | --- |
| `src/check.rs` (lane 29) | Chain the secret prelude's items in `World::collect`; type the bitwise intrinsics like `wrapping_*` after the user-function lookup; `widen_call` |
| `src/ir.rs` (lane 28) | Lower the intrinsics to `Op::Binary`/`Op::Unary` |
| `src/runtime.rs` (lane 28) | Evaluate them in `binary`/`unary`; flush subnormals in `ConstDenorm` code (`binary_at`/`unary_at`, a `flush` flag on the operator frames) |
| `src/native.rs` (lane 23) | Direct and Place-IR forms, and `bitwise`; `Codegen::flush` for `ConstDenorm` float operations |
| `src/native_session.rs` (lane 23) | Pass the `ConstDenorm` spans to the session `Codegen` |
| `src/native_verifiers.rs` (lane 23) | `cap_verifier` returns `Option`; `ConstDenorm` has no verifier |

## Checks

- `cargo test -p vesper --test capability_effects_more`: 15 passed (positive
  and negative tests per rule, both backends, flush-to-zero on both
  backends, Lean model check).
- `cargo test -p vesper --lib` and the capability, verifier, native and
  runtime integration tests (`capability_effects`, `capability_verifiers`,
  `effect_optimization`, `native_batch`, `native_coverage_cli`,
  `native_suspend_abi`, `native_verifiers_cli`, `rewrite_runtime`,
  `runtime_resource_trace`, `source_native_suspension`): all pass.
- `rustup run 1.100.0 cargo clippy -p vesper --locked --all-targets -- -D warnings`: clean.
- `cargo fmt --check` and `vesper fmt --check` on every new fixture: clean.
- `lean lean/CapabilityEffects.lean` with Lean 4.24.0: no errors, no
  `sorry`, `admit` or `axiom`.
- The full workspace test suite did not fit on this machine's disk; CI's
  "Test all targets" covers it.

## Decisions

- **The old E0705 fixture changed.** Lane 15's
  `reject/consttime_public_param.ves` passed a secret payload to a ConstTime
  helper's public parameter; with inference that program is accepted, which
  is what item 2 asks for. The fixture now passes it to a function value's
  public parameter, which still rejects. The other lane-15 fixtures and tests
  are unchanged and pass.
- **Built-in operations as checked source.** Writing the secret operations
  as ConstTime functions over primitive intrinsics keeps every backend's new
  surface down to five binary operators and one conversion, and lets the
  existing rules and Stage 5 verifier check the operations themselves.
- **Opt-in by use.** The prelude is added only when a program declares the
  scalars and mentions an operation, so existing programs and their
  artifacts are unchanged.
- **Inference is context-insensitive.** One label set per function, the
  union over its ConstTime callers; the Lean lemma `call_ok_of_summary` is
  the model-level justification. A helper called once with a secret and once
  with a public value is checked as secret for both.
- **Resume taint is global per operation.** Any handler resuming an
  operation with a secret makes all its perform sites secret. A resume that
  uses a value derived from an inferred parameter therefore also counts in
  the first walk and may report E0706 where the caller could have accepted
  the result: conservative.
- **RtBus stays a declared effect.** Its operations name types the core
  language does not have (`WireSlice`, `RtOutputLease`), so the checker
  recognises a declared `RtBus` instead of building one in.

## Remaining gaps

- `@realtime(stack_budget_bytes = .., period_us = ..)` needs §16.4 attribute
  syntax, which other agents own; it does not parse
  (`realtime_attribute_syntax_is_not_available`), and the stack budget in the
  realtime policy stays `None`.
- `ConstDenorm` is flushed in software, per operation; no backend sets the
  hardware MXCSR/FPCR FTZ/DAZ bits around the call. Float values that host
  effect operations or FFI return are not flushed until the next operation.
- `ConstTime::scope` and `DetFloat::scope` have no syntax; a call from plain
  code is still the entry.
- `load_sec_u64le` takes an eight-element `u8` tuple, not a `ByteArray[N]`
  with an offset: the core language has no byte arrays. `SecVec[P, T]` and
  vector secret operations wait for lane 28's vector types.
- Method calls on non-place receivers (`f(x).bit_xor(y)`) are not resolved by
  the core checker in general; bind the receiver first.
- Inference does not cross function values; the Lean model has no
  declassification theorem (delimited release) and no handler state.
- Lean extraction rejects programs that use the bitwise intrinsics.
- The evaluator still allocates on its own heap regardless of `Realtime`;
  the RtBus rules are source rules, not a wait-freedom proof of handlers.
