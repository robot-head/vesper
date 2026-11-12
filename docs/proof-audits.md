# Local Lean artifact audits

`proof-audit` kernel-replays existing local Lean 4.24.0 artifacts and checks named theorems against an explicit axiom policy. It does not compile Vesper, establish compiler preservation or termination, publish a certificate, or approve a package cache entry.

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

The generated auditor is elaborated using only the selected toolchain. It imports artifact data without plugins or package environment-extension initialization, constructs a separate base containing only trusted toolchain declarations, and invokes Lean's `Environment.replay` for the artifact's declarations. Requested names must be actual artifact theorems. Replayed constants are checked against the original metadata, including reconstructed inductive constructors and recursors. Version 1 rejects artifacts containing unsafe or partial declarations instead of claiming those skipped declarations were checked.

Plain import, an empty axiom list, and `--trust=0` alone are insufficient for this purpose. A deliberately forged `.olean` can pass those steps while containing a false theorem. Kernel replay and the separate trusted base are required, and panic output is rejected even when Lean exits with status zero.

## Axiom policy and reports

The default allowed-axiom set is empty. `--allow-axiom Example.assumption` explicitly admits that named assumption; it remains visible in the theorem's transitive axiom list. `sorryAx` is always forbidden. This acceptance is conditional on the reported assumptions and trusted Lean toolchain, not a claim of unconditional truth.

JSON reports include schema version 1, acceptance, kernel-replay status, theorem types/axioms, checked-constant count, the copied artifact inventory and its BLAKE3 digest, actual resolved import parts/digests, the auditor digest, and the real Lean binary version/digest. The inventory records implementation inputs, so changing an imported definition changes it even when theorem statements remain unchanged. Type representations are diagnostic Lean expressions, not canonical public interface hashes.

Every report has `cache_eligible: false`. The command does not establish namespace/source provenance, bind the full native toolchain/OS environment, authenticate a distribution, or produce the specification's complete `ProofArtifactV1`. These local audit reports must not be used as reusable package proof certificates. Source elaboration, compiler-correctness proofs and trusted registry publication remain separate work.

## Execution limits

Use `--lean PATH` or `VESPER_LEAN` to select Lean; the command resolves the real toolchain binary rather than hashing an elan proxy. Only Lean 4.24.0 is supported by this helper. `--timeout N` bounds the Lean checking process to 1–300 seconds (default 30). Checking runs with one worker, a 4 GiB Lean memory limit and bounded output capture. Artifact snapshots are limited to 128 MiB, 4,096 artifact files, 16,384 directory entries and depth 64; symlinks are rejected.

The selected toolchain, kernel, artifact deserializer and host OS remain trusted. This command is not an OS sandbox or a safe way to execute arbitrary downloaded proof packages. Input directories are expected to be caller-owned, without hostile concurrent writers. Standard-library import fingerprinting also incurs local file I/O outside the checking-process timeout.

Successful policy checks exit zero; unapproved axioms exit nonzero with the completed audit report. Replay, missing-artifact/name, unsupported-artifact, toolchain and timeout failures exit nonzero; JSON mode emits an `audit_failed` error. Argument validation errors use stderr. Staging is removed after the process ends, including failed checks.
