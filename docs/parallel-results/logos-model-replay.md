# Independent Logos replay of every Lean model (COMP-02)

Claim: #843. Tool: [`tools/logos-sweep`](../../tools/logos-sweep/README.md).
CI: `.github/workflows/logos-sweep.yml` (nightly on main, and on PRs touching
Lean, the extractor or the Logos tooling), `scripts/ci/logos-sweep.sh`.

## Gap closed

Since #648, the pinned Logos checker (`tools/logos42`) independently replayed
three fixed closures (`RewriteWitness`, `ArithmeticOracle`, the ResourceModel42
kernel snapshot). The other Lean modules, including every verified-codegen
model, were checked by Lean's own kernel only. The sweep now replays every
module under `lean/` and every §11 extraction fixture with the same sealed
Logos root, nightly and on every PR that can change them.

## Result (Lean 4.35.0, Logos 8ce95bd, lean4export df81913; 2026-11-16)

| | |
|---|---|
| Modules replayed | 158: all 149 under `lean/` and 9 extraction fixtures (`tests/fixtures/lean_extract`, extracted module plus proofs) |
| Batches | 21 (import closures that fit one environment) |
| Declarations accepted by Logos | 1,955,538 across batches (the shared `Init`/`Std`/`Lean` closure is counted once per batch); 26,731 owned by the replayed modules |
| Skipped as `unsafe`/`partial` | 348, all compiler `_unsafe_rec` implementations; their kernel-visible definitions are replayed |
| Logos rejections | 0 |
| Logos declines | 1: `Vesper.VerifiedCodegen.Rv64.decode_encode` (below) |
| Coverage control | passed: dropping one inventory entry is refused by the bridge |
| Time | 25 min on 4 cores with 2 batch jobs; peak memory about 7.7 GB. CI uses 2 batch jobs on a 4-core GitHub runner |

Per-batch figures (export size is the lean4export NDJSON):

| Batch | Roots | Closure modules | Logos-checked | Withheld | Export MiB |
|---|---|---|---|---|---|
| 0 | 56 | 709 | 82,927 | 4 | 489 |
| 1 | 81 (incl. the `import Lean` helpers) | 2,461 | 214,690 | 0 | 880 |
| 2-13 | 1-2 each (fixtures defining `main`, `ReferenceEval`, `ResourceOracle42`, ...) | 654-1,173 | 65,412-120,749 | 0 | 343-611 |
| 14-20 | 1 each (extraction fixtures) | 1,148 | 116,089-116,194 | 0 | 583 |

The full JSON report (per module: checked, skipped and withheld names) is
written by every run; CI prints its summary at the end of the step.

## The one decline

Logos at the pinned revision declines
`Vesper.VerifiedCodegen.Rv64.decode_encode` (`lean/CodegenRv64Encoding.lean`):
"recursion depth limit reached". Its evaluator has a fixed depth limit
(`MAX_DEPTH` in `logicaffeine_kernel/src/dag/sem`), which the RV64
field-extraction arithmetic exceeds. A decline is never acceptance. The sweep
withholds it and its users (`encode_lt`, `emit_decodes`, `bytes_encode`),
replays the other declarations of that module, and lists all four as checked
by Lean's kernel only. It is recorded in `tools/logos-sweep/expected-declines.json`;
the sweep fails on any unlisted decline and on a listed one that Logos starts
accepting.

Follow-up options: restate `decode_encode` so the field lemmas are proved by
rewriting rather than kernel evaluation of large numerals, or raise the limit
upstream in Logos and re-pin.

## Not established

This is an independent kernel replay. It does not audit axioms (that stays
with `vesper proof-audit` and the per-model `#print axioms` checks), does not
prove Logos, the exporter, this adapter, the Lean compiler or the host, and
does not cover proofs that exist only inside Rust tests' temporary files
beyond the nine on-disk extraction fixtures.
