# Wave six, lane 15: source capability effects (§23.2, §32.2, §36.2)

Claim: #257. Branch `claude/project-thread-862sm4`, based on `main` at
`07322e3ba47890e939a48967a1ea8f62fdd7aac1` (a descendant of the required
baseline `cda092f`). Root package only.

## What was built

`ConstTime`, `Realtime` and `DetFloat` are built-in capability effects of the
source checker. They are written in ordinary effect rows (`eff [Realtime]`,
`eff [ConstTime, Fail[ArithOverflow]]`) and need no declaration. This is the
first wave-six step that enforces source typing rules in the checker; the rules
below are exactly the ones it enforces.

| Path | Content |
| --- | --- |
| `src/capability_effects.rs` (new) | The rules, the diagnostics and `capability_policies` |
| `src/check.rs` | Five hook calls: reserve the names in `World::collect`, accept them in `valid_effects`, never request them in `require_effect`, reject them in `handle`, and run `capability_effects::check_world` after checking succeeds and before Place-IR validation |
| `src/lib.rs` | `pub mod capability_effects;` |
| `tests/capability_effects.rs` (new) | 12 tests |
| `tests/fixtures/capability_effects/{accept,reject}/` (new) | 4 accepted and 22 rejected `.ves` fixtures, canonically formatted |

No syntax was added: `crates/vesper-syntax` is untouched.

### Semantics

A capability effect is a property of a body, not a request to the caller.
Calling a `Realtime` function from plain code enters the realtime discipline;
it does not make the caller realtime, so the checker never asks the caller's
row for it. The discipline binds callees instead:

- **Call discipline (E0700).** A body carrying capability `C` may only call
  functions that carry `C`, and only through function values whose type
  carries `C`. Constructors and the `wrapping_*` intrinsics are allowed (single
  primitive operations). A callee may carry more capabilities than its caller,
  never fewer. Calls whose target or capabilities cannot be determined are
  rejected (E0709).
- **No discharge (E0701, E0100).** `handle Realtime in ...` is an error, and a
  source `effect DetFloat {}` declaration is rejected as reserved. Capability
  rows take no type arguments (E0400).
- **Function types (E0708).** A function type carrying `C` is inhabited only by
  code checked under `C`: a closure literal flowing into such a position
  (call argument, annotated `let`, struct field, function result, through
  `if`/`match`/block tails) must declare `C` itself (`|x: f64| -> f64 eff
  [DetFloat] { .. }`), or a named function must carry it. Closures written
  lexically inside a capability body are checked under the enclosing
  capabilities and may be called there.

**Realtime** (§32.1, §32.5):

- E0720: no effect the design names as allocating, blocking or suspending
  (`Alloc`, `Async`, `Net`, `Storage`, `Scope`, `Mutex`, `Lock`, `Block`,
  `Blocking`, `Ffi`, `Io`, `Sleep`) in a realtime row, handled in a realtime
  body, or performed there.
- E0721: no open effect-row tail on a realtime function or closure, and no
  call through a function value with an open row.
- E0722: no recursion, direct or mutual, among realtime functions. The core
  language has no loop construct, so recursion is its only iteration and no
  static bound can be written: every realtime call cycle is rejected at each
  call site that closes it. Calls through function values count: each one is
  an edge to every realtime function whose value is taken anywhere and to
  every realtime closure, so `apply(recur)` inside `recur` is a cycle.
- E0723: no handler of a resumable (`linear`) effect inside a realtime body;
  its captured continuation may need a suspended fiber stack. Abortive
  handlers such as `Fail[ArithOverflow]` are allowed.

**DetFloat** (§36.1, §36.2):

- Float operations are the correctly rounded IEEE 754 basic operations: `+`,
  `-`, `*`, `/`, negation and comparisons.
- E0730: float remainder (`%` on `f64`) is rejected.
- E0731: effect operations whose signature mentions `f64` are host
  implemented, so their rounding is platform dependent; they are rejected
  unless the operation itself declares `DetFloat`.
- The surface language has no fused multiply-add and no libm call, so there is
  nothing else to reject at source level. Contraction and reassociation are
  backend properties for Kinetic's Stage 5 auditor; the policy input says
  `allow_fma: false`.

**ConstTime** (§23.2):

- A value is secret when its type mentions a declared ADT named `Secret`
  (§22.4's `Secret[T]`) or one of §23.2's `SecU8`, `SecU32`, `SecU64`,
  `SecMask64`, directly or through tuple, argument or field types. §23.2
  declares these as ordinary `copy struct`s, so programs declare them too;
  undeclared names do not count. Everything computed from a secret is secret
  (forward taint over the surface HIR, with pattern bindings and assignments).
- Inside `eff [ConstTime]`: E0702 rejects a secret `if` condition, a secret
  scrutinee of a refutable `match` (a single irrefutable destructuring arm is
  not a branch), a secret left operand of `&&`/`||`, and `?` on a secret;
  E0703 rejects secret `/` and `%` (variable latency) and checked `+ - *` on
  secrets that the checker could not prove in range (an overflow abort is a
  secret-dependent branch: use `wrapping_*`); E0704 rejects a secret index;
  E0705 rejects passing a secret to a parameter whose type is not secret, to
  an effect operation's public argument, or writing it into a public `inout`
  parameter; E0706 rejects returning a secret-derived value from a non-secret
  result type (Unit results excepted), for named functions and for closures
  checked under ConstTime alike. Checked unary negation of a secret that may
  overflow is rejected like the binary operators. Generic effect parameters
  are judged by their instantiation in the active row.
- Declassification is an explicit, auditable gate: a `ConstTime` function
  whose name is `declassify` or starts with `declassify_` (the design's
  `declassify_bool`). Only a gate may return a secret's payload as public, and
  a call to a gate yields a public value.
- Everywhere, E0707: a secret's payload can only be read (field access or
  destructuring, including nested tuple and struct patterns) inside
  `eff [ConstTime]`, and secret values are never compared
  with `==`, `!=`, `<`, `<=`, `>` or `>=` (secret types omit Eq and Ord).

