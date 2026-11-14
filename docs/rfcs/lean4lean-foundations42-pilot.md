# Lean4Lean foundation package admission pilot (#402)

The exact patched `Lean4Lean.List` and `Lean4Lean.Level` sources freshly compile,
pass the unchanged complete-package kernel projector, and admit a strict audit
of their actual public generated theorem roots. This clears a package-admission
gate for new proofs. It does **not** prove the normalizer's semantic correctness,
the checker implementation, native equivalence or any new foundation theorem.

The public claim preceded edits. This lane changed only this report in worktree
`lean4lean-foundations42/pilot`, based on frozen #323 head `ad7208b`. No Cargo,
kernel/profile/auditor/projector modification, source renaming, hidden trust-root
exception or proof admission was used. The one Lean compiler lease is released.

## Exact inputs

Both vendored source bytes matched two independent published associations:
the actual fresh profile-2 setup receipt at
`/tmp/lean4lean42-resource-profile2-395/setup-receipt.json`, and the repository's
existing `tools/lean4lean42/source-manifest.json` entries. The selected files are
unchanged between those profiles; no floating fetch or sealed-tree edit occurred.

| Actual source | Bytes | BLAKE3 |
| --- | ---: | --- |
| `Lean4Lean/List.lean` | 176 | `508045af9f1a1590749506718fa43245c035a5d37fd06e7ae97ec9722a4e1027` |
| `Lean4Lean/Level.lean` | 13,744 | `4b3f0b40448d255f3d7d48e03f2251610b73993d13beb6fdd372f0d511538139` |

The actual Lean executable reported 4.24.0, release commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`; its binary BLAKE3 is
`ffb9dc0e90bae095f4dee95346eec8bdfad04204b219ededce15788e55cfce06`.
The actual Vesper CLI is 125,485,192 bytes, BLAKE3
`8959f562e4bcab426643d779ead3ed69800455dd53f9ed05a17b1389fbdbbf71`.
The unchanged 35,773-byte projector has BLAKE3
`bd2a748c9886b06e5419cf5aa01711dc46d8d3da916d9e184b543afaa7c27eb2`.
The strict audit reported auditor BLAKE3
`e3e61372c54ef68ed41ef94a49d299bba79837523a58549eca35275f9a409062`.

The seven selected source/tool/executable input identities were recorded before
compilation and matched after the pilot. A post-pilot comparison also matched
all thirteen compiler/runtime files (486,041,402 bytes) to the sealed receipt.
Hashes associate observed bytes; they do not prove binary derivation or remove
the trusted compiler/parser/host from this operation.

## Genuine compilation and projection

All work used fresh private root
`/tmp/lean4lean-foundations42-pilot-402`. Original package root `original/`
contained the two exact sources and their freshly compiled legacy objects.
Output root `projection/` was absent and disjoint before export.

Compiler commands were pinned Lean `-j1 -M4096 -o Lean4Lean/List.olean
Lean4Lean/List.lean`, then the corresponding `Level` command, with working
directory `original/` and only that directory plus the pinned toolchain library
on `LEAN_PATH`. The unchanged projector ran with toolchain-only initial search
path and a fixed schema-1 JSON request: original module `Lean4Lean.Level`,
output module `Lean4LeanFoundations42Kernel`. Plugins/extensions were disabled
by the existing loader; the entire canonical package was retained as required.

| Actual phase | Exit | Seconds | Stdout / stderr bytes |
| --- | ---: | ---: | ---: |
| Compile List | 0 | 1.056 | 0 / 0 |
| Compile Level | 0 | 5.472 | 0 / 0 |
| Exact full-package snapshot | 0 | 33.645 | 260,774 / 0 |

The snapshot observed 236 original constants and 236 raw occurrences. It kept
223 canonical logical declarations and separately inventoried thirteen matched
partial native compiler auxiliaries. There were zero duplicate occurrences and
zero alternate-proof aliases. The expression walk visited 1,033,911 nodes.
`exact_inventory`, `constant_info_equal`, `kernel_replayed`, `reimport_verified`,
`extensions_empty` and `ir_empty` were all true. No trusted-name shadow,
explicit-unsafe/source-partial, duplicate/private-name or closure blocker arose.
The omitted auxiliary inventory is logical-projection scope, not native-code
equivalence evidence.

| Recorded object | Bytes | BLAKE3 |
| --- | ---: | --- |
| Original `Lean4Lean/List.olean` | 46,120 | `cd5b988c2a98e35be4ef082ef833db517fbcbe34302643b34f271408392be5fd` |
| Original `Lean4Lean/Level.olean` | 1,764,144 | `e614106c91f59021e845972c5b6b65ce709caeb49382d9f1bcf25cb67a0bb839` |
| Projected standalone object | 876,480 | `d0f4f7a2165e98be99e860b2bb1647b09398593dce479e5bde1acdae2d679ec0` |

These object identities were recorded immediately before the public-root audit
and matched afterward. The existing snapshot separately compared original,
replayed, exported and reimported logical `ConstantInfo` inventories exactly.

## Real root-kind refusal, then actual theorem audit

The first strict invocation requested the actual definitions `Node.subsumeBy`,
`Node.subsume`, `subset` and `subsumeVars`. It exited **1** after 16.771 seconds:
`requested name is not an artifact theorem: Lean.Level.Normalize.Node.subsume`.
This is the intended theorem-root gate, not a projection failure. It was not
silenced, relabelled as success or bypassed.

A fixed handwritten metadata enumerator imported the projected object with
`plugins := #[]` and `loadExts := false`, examined actual `.thmInfo` records,
and classified their **typed** name components. It found 73 existing generated
theorems: 41 public ASCII string-component names and 32 private/numeric names.
No display string was split or inverted, and no equation theorem was guessed
or lazily realized. Examples actually present include `subsumeVars.eq_def`,
`normalizeAux.eq_def` and their public generated termination/equation proofs.
Two temporary enumerator attempts failed on reserved-identifier/API syntax;
their diagnostics remain preserved. They changed no logical package artifact.

