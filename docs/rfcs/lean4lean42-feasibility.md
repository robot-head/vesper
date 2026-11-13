# Lean4Lean 4.24 feasibility evidence (#297)

The coordinator built a temporary Lean4Lean port and completed fresh external
replay of both fixed §42 modules. This snapshot records that experiment and its
limits. It is not a production setup, a general artifact gate, or a claim that
the checker implementation is fully verified.

The current candidate still needs a disposition and controls for later reachable
kernel fixes, including nested parameters, normalized-Prop/projection rules,
constructor free-variable checks, nested auxiliaries and mutual declarations.
The [proof plan](lean4lean42-proof-plan.md) tracks that review. Positive checks
of these two modules do not discharge those gates.

## Exact experiment

The unmodified upstream reference checkout is pinned to
`4faf51403bc4cb35cf6666d91fc9c938ae46d3aa`. A separate temporary worktree applied
the [recorded patch](lean4lean42-feasibility.patch). It pins Lean 4.24.0 and
Batteries `8da40b72fece29b7d3fe3d768bac4c8910ce9bee`, restores the three
declaration-value guards, loads the actual fresh target including private
imports, and adapts access to the private `mainModule?` API.

Two exact upstream fixes are included:

- `d7a52a83bea3e93968672b89ba88af4372b08f23`: remove checker-introduced `And`
  dependencies from primitive reflection while retaining its equation checks.
- `bc9a736be9c318209992e91ef6a218864b817dcf`: correct the reversed normalized
  universe-level comparison while retaining inductive universe checks.

The standard `lake build lean4lean` completed successfully with 53 jobs. The
`Lean4Lean.Verify` proof target was not built by this experiment. Existing
upstream unfinished proof cases remain; building the executable cannot close
them.

These are experimental SHA256 identities, separate from Vesper's production
BLAKE3 artifact scheme:

| Artifact | SHA256 |
|---|---|
| Recorded port patch | `17d11684630a4c7d7aa0981235449313df0e9ec34980041d4c55e201fda4b555` |
| Actual experimental checker executable | `5b02bccb7f020b9fa1bfc02be0a179d921f9ddf4dcf1895b9728253f005925ae` |

The executable hash identifies the observed build; it does not prove that an
arbitrary binary was derived from these sources.

## Actual model and rejection results

The coordinator copied the existing `RewriteWitness.lean` and
`ArithmeticOracle.lean` source bytes to a private regular-file directory and
compiled both with the actual pinned Lean. The external checker then ran
`--fresh` on each module, sequentially, with an explicit sysroot/search path.
The [unmodified result snapshot](lean4lean42-feasibility-results.json) records:

| Module | Actual replayed declarations | Exit | Wall time |
|---|---:|---:|---:|
| RewriteWitness | 84702 | 0 | 173.966 s |
| ArithmeticOracle | 86270 | 0 | 172.626 s |

Both stderr captures were empty. These declaration totals overlap through
shared imports and must not be added as distinct coverage. The upstream replay
filters unsafe and partial constants; this is not evidence for their executable
implementations. Exact inventory matching, target-root participation and
production artifact binding remain contract work, rather than guarantees
inferred from the CLI's generic count.

The coordinator also executed these fixed, handwritten control sources:

- [Universe controls](lean4lean42-universe-controls.lean): the candidate and
  Lean's actual kernel both accept a valid two-field datatype and reject its
  undersized-universe variant. The candidate rejection must come specifically
  from the retained universe check. Exit 0, observed 0.659 s on the final run.
- [Declaration-value controls](lean4lean42-fvar-controls.lean): the exact
  upstream regression from `779c51fdea49c57c60f8039703605faae126b17f` rejects
  cache-primed free variables in theorem, safe-definition and opaque values,
  and accepts all three closed counterparts. Exit 0 with empty output.

These controls do not establish all later kernel-hardening cases. No unpatched
variant of these controls was executed in this checkpoint, so the historical
vulnerability explanation remains a source/upstream finding.

## Failed attempts and remaining integration

The retained logs distinguish compatibility failures from model rejection:
the first two builds failed on private `mainModule?` access; explicit private
opening and application fixed that compilation issue. A first model probe hit
the coordinator's 1 MiB compiler-file cap; a 64 MiB artifact cap allowed both
modules to compile. This was a probe configuration error, not a compiler bug.

Fresh replay initially failed at `Nat.mod` with missing `And.left`; requesting
all private imports alone did not repair the checker-introduced prerequisite.
After the reflection fix, both models reached the `FloatSpec.mk` universe
comparison failure. The level correction addressed that failure without
changing datatype safety flags or removing the universe guard.

Execution used Linux-only experimental watchdogs: 180 seconds per checker,
an 8 GiB address-space cap and a 1 MiB output-file cap. These are prototype
limits, not portable production guarantees. Logs and fixed model artifacts
remain under `/tmp/lean4lean42-model-check-47qniaf5`; the source port is
`/tmp/vesper-lean4lean42-port`, separate from the clean reference checkout.

The next implementation must pin and verify the reviewed source/patch/dependency
manifest, bind the actual target inventory and artifacts, enforce bounded
execution and failure handling, exercise all applicable rejection controls,
and report axiom policy and unfinished implementation proofs separately.
No Vesper compiler, runtime, oracle, Cargo dependency or CI source was changed
by this experiment. It supplies feasibility evidence for the remaining §42
external-checking component, not whole-compiler refinement or full §42 completion.

The port material and copied declaration-value control originate in
[digama0/lean4lean](https://github.com/digama0/lean4lean). The original
[Apache 2.0 license](lean4lean42-upstream-LICENSE) accompanies this snapshot;
the selected source commits and changes are identified above.