These are source typing rules. They do not show that the machine code is
constant time, realtime safe or bit deterministic; that is the Stage 5
verifiers' job (lane 24), and Rust ownership in the host libraries is not
evidence for any of them.

### Public API

- `vesper::capability_effects::{CONST_TIME, REALTIME, DET_FLOAT,
  SECRET_TYPES, REALTIME_FORBIDDEN, POLICY_SCHEMA}`.
- `Capability::{ConstTime, Realtime, DetFloat}` with `ALL`, `from_name`,
  `name`; `is_capability_effect(&str)`, `capabilities(&Effects)`,
  `is_declassify_gate(&str)`.
- `capability_policies(&Checked) -> Result<CapabilityPolicies, PolicyError>`.
  `PolicyError::Unchecked { diagnostics }` refuses programs with diagnostics.
  `CapabilityPolicies` (`schema = "vesper.capability-policies/1"`, serde
  `Serialize`) holds plain data, no dependency on the verifier crates:
  - `const_time: Vec<ConstTimeInput { function, span, secret_params,
    secret_result, declassify_gate, constant_time_callees,
    declassify_callees }>` for Vault's `CtPolicy` (`secret_params`,
    `constant_time_callees`, `declassify`);
  - `realtime: RealtimeInput { functions: Vec<RealtimeFunction { function,
    span, root, effects, stack_budget_bytes }>, calls, forbidden_effects,
    reject_unbounded_loops }` for Resonance's `RealtimePolicy` roots
    (`root` is true when no other realtime function calls it);
  - `det_float: Vec<DetFloatInput { function, span, allowed_float_calls,
    allow_fma }>` for Kinetic's `DetFloatPolicy`.
  Parameter indices are source positions; mapping them to Cranelift block
  parameters and callee names to Cranelift `ExternalName`s belongs to the
  backend hook (integration B, lane 24).

## Checks

- `cargo test --locked --test capability_effects`: 19 passed (seven
  regression tests for the first review round). Accepted
  fixtures check and produce the same value under `vesper run` and `vesper
  jit`; every rejected fixture reports its expected code through the library
  and through `vesper check`, with a span inside the code; every rule has a
  fixture; examples/ still check; `vesper fmt --check` is clean on the fixture
  directory.
- `cargo fmt --all -- --check` and `cargo clippy --workspace --locked
  --all-targets -- -D warnings`: clean.
- `cargo test --workspace --locked` on the first head (`00497a3`): 3,339
  passed, 0 failed, across 379 test binaries.

## Decisions

- **Callers are not asked for the capability.** Asking would force `main` to
  carry it, and no handler may discharge it. The design enters a capability
  with `ConstTime::scope` or `@realtime`; there is no attribute or scope
  syntax yet, so a call from plain code is the entry. A capability on the entry
  function itself is rejected by the backends as today ("entry must handle all
  effects").
- **Diagnostics E0700-E0731** are a new block; existing codes were unchanged.
- **Checking order.** The capability pass runs only on programs that already
  check, so it never reports on top of type errors, and Place-IR validation
  still runs afterwards.
- **No new built-in types.** Secrets are recognised by declared ADT name, as
  in §23.2, so `World.adts` and every emitted artifact (interface, Lean, IR)
  are unchanged for existing programs.
- **Realtime denylist, not allowlist.** User effects are allowed unless named
  by the design as blocking or allocating. Resonance's Stage 5 allowlist stays
  stricter.

## Remaining gaps

- `@realtime(stack_budget_bytes = .., period_us = ..)`, `ConstTime::scope`,
  `DetFloat::scope` and `DetFloat::fma` have no source syntax;
  `stack_budget_bytes` is always `None`, `allow_fma` always false.
- `ConstDenorm`, `Isa[P]`, `RtBus` and the design's secret operations
  (`ct_select`, `bit_xor`, `load_sec_u64le`) are not built in. The core
  language has no bitwise operators, so constant-time selects are written with
  `wrapping_*` arithmetic.
- Taint is intraprocedural with typed boundaries: a secret may only cross a
  call through a secret-typed parameter. Generic parameters instantiated with a
  non-secret type reject secret arguments; there is no interprocedural
  secret-parameter inference.
- Secrets that flow through a handler's `resume` value or handler state are
  tracked only by type.
- The evaluator (`vesper run`) allocates on its own heap whatever the source
  says; the realtime rules are about the native backend, whose values live in
  stack slots, and about source effects.
- No Lean model or proof of these rules; no backend verification. Lane 24
  runs the Vault, Resonance and Kinetic verifiers on the emitted Cranelift
  functions from `capability_policies`.