All 41 public roots were then supplied in one actual `vesper proof-audit`
invocation, with the pinned Lean path, a 180-second CLI budget and explicit
standard allowlist `propext`, `Classical.choice`, `Quot.sound`. It exited **0**
in **19.785 seconds**, with empty stderr and a 1,704,768-byte JSON result:
`status=audited`, `accepted=true`, `kernel_replayed=true`,
`checked_constants=223`, `forbidden_axioms=[]`, `cache_eligible=false`.
The observed axiom union is exactly those three standard axioms; nine roots
have an empty axiom set. No generated aliases existed to add to the root set.

The other 32 private/numeric theorems were included in complete kernel replay
but were **not** individually requested as policy roots through the public-name
CLI. This pilot does not claim a direct all-73-root policy audit, new semantic
proof completion or checker self-verification. Future manually authored proof
roots must be public, and every actual proof alias remains mandatory.

## Bounded local evidence

Compilation/snapshot phases had 300-second deadlines; the audit wrapper had a
210-second outer deadline. Compiler stdout/stderr caps were 64 KiB; snapshot
and audit stdout caps were 4 MiB, with 64-KiB stderr caps. All recorded exits
preceded their deadlines. Controlled environments used only explicit search
paths, pinned tools and one Lean thread.

The private root retains `phases.json`, `request.json`, `snapshot.stdout`,
`snapshot-report.json`, original/projected objects, `Inventory.lean`, actual
`theorem-inventory.json`, the exact 41 roots in `audit-public-roots.json`, the
genuine definition refusal and public-audit JSON, and before/after input/object
identity records. Public audit JSON BLAKE3 is
`fa30bb38b76f2dbc4fec9a2417af1fb7613df91eda1ed8d9d99fb5690a77e000`.
No external Lean4Lean replay or new semantic proof was executed in this pilot.
