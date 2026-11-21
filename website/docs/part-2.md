# 8 How the compiler sees a program

The parser identifies the tokens in a field assignment. The type checker resolves the field and its type, and the place checker verifies that the caller may write there.

## 8.1 Follow the five stages

The proposed pipeline parses a lossless syntax tree, expands notation and resolves types, then checks places and refinements over control flow. Optimization searches for equivalent expressions while preserving effects. The last stage can lower supported operations to native instructions or translate them into a Lean model. A theorem about the Lean model covers the operations the translation represents.

## 8.2 Find the implementation of a stage

Several compiler stages can share a Rust crate. The checkout’s root compiler includes the checker and Place IR. Run `cargo metadata --no-deps --format-version 1` in the repository to find the actual workspace members before looking for an illustrative crate name.

## 8.3 Represent an operation in high-level IR

High-level intermediate representation (HIR) retains resolved names, ownership modes, and effect rows. A field tree groups projections by their parent. Two leaves under the same broker can denote two writable fields. A lexical effect label identifies the handler scope an operation depends on.

## 8.4 Check modes, ordered obligations, and costs

One helper combines field modes. Another tracks the stack of ordered guards. Others track initialization and resource intervals. For example, exit from both arms of a branch must agree on which guard is topmost. A bound on stack cost uses different composition rules from a bound on allocated bytes.

## 8.5 Follow place permissions across control flow

Place IR divides execution into basic blocks joined by branches. A place capability graph records which locations are owned, lent, initialized, or moved at each point. Joining branches exposes a ticket consumed on only one path. At a possible suspension, check which live values the continuation would carry.

# 9 What a Lean proof verifies

Extraction translates supported Vesper operations into definitions in the Lean proof assistant. A theorem establishes a property of those definitions. To apply it to a native binary, you also need evidence that the compiler preserved the modeled behavior.

## 9.1 Choose a verification boundary

Specify inputs, assumptions, and an observable result. For a buffer update you might require an in-bounds index and promise that other elements retain their values. Ask which allocation, scheduling, and failure behavior the extracted model includes before applying that result to a running service.

## 9.2 Turn mutation into a returned value

A lens pairs a getter with an operation that rebuilds a larger value after changing one part. Extraction can represent a write to a record field by returning the updated record. Mutable parameters become additional outputs. Effects need an explicit model, and a recursive translation of a loop needs a termination argument for a total function. Suggested Lean notation can preserve the shape of a source expression.

Suppose a pair starts as (4, 9) and the callee writes 7 to its first field. The functional result is (7, 9). Reading back the changed field gives 7, and the sibling stays 9. The bounded loan ends at the call, so the caller receives the actual new pair. The chapter calls this translation “prophecy-free” because the model can compute the final value at the call boundary.

## 9.3 Specify what a batch append changes

For a write-ahead log append, name the starting log position and the batch length. The postcondition can relate them to the new position and state which fields remain intact. A transaction’s consuming operation also needs to appear in the model. A proof of the position calculation alone says little about whether a real device persisted the batch.

## 9.4 Admit a compiled proof artifact

A compiled Lean module has the extension `.olean`. The gate replays artifacts and inspects the axioms their declarations depend on, including indirect dependencies. For a cached extraction, it compares against a fresh compilation. Checking a contract also requires the expected theorem signature and an admissible input. Checking the expected signature rules out a valid proof about an unrelated function.

## 9.5 Distinguish extraction, audit, and publication

Extraction starts from a checked source module and produces a Lean bundle. An artifact audit starts from compiled proof evidence. Publication can apply an admission policy to evidence attached to a release. Follow the artifact and identify which gate checked it before treating a proof report as evidence for a particular source contract.

For an existing module called `Model`, an explicit `vesper proof-audit` request names the theorem, such as `Example.correct`, and the directory holding its artifacts. In contrast, `vesper verify --extract` starts with the Vesper file. Both paths need the pinned Lean toolchain. The linked section gives complete command lines and explains the axiom policy.

# 10 Bootstrapping a compiler

Building a compiler with itself tests whether successive builds agree. Independent builds help check where the binary came from. Rewrite witnesses record how to check that an optimization preserved behavior.

## 10.1 Identify the trusted components

The design combines an executable reference model, replayable rewrite evidence, backend validation, a confined self-hosted build, and diverse double-compiling. A reference run can catch a disagreement on a particular input. A simulation theorem relates steps in two models under stated assumptions. For a simulation result, record the assumptions. For a reference run, record the inputs.

