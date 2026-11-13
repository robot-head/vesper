# Lean4Lean fixed machine replay (#299)

Both exact bundled models passed actual native inventory followed by fresh
eligible-closure replay with pinned Lean 4.24.0 and the patched Lean4Lean
executable. Every required root was covered; remaining, pending constructor
and pending recursor counts were zero. All four processes exited 0 with clean
framed EOF. These are observed results, not expected counts substituted for
execution.

The tested Main-only source is checkpoint `7ad944e`, following `f8db295` and
`f48c78e`. Its `replay.patch` SHA256 is
`b9b7a36ce82380000ab9953fb1d8f62d3608bd85a9743a9164f1cf6ce8ca2e71`.
The tested executable SHA256 is
`5168d18b2663772606b05f12034f0752b28aa7c40ef09cdaffd6eaf722941445`.
The adopted contract was `43b1d406`, plus published shared-comparison amendment
`859429c`. The separate #298 helper/control commit is `c54ce0f`.

## Implemented behavior

The fixed argument branch implements framed profile, native inventory,
control-baseline inventory, actual controls and eligible fresh replay. It
imports exactly the fixed target with `importAll := true`, bounds the native
inventory, walks exact tagged Name components before sorting by Lean.Name.cmp,
records origins and owner groups, and checks the required roots. Replay starts
with an empty kernel environment and checks scalar/group declarations through
Lean4Lean. Mutual inductive reconstruction retains the original all-list order;
sorted wire owner IDs do not reorder the declaration.

Replay schedules checked Eq before one genuine quotient initialization and
compares all four generated quotient records. Constructors and recursors are
covered only after exact generated/imported metadata comparison using the same
helpers exercised by the five real metadata controls. Coverage bitsets,
counters and required roots must be complete before done. Main also schedules
the checked Char.ofNat declaration before String's primitive addition. Errors
produce a bounded error frame and a nonzero exit, without success termination.

The patch modifies upstream Main.lean only. No kernel patch, models, Cargo or
existing Vesper compiler source was changed by #299.

## Actual commands and focused checks

In `/tmp/vesper-lean4lean42-replay-worker`:

```sh
LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lake build lean4lean
```

The final executable build passed (55 jobs), using the exact toolchain/kernel
patches, fixed Batteries pin and kernel94 umbrella. Logs for the initial and
successive source builds are `/tmp/lean4lean42-machine-build.log`,
`/tmp/lean4lean42-machine-build2.log` and
`/tmp/lean4lean42-machine-build3.log`. Standard `git apply --check` passed
against the clean upstream base; staged diff whitespace checks passed.

Actual `/tmp/lean4lean42-machine-probe.py` invoked the real executable with
controlled sysroot/search paths and bounded framed pipe reads. Profile passed
(two frames, clean exit 0). Seven malformed argument vectors and nonempty stdin
failed closed. Controls mode ran the real 94-case suite, matched every ID,
expected reason and actual outcome against the fixed registry, and passed
(97 frames, 7,876 stdout bytes, exit 0). The log is
`/tmp/lean4lean42-machine-probe.log`.

Actual Name helper checks passed 16 cases, including anonymous/empty names,
UTF8 component limits, numeric 0/u64MAX/u64MAX+1, 128/129 components,
4,096/4,097 binary bytes, dotted versus nested identity, numeric versus string
identity, and JSON escape roundtrip:

```sh
LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lake env lean /tmp/lean4lean42-name-controls.lean
```

The final log `/tmp/lean4lean42-name-controls.log` records
`NAME CONTROLS PASSED 16`. The first probe used two incompatible probe-only API
choices (String.ofList and Except equality); correcting the probe to actual
4.24 APIs preceded this successful execution.

Native MODE0/TARGET2 passed with 139,422 constants, 1,584 modules and 10,335,275
typed-name bytes (141,010 frames, 15,879,202 stdout bytes). Its genuine module
paths resolved to 6,261 unique regular artifact files totaling 1,463,175,880
bytes, including base/server/private/IR companions. This exposed the original
4,096-file default as insufficient; the coordinator owns the published default
and Rust resource accounting changes. Raw frames and the module/artifact list
are `/tmp/lean4lean42-control-inventory.frames`,
`/tmp/lean4lean42-control-modules.json` and
`/tmp/lean4lean42-control-artifact-counts.json`. This native binding check is not
fresh replay of the control baseline.

