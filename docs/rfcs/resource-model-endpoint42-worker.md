# Executable finite-model worker (#479)

Implemented the fixed handwritten `lean/ResourceOracle42.lean` endpoint under
`resource-model-endpoint42-contracts.md`, including the explicit 512-byte complete
BYE-frame reserve from contract clarification `a51d5f2` (local cherry-pick
`03899d5`). Existing model and metatheory sources are unchanged.

The dispatcher calls the actual existing definitions in order:
`validate → checkProgram → validateInvocation → evalSteps`. Its other code is
bounded data decoding, serialization and IO. It does not implement a replacement
checker/evaluator or accept expected results, proof flags, generated Lean source,
module selectors or callbacks. Only `ResourceModel42.Evaluator` and
`Lean.Data.Json` are imported. There are no new logical proof claims.

## Wire implementation

The worker implements the frozen eight-field request and all six result stages,
exact constructor spellings/payloads, READY bounds12, sequential numbering and
partitioned BYE counters. CheckError starts with its actual kind spelling.
Naturals use canonical strings up to 20 digits, preserving genuine over-u64
model refusals. Zero and over-hard model limits are not clamped. Runtime handle
inputs remain representable and are rejected by actual invocation admission.

Framing and lexical admission precede JSON parsing. Before constructing any
model tree, all code/input row counts are computed and bounded. Postfix stacks
consume children once, enforce atom operands, preserve child order, check depth
and end with exactly one result. Limits cover JSON bytes/depth/tokens, scalar
syntax, per-code/total rows, inputs and constructed depth.

Before payload allocation or model execution, each request prepays its complete
frame, a maximum response frame and the fixed 512-byte BYE reserve. READY,
responses and BYE count actual framed bytes. Output overflow and malformed,
truncated, over-budget or out-of-sequence input fail without a model result.
Fatal stderr is a fixed bounded reason; untrusted text is not echoed.

Evaluation serializes actual chronological events, live residuals, the full
ledger including tombstones, completed values and `result.actions.length`.
Refusals before execution have no execution payload. Returned owners retain
live residuals, and failures preserve actual prefixes without cleanup events.
Stopped expression snapshots are outside the schema.

## Actual builds and artifacts

Final isolated, frozen artifact root:
`/tmp/resource-model-endpoint42-worker-v3`.

`manifest.json` records all ten original project source/object identities and
all evidence-file sizes/SHA256/BLAKE3. Its BLAKE3 is
`55d79a3c4385d40568ac1a6f180045af75027e69b203548fe7eae2a6fd939b4d`
(11,313 bytes; SHA256
`24ec49856abdb7b9c7ce1d19828dda31b1b37d2d9a895bf372428c4da753967f`).
Before/after identity records additionally cover 1,897 toolchain/runtime files:
all toolchain `.olean` files, Lean shared libraries, the compiler executable and
resolved libraries reported by `ldd`. These records are equal. Source copies
were compared against regular, nonsymlink worktree files before construction
and again after execution.

