# Lane 24: Stage-5 verifiers from source capability effects

Claim: #266 (wave six, lane 24; §23, §32, §36, §7 Stage 5). Branch
`claude/project-thread-1ekfp6`, based on `main` at `a07fbb8` (integration B,
#295) with lane 15's branch (PR #278, `5c1a0b4`) merged in, because lane 15
had not merged when this lane started; the branch is rebased onto `main` once
#278 lands.

## What was built

`eff [ConstTime]`, `eff [Realtime]` and `eff [DetFloat]` now drive the
integration-B verifier hook. A program with a capability effect is checked at
the backend by `vesper build` and `vesper emit --emit object|clif` **without
any flag**; `--verify=...` and `--verify-policy` still work and are layered on
top.

| Source effect | Verifier | Policy derived from `capability_policies` |
| --- | --- | --- |
| `eff [ConstTime]` | Vault `verify_constant_time` | one `const_time` entry per emitted function: secret-typed parameters (`Secret[T]`, `SecU64`, ...) as `secret`, declassification-gate callees as `declassify`, other (ConstTime) callees as `constant_time_callees` |
| `eff [Realtime]` | Resonance `verify_realtime` over `call_graph_from_clif` | every emitted realtime function is a realtime root with effect `Realtime`; unbounded loops rejected |
| `eff [DetFloat]` | Kinetic `audit` | `det_float.functions` = the emitted DetFloat functions, their DetFloat callees allowed, no FMA; NaN canonicalisation taken from the codegen flags |

Public API (`vesper::native_verifiers`):

- `capability_verifiers(&World) -> BTreeSet<NativeVerifier>`: verifiers the
  program's capability effects imply.
- `policy_from_capabilities(&World, &[EmittedFunction], &NativeVerifierPolicy) -> Result<CapabilityPolicy, String>`:
  the derived policy layered under a base policy (a file entry for a function
  wins), plus `notes` and `findings`.
- `verify_native_with_capabilities(world, entry, target, optimize, requested, base)`:
  compile, derive, verify. With no requested verifier and no capability
  effect it is exactly `native::object_with_options`.

`vesper::native` gained `needs_nan_canonicalization(&World)` and
`object_isa_for(&World, target)`.

Findings keep integration B's mapping: the function is the source function,
and `render` prints `file:line:col` of its item, for example

```
tests/fixtures/capability_verifiers/reject/realtime_fmod.ves:5:1: error[realtime/rt-unknown-callee]: wrap_phase (inst 13): @realtime root `wrap_phase` reaches unknown callee `fmod` with no effect declaration via wrap_phase -> fmod
```

## Two backend changes the verifiers needed

Both are in `src/native.rs`, in their own commit, and only change code for
programs that use capability effects.

1. **Capability-only functions are compiled once.** The native backend used
   to specialize (inline) every function with a non-empty effect row into its
   callers, so an `eff [ConstTime]` function never had a Cranelift function of
   its own and nothing could be verified per function: every lane-15 fixture
   compiled to a single `main`. Capability effects have no operations and no
   handlers, so a function whose row contains only capability effects is now
   compiled like a pure function (`capability_only`). Rows mixing a capability
   with real effects (for example `[Realtime, Fail[E]]`) and functions taking
   or returning function values are still specialized; see the gaps below.
   `vesper run` and `vesper jit` agree on every capability fixture
   (`native_results_match_the_evaluator`).
2. **DetFloat programs are compiled with NaN canonicalisation.** Without it
   Kinetic correctly reports `detfloat.nan-escape` on every DetFloat function
   whose float result escapes, because x86-64 and AArch64 produce different
   default NaN bits; every lane-15 DetFloat fixture was rejected. Object
   emission and the JIT now set Cranelift's `enable_nan_canonicalization` when
   the program has a DetFloat function, and the auditor is told so
   (`codegen_canonicalizes_nans`). The test also audits the same IR with the
   flag off and checks that `nan-escape` is then reported.

## Fixtures and tests

`tests/fixtures/capability_verifiers/` (canonically formatted):

- `accept/consttime_select.ves`, `accept/detfloat_canonical.ves`, plus all four
  lane-15 accepted fixtures (`tests/fixtures/capability_effects/accept`, read
  only).
- `reject/realtime_fmod.ves`: source-accepted (float `%` is legal under
  Realtime); the backend lowers it to a call to libc `fmod`, an import with no
  realtime effect declaration, which Resonance rejects as
  `rt-unknown-callee`. An attested symbol table in a policy file
  (`realtime.symbols.fmod`) lifts it.
- `reject/consttime_generic.ves`: a generic ConstTime function is
  source-accepted, but the const-time parameter mapping needs concrete
  layouts, so the build rejects it explicitly with
  `const-time/capability.generic-unsupported` instead of skipping it.

`tests/capability_verifiers.rs` (9 tests), every object case for both
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`:

- accepted fixtures build and report the implied verifiers;
- rejected fixtures exit 1 with the expected rule at the function's
  `file:line:1`;
- the policy-file override;
- a program without capability effects runs no verifier and emits the same
  bytes as `object_with_options`;
- the derived policy contents (secret parameter names, declassify and
  constant-time callees, realtime roots, DetFloat functions, the note for a
  specialized function, file entries winning);
- the NaN-canonicalisation flag and the auditor's verdict with and without it;
- evaluator/JIT agreement on all capability fixtures;
- `vesper build` on Linux links the accepted program and runs it, and does not
  link the rejected one.

## Checks

Run on Linux x86_64 (cloud host, Rust 1.97):

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -p vesper --locked --all-targets -- -D warnings`: clean.
- `cargo test --locked --test capability_verifiers`: 9 passed.
- `cargo test --locked --test native_verifiers_cli --test capability_effects`:
  8 + 19 passed (integration B's identical-code test included).
- `cargo test -p vesper --locked`: see the PR.
- `vesper fmt --check tests/fixtures/capability_verifiers`: 4 files unchanged.

## Decisions

- Verifiers run automatically, not behind a flag: the point of the
  annotation is that the build refuses code the backend cannot confirm.
  Programs without capability effects take exactly the old path.
- Every emitted realtime function is a root, not only the source roots: a
  source root that was specialized into a caller has no function to start
  from, and checking more roots is never weaker.
- A capability function that has no Cranelift function of its own produces a
  `note:` line saying why (specialized into its callers, or unreachable from
  the entry), since its code is then checked only inside callers that carry
  the same capability.
- Only `Realtime` is passed to Resonance as an effect; ConstTime and DetFloat
  constrain the body and are not Resonance effects.

## What the backend did not find

I looked for source-accepted ConstTime and DetFloat programs the backend
rejects and found none in the current language: the core language has no
loops, bitwise operators, shifts or libm calls, the lane-15 ConstTime taint
is at least as coarse as Vault's (a struct holding a secret field is secret as
a whole), and with NaN canonicalisation on, Kinetic's remaining rules (FMA,
libm, unlisted or indirect float calls, reductions) have no source construct
that produces them. The backend rejections in this lane are therefore Realtime
(`fmod`) and the explicit generic ConstTime limit. The verifiers still run on
every capability function, so a future construct or codegen change that
introduces such a pattern is caught.

