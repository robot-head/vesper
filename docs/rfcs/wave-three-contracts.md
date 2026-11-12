# Third-wave integration contracts

Baseline: `02b3a1d175efb5d2561b7fa5b0aaf80121ae1bc6` or a descendant. All seven prior handoffs are merged and
registered. Checked arithmetic is `vesper.arith.checked/1`, interface semantics
version is 2, and fiber ABI is 2. This is an ownership/compatibility agreement,
not a new soundness theorem.

## Frontend to existing core

The source-traits lane owns additive surface declaration metadata, syntax,
checker/elaboration and interface export. Existing `Type`, `ExprKind`, Place-IR
`Op`/`Terminator`, value layouts and arithmetic/failure semantics stay fixed.
New surface projections/model selection must elaborate to existing core types
and ordinary resolved calls, or fail before an accepted `World` reaches a backend.
Do not smuggle unresolved traits into fake nominal types or runtime name lookup.
Generated implementation functions need explicit frontend identity/visibility
metadata; their bodies must not accidentally become new public source exports.

`vesper-pact-bridge` depends on root `vesper`. Root must not depend on that bridge.
The frontend lane may consume root-free Pact directly or extract a root-free
binding/lowering layer while keeping compiler wrappers above root. This choice
is its implementation work; dependency cycles are never an integration exception.
Current public interface DTOs do not describe traits/models: use Unsupported for
unrepresentable contracts rather than silently certifying them. Richer SemVer
schema changes remain coordinator-owned.

## Native parking to fixed ABI

The source-native lane owns `src/native.rs` and new source-session helpers.
It consumes checked `FunctionIr`, existing call/capability/effect information and
fiber ABI v2. Source-specific operation binding, suspension layout and data-base
tracking live in backend-private structures; no shared IR or ABI changes are
needed for the initial explicit supported slice. Bindings are validated against
checked operation signatures before machine code is executable.

The platform lane owns fiber implementation/platform gates. It preserves v2
record layouts/offsets, Linux public APIs and one-shot semantics, adding Darwin
direct-fiber support without implying a portable Pulse scheduler. The native
source slice remains Linux. Physical stacks stay pinned; source data-frame
relocation requires fresh post-park bases and the existing DMA eligibility rule.
No runtime boolean establishes source portability/Send.

Any necessary shared schema/API change goes to the coordinator as a small
concrete patch. Finish independent supported work rather than weakening checks.
This freezes the interfaces so lanes 1, 2 and 6 can start in parallel.

## Execution caches versus fresh authority

The macro runner is a new crate, not an option to make the build-script runner
accept macros. Cache bounded untrusted wire results under complete execution
identities. Recreate session scopes/origins and rerun current registry/resolution/
authority gates on every hit. Guest data never authenticates caller provenance.
Approval depends on complete trusted input content, not opaque version labels.
No accepted host scope/splice/evidence object becomes a reusable cache capability.

## Simulation and protocol boundaries

Simulation owns separate model traces/identities while keeping the SimPulse v1
journal read-only. Process crash is not normal source cancellation or linear
consumption; make lost process state explicit. Recoverable owners stay recoverable.

Prism remains socket-independent and fixed-length HTTP/1.1. Connection state,
routing and URI decoding are its own lane; native source/compiler work does not
implicitly supply a server, linear responder typing or async transport.

The SIMD lane proves its explicit block/tail algorithm and bounds model. It does
not certify Cranelift, emitted instructions, CPU feature probes or source ISA effects.

## Coordinator ownership and validation

Root workspace/dependencies/lock, CLI/module registration, existing CI/global
documentation, source Lean extraction (`src/lean*.rs`, `vesper-lean-syntax`) and
the TUF verifier/updater stay with the coordinator. Lane-specific report paths
are exclusive; no other lane rewrites them. Only the SIMD proof lane owns its
new `lean/SimdScanner.lean` files; it does not edit the extractor or proof auditor.

Each worktree uses an isolated build target, especially when root compiler
sources differ. Debug info/incremental caches may be disabled to fit disk limits.
The coordinator will recheck all compiler consumers, full workspace tests,
strict Clippy, real Lean, native execution and available target evidence before
claiming integrated acceptance. Cross-compilation is not platform execution.