Pinned compiler:
`/home/agent/.local/share/vesper-lean/bin/lean`, Lean 4.24.0,
commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`.
All five final commands exited zero, with one thread and `-M4096`. From the
private root, for each module in the following order, the actual command was:

```text
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o MODULE.olean MODULE.lean
```

| Module | Exit | Elapsed seconds | Compiler output bytes |
| --- | ---: | ---: | ---: |
| MetatheoryFull | 0 | 13.080 | 11746 |
| ResourceModel42/Syntax | 0 | 2.707 | 0 |
| ResourceModel42/Checker | 0 | 0.876 | 0 |
| ResourceModel42/Evaluator | 0 | 1.458 | 0 |
| ResourceOracle42 | 0 | 1.804 | 0 |

The final environment was exactly `PATH=/usr/bin:/bin`, `LANG=C.UTF-8` and
`LEAN_PATH=/tmp/resource-model-endpoint42-worker-v3:/home/agent/.local/share/vesper-lean/lib/lean`.
Compiler output was read through a bounded pipe (1 MiB), with a 180-second
per-module deadline. Each actual output object was checked to be at most 64 MiB.
`compile-results.json` and `build-v3.py` preserve commands and outcomes.

| Project file | Bytes | BLAKE3 |
| --- | ---: | --- |
| MetatheoryFull.lean | 117218 | a982ff27e761a06abed814356757e72b74bc50229cb3d00b5ee7561e30268a1f |
| MetatheoryFull.olean | 13455096 | f1a7ad2f84d789107f1fa7b4447b624af0a745126fdbc2fc24267cf896aea8a4 |
| ResourceModel42/Syntax.lean | 19712 | 74e234aea0203044d2d5159fba90c90d39279cd126515f3bdf8fe176e0147dae |
| ResourceModel42/Syntax.olean | 3257760 | 80978781b5feb67b5f136a8517bb28ca905820cc2507d689aa371eb90d2b8873 |
| ResourceModel42/Checker.lean | 9026 | 6d76de5e04384e4239bb9836bdc24b3f1846e63b16f7c13b5faac93ee4f5a2e4 |
| ResourceModel42/Checker.olean | 757568 | 2a7368b13eb0dd34d5b783dd4894d7bd5ad61a749ef2433250397765225cc74f |
| ResourceModel42/Evaluator.lean | 11133 | 8c82416335a00edbe7a3d8a1b9b1803c4233e1da8f8768b0be3a2c839ed3896b |
| ResourceModel42/Evaluator.olean | 1462904 | 7f822144363d11012047e1ceae87c0e81b7cb770cf65e35a8f1662f1b3d02790 |
| ResourceOracle42.lean | 16855 | 7381930f329b5feeeb3c26dd3f3bb1301dc8641bcea497663ea653aed14c00e5 |
| ResourceOracle42.olean | 1744976 | 134f2b53ea1e9674a4de669b8c7e128ca0d3cbdf56b8adb1c9df958f8ce1d1d9 |

Worker source SHA256:
`3d6e4f99a46e51e95511a4fd69a993a86167a32d2002b12fa31c4996d7b31e41`.
Worker object SHA256:
`e32be05f058d316c417dc119a8788fe1c133a2e629309181cddd16b8a5a98fe8`.

Initial v1 compilation and probes succeeded. The first attempt at a uniformly
bounded final rebuild, v2, mistakenly applied a 1 MiB `RLIMIT_FSIZE` to the
compiler process; this also capped its 13 MiB MetatheoryFull object and caused
SIGXFSZ (exit -25) after 12.529 seconds. No model or worker source diagnostic
caused this failure. The failed command/result/log are retained. V3 corrected
the harness to bound the output pipe separately and rebuilt every original
module successfully. All ten v1/v3 project source/object files are byte-identical.
No object from the failed root or a kernel projection was reused.

## Genuine framed execution

Actual executable command in each session:

```text
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 --run ResourceOracle42.lean
```

Python framed probes against original executable artifacts passed:

- 18 baseline model cases cover all six stages, terminal fuel zero, maximum u64,
  genuine out-of-range literal, unknown variable, zero type bounds, invalid model
  limits, shape/handle invocation errors, independent fuel admission, live ticket
  return, mint/consume, tombstone ID freshness, both branch values, both unpack
  binder indices, nonterminal fuel exhaustion and a genuine limited live prefix.
  Changing invocation values preserved identical static checked data.
- 11 baseline hostile sessions cover zero/oversized frames, truncated header and
  payload, forbidden objects, noncanonical controls/naturals, sequence mismatch,
  too-long natural strings, postfix underflow and nonatom pair operands. Each
  exited nonzero with no result or BYE and bounded stderr.
- 17 additional controls cover preparse depth/token quotas, row/input/total-row
  quotas, constructed-depth quota, exact arity/tags, fragmented complete IO,
  actual unusedLinear/duplicateUse/branchOwnership/affineBranchDemand checking,
  session wire prepayment, exact 4096-request retirement and 4097th-header refusal.
  The wire-budget control completed 15 actual maximum-sized valid frames, then
  rejected the next header before reading a payload. The request-count controls
  use repeated unit fixtures, not a source or native campaign.
- A final v3 clean-environment smoke actually completed unit, live owner and
  mint-consume-mint requests, followed by exact BYE, EOF and exit zero. The full
  direction + little-endian length + payload transcript is retained and hashed.

`v1-probe-results.json`, `v1-extra-probe-results.json`, `v3-smoke-results.json`,
probe scripts, transcript files and identity manifests are in the frozen root.
`git diff --check` passed. No Cargo command was run by this author.

The worker is transport code exercising the existing finite logical model.
These results do not establish source adaptation, Rust diagnostic agreement,
native observation, compiler refinement, a 100,000-program resource campaign,
new kernel proofs or external verification of executable implementation helpers.
The compiler lease has been released; no Lean process from this author remains.
