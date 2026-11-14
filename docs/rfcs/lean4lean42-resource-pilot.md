# Resource logical-snapshot external replay pilot (#395)

The genuine pilot passed: the existing external Lean4Lean kernel checked the
complete eligible imported closure of the final resource logical projection.
No kernel rule, import, declaration, constructor/recursor comparison or proof
alias was removed to obtain this result. This is a pilot, not a production
profile-2 setup receipt, Rust-driver acceptance result, axiom-policy rerun or
completion of Lean4Lean's implementation proofs.

The public issue preceded edits. Worktree `lean4lean42/resource-pilot` starts
at PR #323 head `ad7208b825513d1e6095fcb80c9621d000f570e2`. Production follow-up
is frozen by `lean4lean42-resource-contracts.md` at `edda8af`.

## Exact input and the caught label mismatch

The retained v8 object named `ResourceModel42KernelSnapshot.olean` has length
21,858,464 but BLAKE3
`764ba3db5b9ba4c7548cc8d5f6512693398266ae55ba3bcb616545ff4c74f354`.
The pre-build identity guard refused to conflate it with the final independently
accepted object. No compiler or external replay ran on that mismatched input.

The final acceptance binding is
`/tmp/resource-kernel42-acceptance-final-binding.json`. Its actual module label
is `ResourceModel42Kernel`. All **24** original source/object files retained
under `/tmp/vesper-resource-model42-782806-0` matched that binding exactly, as did
the unchanged projector source (35,773 bytes, BLAKE3
`bd2a748c9886b06e5419cf5aa01711dc46d8d3da916d9e184b543afaa7c27eb2`).

The genuine projector was rerun with those originals, original module
`ResourceModel42.Acceptance`, pinned actual Lean prefix
`/home/agent/.local/share/vesper-lean`, and fresh disjoint output root
`/tmp/lean4lean42-resource-pilot-395/projection`. With the exact accepted output
label it reproduced the accepted object: **21,858,464 bytes**, BLAKE3
`511f9481e7bcefad5bb2adbf1940072a086f791dc00eb7a09f45d8cb55c8aed6`.
Canonical, omitted, duplicate, occurrence and alternate-proof mappings all
equal the independent acceptance binding: 3,791 retained canonical constants,
48 narrowly matched omitted auxiliaries, and one additional actual proof alias.
These are observed counts, not replacement acceptance predicates.

The reproduction exited zero with empty stderr and exactly one genuine
snapshot marker. A separate post-launch watcher enforced a 240-second deadline
and 4-MiB/64-KiB stdout/stderr caps; it observed completion 1.202 seconds after
attachment. No precise end-to-end snapshot wall time is claimed.

## Small isolated native build

The corrected original sealed tree
`/tmp/vesper-lean4lean42-root-setup-9uyvct2u/checker-string` remained unchanged.
Its historical v1 checker identity is
`776dbf335b8135322a60d4273561e72ea0b53860852712ecad8788c8bbaab399`.
The separate replay-worker cache was read, never rebuilt or overwritten.
All **235** cached Lean4Lean/Batteries Lean sources compared byte-for-byte equal
to that corrected sealed tree. The actual 13 selected compiler/runtime files
also matched the sealed library identities.

Only a new Main was compiled, in the absent private directory
`/tmp/lean4lean42-resource-pilot-395`. Cached dependency objects and import
artifacts were reused through explicit absolute paths. No build-tree clone or
Cargo invocation was needed.

| Actual phase | Exit | Seconds |
| --- | ---: | ---: |
| Pinned Lean 4.24, `-j1 -M4096`, Main to olean/ilean/C | 0 | 5.637 |
| Pinned clang, Main C to export object | 0 | 4.525 |
| Pinned clang, Main object plus cached checker/Batteries objects | 0 | 1.080 |

The complete argument vectors, including every linked object, flags and system
library, are retained in `build-result.json`. The commands were reconstructed
from the real cached Lake traces, redirecting only Main input/output and the
new executable. Each phase had a 300-second deadline and 64-KiB output caps.
The **293** recorded build input files had identical before/after SHA256
associations; raw source/object file lengths were also checked.

The executed pilot Main was 31,535 bytes, BLAKE3
`7f5d915355c2b5d20bf2d6fb497a969c229b520372a3532a71a60a28bdeddba8`.
Its native checker was 197,207,752 bytes, BLAKE3
`03baea25f414b2564827106b2a5ed96e83d90d6b77abbe63ab57b53f582315ae`.
Hashes associate the observed inputs and binary; they are not a proof of
machine-code derivation. BLAKE3 values came from the existing genuine Rust hash
utility, not a Python substitute.

## Actual full inventory and external replay

The Main-only pilot adds fixed tag **3**, routed to `ResourceModel42Kernel`.
Every projected owned declaration must have skip=0 and is a required ID.
Existing target0/target1/control-tag2 branches and declaration checking remain.
The pilot retains the v1 greeting solely to reuse the existing wire profile;
it creates no profile-1 or profile-2 receipt.

The two actual native commands were:

