# Local Lean artifact audits

`proof-audit` kernel-replays existing local Lean 4.35.0 artifacts and checks named theorems against an explicit axiom policy. It does not compile Vesper, establish compiler preservation or termination, publish a certificate, or approve a package cache entry.

Compile a local proof module with the pinned toolchain, then audit its artifact:

```sh
mkdir -p /tmp/vesper-proofs
cp lean/Foundations.lean /tmp/vesper-proofs/Model.lean
(cd /tmp/vesper-proofs && lean -o Model.olean Model.lean)
cargo run -- proof-audit Model --root /tmp/vesper-proofs \
  --theorem Vesper.Foundations.join_comm \
  --theorem Vesper.Foundations.linear_exactly_once --json
```

`--root` names an artifact directory, not an arbitrary Lean search path. The command copies supported `.olean`, `.olean.server`, `.olean.private` and `.ir` files into private staging before checking them. It never elaborates the directory's `.lean` source files. Module/theorem names use qualified ASCII identifiers, and entry modules cannot occupy the toolchain's `Init`, `Std`, `Lean` or `Lake` namespaces.

The generated auditor is elaborated using only the selected toolchain. It imports artifact data without plugins or package environment-extension initialization, constructs a separate base containing only trusted toolchain declarations, and checks every artifact declaration with Lean's kernel. Requested names must be actual artifact theorems. Replayed constants are checked against the original metadata, including reconstructed inductive constructors and recursors. Version 1 rejects artifacts containing unsafe or partial declarations instead of claiming those skipped declarations were checked.

The trusted base loads the artifact’s direct toolchain imports and their normal transitive closure. Lean 4.35 may store different proof realizations of a trusted theorem in separate toolchain modules; their theorem headers must agree. Artifact declarations retain exact comparisons, and raw package entries may never replace a trusted name.

Plain import, an empty axiom list, and `--trust=0` alone are insufficient for this purpose. A deliberately forged `.olean` can pass those steps while containing a false theorem. Kernel replay and the separate trusted base are required, and panic output is rejected even when Lean exits with status zero.

## Axiom policy and reports

The default allowed-axiom set is empty. `--allow-axiom Example.assumption` explicitly admits that named assumption; it remains visible in the theorem's transitive axiom list. `sorryAx`, `Lean.ofReduceBool`, `Lean.trustCompiler`, and Lean 4.35-generated axioms with a `_native` namespace component are always forbidden, including explicit whitelist attempts. This acceptance is conditional on the reported assumptions and trusted Lean toolchain, not a claim of unconditional truth.

JSON reports include schema version 1, acceptance, kernel-replay status, theorem types/axioms, checked-constant count, the copied artifact inventory and its BLAKE3 digest, actual resolved import parts/digests, the auditor digest, and the real Lean binary version/digest. The inventory records implementation inputs, so changing an imported definition changes it even when theorem statements remain unchanged. Type representations are diagnostic Lean expressions, not canonical public interface hashes.

Every report has `cache_eligible: false`. The command does not establish namespace/source provenance, bind the full native toolchain/OS environment, authenticate a distribution, or produce the specification's complete `ProofArtifactV1`. These local audit reports must not be used as reusable package proof certificates. Source elaboration, compiler-correctness proofs and trusted registry publication remain separate work.

## Execution limits

Use `--lean PATH` or `VESPER_LEAN` to select Lean; the command resolves the real toolchain binary rather than hashing an elan proxy. Only Lean 4.35.0 is supported by this helper. `--timeout N` bounds the Lean checking process to 1–300 seconds (default 30). Checking runs with one worker, an 8 GiB Lean memory limit and bounded output capture. Artifact snapshots are limited to 128 MiB, 4,096 artifact files, 16,384 directory entries and depth 64; symlinks are rejected.

The selected toolchain, kernel, artifact deserializer and host OS remain trusted. This command is not an OS sandbox or a safe way to execute arbitrary downloaded proof packages. Input directories are expected to be caller-owned, without hostile concurrent writers. Standard-library import fingerprinting also incurs local file I/O outside the checking-process timeout.