## 10.2 Compare compiler generations

Stage 0 is a seed compiler. It builds a compiler from source to produce stage 1. Stage 1 compiles the same source to produce stage 2. Diverse double-compiling repeats the chain from a sufficiently independent compiler and compares the resulting artifacts under reproducible build conditions. Two matching builds from one seed establish a narrower result.

    Seed A + source → compiler A1      Independent seed B + source → compiler B1
    A1 + same source → compiler A2     B1 + same source → compiler B2

Compare A2 with B2 under the same build recipe. The first-stage binaries can differ because the two seeds generated them differently. A second-stage mismatch points to a difference worth tracing through the inputs, compilers, and environment.

## 10.3 Make the driver’s inputs explicit

A hermetic compiler driver reads only the source, tools, and capabilities supplied to the build. Pinning inputs makes a mismatch between generations easier to investigate. For example, embedding the current time can make two builds of the same program produce different bytes.

## 10.4 Relate source steps to target steps

Forward simulation asks whether the target can match an observable source transition. An instruction-selection proof may assume particular operand ranges or a well-formed control-flow edge. A bootstrap fixed point is a statement about repeated compilation under given assumptions. It does not by itself establish instruction correctness.

## 10.5 Replay an optimization witness

A rewrite witness identifies the rule, the matched expression, and evidence for its side conditions. A validator can reject a claimed rewrite that changes overflow behavior or loses an effect. Keep that validation result alongside the output comparison when evaluating a bootstrap claim.

# 11 Packages, authority, and reproducible builds

A package dependency may need permission to allocate, call foreign code, or run a build step. Loom’s package contract records allowed effects alongside version constraints, build inputs, and the identity of the resulting artifacts.

## 11.1 Solve for versions and policies

A resolver chooses versions satisfying all dependency constraints. A policy can add constraints on authority or provenance. If one package needs a newer interface and another pins an incompatible version, a useful explanation identifies the two requirements that conflict.

## 11.2 Follow authority through the build

Capability ceilings limit what a package may request. Sandboxed generators get explicit inputs. An interface fingerprint can spare consumers a rebuild when a private implementation changes. A fast solver handles ordinary dependency constraints. A policy solver can compare several valid choices. Scoped identities identify publishers, quorum review requires agreement from multiple reviewers, and a content-addressed store shares artifacts with identical content.

## 11.3 Read a manifest and trace an import

`Loom.toml` describes the package, dependencies, and declared foreign bindings. A path dependency resolves relative to that manifest. A lockfile records resolved decisions so a later build can use the same inputs. Inspect both when a build works locally but selects different code elsewhere.

Source modules expose public items. `use` brings a name into scope, a qualified path identifies the item explicitly, and `pub use` exposes it through another module. Types and values have distinct namespaces. This is how a library can reorganize implementation modules while keeping a stable public entry point.

## 11.4 Find the command for a package operation

Use the checkout’s `vesper --help` and the relevant subcommand help for available flags. Distinguish resolving source dependencies, building artifacts, auditing evidence, and publishing a release. For a failed release, identify whether the resolver rejected a dependency, the build failed, or the publication gate rejected the evidence.

## 11.5 Retrieve an artifact by content

A content-addressed store keys bytes by their digest. A sandbox controls what a generator can read and produce, while a store index helps locate the resulting artifact. A matching digest establishes identity with the requested bytes. Check who published the artifact and whether your policy permits the capabilities it requests.

# 12 Will a library change break its callers?

An upgrade is source-compatible if an existing caller can still supply the same inputs, rely on the same results, and stay within its permissions and resource budgets.

## 12.1 Compare modes, effects, and field footprints

Narrowing a helper’s writes from the whole broker to its index can free other fields for the caller. Introducing a new effect can make an existing caller lack a handler. Changing a consuming parameter into a borrow also changes who must finish a linear resource. Compare passing modes as well as the types printed next to them.

## 12.2 Check promises in the right direction

A compatible precondition can admit more inputs. A compatible postcondition must still imply what earlier callers relied on. Lowering an upper memory bound can satisfy the old budget. Changing an exact cost can invalidate the contract in either direction. Extending an open enum also requires checking whether the new payload changes the ownership of the whole value.

## 12.3 Find a witness for a breaking change

A witness is a caller or input admitted by the old interface that the new one rejects or handles outside its promise. For example, changing a minimum buffer size from eight to sixteen breaks a caller passing eight bytes. Source substitutability, binary layout compatibility, and transport of proofs each require their own comparison.