```text
/tmp/lean4lean42-resource-pilot-395/lean4lean --vesper42 0 3 11223344556677889900aabbccddeeff
/tmp/lean4lean42-resource-pilot-395/lean4lean --vesper42 1 3 11223344556677889900aabbccddeeff
```

Both used only the fresh projection on LEAN_PATH, the explicit pinned sysroot,
one Lean thread, closed stdin and controlled PATH/LANG. They were serial.
The bounded stdlib Python pilot validator checked framing, typed names,
module routing, ownership groups, exact native/external inventory equality,
skip IDs, unique eligible coverage IDs, quotient prerequisites, constructor and
recursor comparisons, complete terminal counters, EOF and real process exit.
It is not the production Rust protocol validator. Limits were 65,536-byte
frames, 64-MiB stdout, 256-KiB stderr, bounded declaration/name metadata, and
300/1,800-second inventory/replay deadlines. No inventory prefix was accepted.

| Observed measure | Native inventory | External replay |
| --- | ---: | ---: |
| Exit / stderr bytes | 0 / 0 | 0 / 0 |
| Seconds | 4.820 | 340.181 |
| Modules | 1,732 | 1,732 |
| All constants | 161,247 | 161,247 |
| Eligible constants | 158,207 | 158,207 |
| Unsafe-only / partial-only / both skips | 438 / 2,602 / 0 | 438 / 2,602 / 0 |
| Encoded name bytes | 12,001,294 | 12,001,294 |
| Projected owned eligible / skipped | 3,792 / 0 | 3,792 / 0 |
| Required projected IDs covered | not a replay | 3,792 / 3,792 |
| All eligible IDs covered | not a replay | 158,207 / 158,207 |
| Actual additions | not a replay | 151,847 |
| Constructor / recursor comparisons | not a replay | 3,892 / 2,440 |
| Quotient groups | not a replay | 1 |
| Remaining / pending / ctor pending / rec pending | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 |
| Frames | 162,983 | 324,202 |
| Stdout bytes | 18,414,272 | 22,517,820 |

Inventory-record SHA256 was exactly identical in both modes:
`41e9db4b269c490e1a179558bd5424c7ded4a90b0e34dbb310b244ab98694646`.
Full framed-transcript BLAKE3 values were respectively
`4f1d3044b9e2ffac50cc540bfd00d8bb0ed30f5bb23469a9dc83aababa0f71d0`
and `1654454f1993a9bac69f6368c87cefea6ba6fe9570e4d5e09a0262dda4fa4015`.
Actual input identities remained unchanged after both calls. No kernel.patch
change was needed, and the full eligible Lean/Std/Lean.Replay import closure
participated; the old controls' inventory alone was not used as replay evidence.

## Typed-name association and final production patch

All 3,792 owned names were inspected from their **typed wire components**.
No producer display string was split or inverted. Their unique forward images
equal the exact producer retained-canonical name set plus every actual alias.
There are six names containing apostrophes and 336 names containing numeric
components. Numeric components were canonical u64 decimals.

The initial identifier-plus-apostrophe restriction genuinely failed on two
generated `_@` string components, IDs 525 and 938, under
`ValidationError.match_on_same_ctor` and its `het` helper. Their full typed
components are retained. After coordinator review, the final contract permits
the **exact literal `_@`**, not arbitrary escaping. The resulting forward map
has 3,792 unique images and no unsupported component. Trusted import names
retain the existing unrestricted typed wire codec.

The final production patch additionally uses the required profile-2 greeting,
makes control tag2 explicit, with unknown target names mapping to anonymous
instead of falling back to the control module, and reserves resource root
storage from the actual bounded target-eligible count. Argument parsing rejects all
unknown tags and invalid mode/tag pairs. Existing tags, old 33/37 roots and all
94 control definitions are preserved. This final Main is **31,616 bytes**, BLAKE3
`c81d811d36cbb7e0631e87c1cc9f596970a5b7abbad591e54035a837d414cd98`;
final replay.patch is **47,413 bytes**, BLAKE3
`56b5cefd037ce3c922ce73312060483c5f80de0b3b76698c9a964f3f74d149ba`.
The patch applies to the immutable upstream Main and reconstructs those exact
production Main bytes. `git diff --check` passed.

The final routing/greeting/root-reservation changes were not rebuilt or replayed by this
lane. Genuine fresh production setup/sealing, all three target replays, the
94-control rerun, Rust metadata-cap measurement and independent acceptance are
separate required gates. No pins, setup, source manifests, receipt, Rust,
model, exporter, auditor, Cargo or CI file was changed here.

Detailed evidence is under `/tmp/lean4lean42-resource-pilot-395`:
`snapshot-request.json`, `snapshot-report.json`, `build-inputs-{before,after}.json`,
`build-result.json`, `replay-inputs-{before,after}.json`, `compiler-runtime-verified.json`,
`target3-mode{0,1}.{frames,json,stderr}`, `owned-typed-names.json`,
`owned-name-summary{,-extended}.json`, `final-artifacts.json`, and `run.log`.
No macOS/Windows execution, native resource observation, source adaptation,
compiler refinement, resource campaign, axiom-policy rerun or checker
implementation-proof completion is claimed.