Successful policy checks exit zero; unapproved axioms exit nonzero with the completed audit report. Replay, missing-artifact/name, unsupported-artifact, toolchain and timeout failures exit nonzero; JSON mode emits an `audit_failed` error. Argument validation errors use stderr. Staging is removed after the process ends, including failed checks.

## Binding extracted contracts to proofs

`vesper verify program.ves --extract` compiles an extracted Lean model. Its `contracts` list inventories emitted propositions; a bare invocation does not prove those propositions. To check them, supply one `--contract-proof Function=Theorem` binding and one `--contract-witness Function=Theorem` for every emitted contract:

```sh
vesper verify program.ves --extract --proofs proofs.lean \
  --contract-proof answer=Proofs.answer_correct \
  --contract-witness answer=Proofs.answer_input \
  --allow-axiom propext --allow-axiom Classical.choice --allow-axiom Quot.sound \
  --lean /path/to/lean --json
```

For recursion, verification uses a total proof projection: original recursive executable definitions and their transitive callers are omitted, and fuel companions are noncomputable kernel definitions without executable recursion helpers. JSON identifies this scope as `source_lean_fuel_projection` and lists `omitted_executable_definitions`. The model hash binds the projected text, including on cache hits. The full executable model remains available through `vesper emit --emit lean`; the projection does not establish equivalence to those omitted definitions. The auditor still rejects unsafe declarations supplied by proof scripts.

For a source function `answer` with an `ensures_answer` predicate, the selected theorem must prove `Vesper.«answer$contract»`. Once any contract binding is supplied, missing or unknown contracts are rejected. The compiler generates a wrapper for each binding; after kernel replay the auditor inspects its actual expression type, requiring exactly the emitted contract constant with no extra hypotheses. A theorem of `True`, an extra conditional premise, or an implicitly abstracted section instance cannot satisfy this check. The JSON `checked_contracts` map records accepted bindings separately from the proposition inventory.

For supported functions, `requires_f` takes the original inputs. `ensures_f` takes those inputs, the return value (including unit), then the final values of all `inout` parameters in declaration order. For example, `replace(inout x: u64, value: u64) -> u64` uses `ensures_replace(before: u64, value: u64, result: u64, after: u64) -> bool`. The post-state comes from the extracted function's returned state tuple. See the [inout contract report](parallel-results/lean-extractor-inout-contracts.md) for examples and effect restrictions.

For type-generic functions, the contract universally quantifies the type parameters before the value parameters. Requires/ensures predicates must declare matching type parameters and mode bounds in order; their names may differ. Explicit instantiation also supports phantom type parameters. A proof for only one concrete type cannot satisfy this universal contract. See the [generic contract report](parallel-results/lean-extractor-generic-contracts.md) for examples and restrictions.

The input witness must prove `Vesper.«answer$admissible»`: an existential over every source type and value parameter, satisfying `requires_answer` when present (otherwise `True`). For a generic function this establishes one admissible type/input combination, not inhabitation of every type. A zero-parameter function must establish its precondition directly. The same exact-type and no-extra-hypotheses checks apply to these witnesses, recorded in `checked_preconditions`. Missing witnesses and contradictory preconditions cannot pass explicit contract admission. The axiom policy for this mode is restricted to subsets of `propext`, `Classical.choice`, and `Quot.sound`; the general standalone auditor still supports explicitly declared mathematical assumptions.

These are partial-correctness proofs of the extracted model with inhabited preconditions, under the reported standard axioms. Input inhabitation does not prove that the function terminates or returns successfully. The checks do not establish termination of fuel-based models, the correctness of extraction, or native executable behavior. Proof-script elaboration uses the trusted Lean toolchain and can execute metaprograms; it is not an OS sandbox.

`--olean-store` records artifacts in the local CAS. A cache hit must match a fresh compilation of the current source/proof bundle before kernel replay of the fresh artifact closure. CAS integrity alone does not prove that a valid artifact belongs to its claimed source. Cache hits therefore still incur compilation and checking.
