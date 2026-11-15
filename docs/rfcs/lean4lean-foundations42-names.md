# Core fresh-name proof packet (#405)

`Lean4LeanNames42.lean` imports only `Lean` and proves twenty-two public facts
about the actual pinned Core `NameGenerator.curr` and `next` operations. It
imports neither the unsafe Expr utilities nor the old Theory/Verify assumptions.
The public source contract is `lean4lean-foundations42-contracts.md` at
`80b81cf`; this lane changes only its new module and this report.

The six reservation/order leaf facts preserve the useful content of the old
Verify/NameGenerator packet, using a namespaced `Extends` predicate rather than
installing a global `LE NameGenerator` instance. Core `curr` is `Name.num` of the
prefix and index; `next` increments that index. These facts cover arbitrary
prefixes and indices, including the checker's explicit `_kernel_fresh`/zero
initial generator, rather than assuming Core's default index is zero.

`advance` iterates the actual `next`; `nameAt` reads its actual `curr`; `emitted`
lists those observations over a finite range. The proofs establish exact
prefix/index equations, extension and reservation monotonicity, injective
generated IDs, non-aliasing with prior reserved IDs, reservation after the
sequence, membership, length and Nodup. The two-step equality exposes the
actual consecutive Core calls. No replacement allocator or unrelated ID model
supplies the result.

`Reserves` is intentionally vacuous for IDs outside the generator's prefix.
The proved freshness is conditional on the caller's reservations. This packet
does not establish context/cache priming, whole-checker correctness, native
equivalence, compiler refinement or resource UID properties.

## Actual compilation

The first genuine compilation passed with empty stdout/stderr and exit zero,
in 0.764 seconds as measured by the command runner:

```text
LEAN_PATH=/tmp/lean4lean-foundations42-names-v1:/home/agent/.local/share/vesper-lean/lib/lean
LEAN_NUM_THREADS=1
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanNames42.olean Lean4LeanNames42.lean
```

The command ran from the fresh immutable dependency root
`/tmp/lean4lean-foundations42-names-v1`. The copied source byte-matches the
worktree module. Its two recorded artifacts are:

| File | Bytes | BLAKE3 |
| --- | ---: | --- |
| Lean4LeanNames42.lean | 4,735 | f9916caa2e359f0d360e91977c41c25cd65c8f31c7a24edb19fe1f6677bb92cb |
| Lean4LeanNames42.olean | 186,056 | eb14e70a7e57be3f7e71b29a143b43df501dbd09be8d7f6c12e1cdeef62ed04b |

SHA256 associations are retained alongside these values in
`/tmp/lean4lean-foundations42-names-v1-manifest.json`. Compilation kernel-checks
the authored proofs. The separate complete-package projection, alternate-proof
association, strict transitive axiom audit and adversarial acceptance remain
the coordinator's integration gate; this lane does not claim they have run.
No Cargo invocation occurred, and the compiler lease was released immediately
after successful compilation. Whitespace verification passed.