## Closures

A capability declared only on a closure (`|x: f64| -> f64 eff [Realtime] { .. }`
inside a plain function) also implies its verifier and, for DetFloat, NaN
canonicalisation. Closures are specialized into the function that contains
them, so that whole function is checked under the closure's capability: as a
realtime root, or audited for DetFloat. That is stricter than the closure
alone and never weaker (`reject/realtime_closure_fmod.ves`). A ConstTime
closure in a non-ConstTime function is rejected as
`const-time/capability.closure-unsupported`, because the container's
signature does not say which values are secret.

An explicit `--verify=detfloat` without a `det_float.functions` list still
audits every emitted function; DetFloat annotations narrow only the automatic
check.

## Source-rule findings fixed in a follow-up

The Codex review of this PR also reported two source-rule holes in
`src/capability_effects.rs` (lane 15's file): assignments to a function-typed
local were not capability-coerced, and a ConstTime handler's state taint was
not propagated into its return clause. Both were fixed on `main` by #311
(closes #310).

## Remaining gaps

- Functions mixing a capability with real effects (`[Realtime, Fail[E]]`) or
  taking/returning function values are specialized into their callers and not
  checked on their own; compiling them once needs the capability-passing ABI
  for real effects in the native backend.
- Generic ConstTime functions are rejected, not checked (instance layouts are
  not exposed by `EmittedFunction`).
- Instruction-level source spans: findings point at the function, because the
  backend attaches no source locations to Cranelift instructions.
- No source syntax for realtime stack budgets (`@realtime(stack_budget_bytes)`),
  so the stack verifier is not implied by `Realtime`; use `--verify-stack`
  with a policy.
- The native batch and session backends (`native_batch.rs`,
  `native_session.rs`) do not set NaN canonicalisation for DetFloat programs.
- The verifiers check Cranelift IR before machine-code generation, not the
  machine code; that is the crates' stated scope.