## Complete model replay evidence

Both sources were copied from the bundled files and compiled in
`/tmp/lean4lean42-replay-validation` with the actual pinned compiler:

```sh
LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -o RewriteWitness.olean RewriteWitness.lean
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean42-replay-validation /home/agent/.local/share/vesper-lean/bin/lean -o ArithmeticOracle.olean ArithmeticOracle.lean
python3 /tmp/lean4lean42-full-replay.py
```

The script sequentially invoked `lean4lean --vesper42 MODE TARGET TOKEN` for
native/fresh modes 0/1 and targets 0/1, with closed stdin, the private model
path, pinned sysroot and one Lean thread. It imposed a 65,536-byte frame cap,
32 MiB transcript cap per process, and 1,800-second process deadline. No
ArithmeticOracle server main was executed.

| Observed result | RewriteWitness | ArithmeticOracle |
| --- | ---: | ---: |
| Imported modules | 800 | 820 |
| Native constants | 87,495 | 89,237 |
| Eligible constants covered | 86,647 / 86,647 | 88,337 / 88,337 |
| Unsafe-only / partial-only / both skips | 85 / 763 / 0 | 86 / 814 / 0 |
| Genuine additions | 84,699 | 86,267 |
| Coverage events | 86,644 | 88,334 |
| Constructor comparisons | 1,178 | 1,252 |
| Recursor comparisons | 767 | 815 |
| Genuine quotient group events | 1 | 1 |
| Target constants covered | 179 | 368 |
| Required roots covered | 33 / 33 | 37 / 37 |
| All remaining/pending counts | 0 | 0 |
| Native seconds | 1.785 | 2.338 |
| Fresh replay seconds | 186.391 | 197.303 |
| Fresh stdout bytes | 11,172,743 | 11,382,333 |
| Fresh frames | 175,791 | 179,295 |

The independent streaming Python validator reconstructed inventory counts,
typed Name identities/bytes, origins, forms, skips, owners and required roots.
It compared the exact native/fresh inventory frame identity, checked unique
eligible coverage IDs, contiguous event sequence, action/group consistency,
owner coverage, Eq-before-quotient and one four-ID quotient event, terminal
counters, clean EOF and exit 0. It did not parse human counts or require golden
addition totals. Compiler source/olean and checker SHA256 identities were
unchanged before and after execution.

Native/fresh inventory SHA256 values were respectively identical:

- RewriteWitness: `130da4b1578c6c43554bd49973b426df53900e78c1da7ac652b8352c47e566d5`.
- ArithmeticOracle: `35b8c50076af2c0f8c25ab13cc82bc35cf44bb395aaa278536e2a64c9b654457`.

The validation directory retains `inputs.json`, `results.json`, all four raw
`.frames` transcripts, summaries and empty stderr files, plus both compiler
logs. The overall execution log is `/tmp/lean4lean42-full-replay.log`.
These private SHA256 checks are inspection evidence; the production Rust
BLAKE3 artifact sealing and its accounting belong to the integration lane.

## Corrected failed attempt and limits

The first actual fresh target0 attempt on `f48c78e` failed after 56 successful
additions with `[1,10,"checker","unknown constant: Char.ofNat"]`, exited 1 and
emitted no done. Target1 was not started after that failure. Evidence remains in
`/tmp/lean4lean42-replay-validation/failed-f48c78e`. String's primitive support
needs Char.ofNat although its stored constructor type does not mention it.
`7ad944e` restores this explicit prerequisite through genuine checked replay;
it neither preloads an unchecked declaration nor weakens the primitive check.
The later complete positive executions above use this correction.

Evidence is from Linux x86_64 and these exact bundled models. Unsafe/partial
constants are explicitly inventoried skips. This lane did not run Cargo,
production Rust receipt validation or setup integration, build Theory/Verify,
or establish axiom policy, implementation verification, full compiler
refinement, process RSS/resource traces or whole §42 completion. The full replay
lease was released after the two successful fresh runs.
