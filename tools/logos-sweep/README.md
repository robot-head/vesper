# Logos replay of every Lean model

`tools/logos42` replays three fixed closures with the pinned
[Logos](../logos42/README.md) checker, under a sealed receipt and a tamper
suite. This sweep reuses that same sealed root to replay **every** module under
`lean/` (plus the archived Lean4Lean helper sources they import) and the §11
extraction fixtures in `tests/fixtures/lean_extract` (extracted module plus its
proof script), so that no audited model rests on Lean's own kernel alone.

```sh
# after scripts/ci/prepare-logos42.sh ROOT (or tools/logos42/setup.py)
VESPER_LEAN="$(elan which lean)" scripts/ci/logos-sweep.sh ROOT report.json
# or directly
python3 tools/logos-sweep/sweep.py --root ROOT --driver target/debug/logos42 \
  --vesper target/debug/vesper --lean "$(elan which lean)" --report report.json
```

CI runs it in the BuildBuddy "Test all targets" action, on the VM while the
remote Bazel tests run, and the action fails if the sweep fails.

## What it does

1. `logos42 --verify-setup-source ROOT` checks the sealed root's source
   inventory, which binds the `Export.lean` the sweep compiles.
2. Compiles every module from source with the pinned Lean 4.35.0 (no prebuilt
   objects are accepted), in dependency order and in parallel.
3. `Sweep.lean --plan` partitions the modules into the fewest batches it finds
   whose joint import closures declare each name once (several fixtures each
   define `main`, and every extraction fixture has its own `Vesper` prelude).
4. For each batch, `Sweep.lean` imports the closure with `importAll`, exports
   every declaration that is neither `unsafe` nor `partial` with the pinned
   lean4export, refuses if the exporter visits any other set, and writes the
   exact name/kind inventory. The sealed `logos-kernel --check-manifest` bridge
   checks the whole export with Logos (no trusted prefix) and then requires
   Logos' checked environment to contain exactly that inventory.
5. On the smallest batch a coverage control drops one entry from the inventory;
   the bridge must refuse it.
6. Writes a JSON report with per-batch and per-module counts.

## Declines

Logos can *decline* a declaration on a resource limit (it never accepts it).
The sweep then withholds that declaration and every declaration that uses it,
replays the rest, and reports them as checked by Lean's kernel only. Every
decline must be listed with a reason in `expected-declines.json`; an
unlisted decline, a listed declaration that Logos now accepts, or any Logos
*rejection* fails the sweep.

## What it does not establish

The skipped `unsafe`/`partial` declarations are the compiler's `_unsafe_rec`
implementations of recursive definitions; the kernel-visible definitions are
replayed. Axiom-policy auditing stays with `vesper proof-audit` and the other
audits. The exporter, this adapter, Logos, the Lean compiler and the host
remain trusted; external replay is not a proof of any of them.
