# Lean4Lean fixed machine replay (#299)

This source checkpoint implements the adopted contract at `43b1d406`, plus the
published shared-comparison amendment `859429c`. Only `replay.patch` and this
report belong to #299. The patch modifies upstream Main.lean only. It uses the
fixed kernel94 umbrella and the same constructor/recursor metadata helpers as
the five actual comparison controls; the #298 helper update is a separate
commit (`c54ce0f`).

The fixed argument branch implements framed profile, native inventory,
control-baseline inventory, actual controls and eligible fresh replay. It
imports exactly the fixed target with importAll, bounds the native inventory,
walks exact tagged Name components before sorting by Lean.Name.cmp, records
origins/owner groups and checks the required roots. Replay starts empty, checks
scalar/group declarations with Lean4Lean, schedules Eq before a single quotient
initialization, compares all four quotient metadata records, and covers
postponed constructors/recursors only after the shared exact comparison succeeds.
Coverage bitsets/counters and required roots must be complete before done.

Actual focused build, in `/tmp/vesper-lean4lean42-replay-worker`:

```sh
LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lake build lean4lean
```

The executable build passed (55 jobs), with the actual pinned Lean4.24 compiler
and exact kernel/toolchain/umbrella sources. The retained log is
`/tmp/lean4lean42-machine-build.log`. Standard `git apply --check` for replay.patch
passed against the clean upstream base. No kernel patch, metadata, models,
Cargo or existing compiler/replay source outside Main was changed by this patch.

At this checkpoint, machine-mode protocol probes and complete fresh model
replays have not yet been executed by this lane. They remain required; a
successful executable build does not establish their success. This checkpoint
unblocks assembly of actual source pins without a placeholder patch or fake
receipt. Further actual results and any fixes will be recorded in follow-ups.
No axiom-hygiene, complete implementation verification or §42-completion claim
is made.

## Small machine probes and mutual-order follow-up

Actual `/tmp/lean4lean42-machine-probe.py` invoked the real built executable
with controlled sysroot/search paths and framed pipe reads. Profile mode passed
(two frames, clean exit0). Seven malformed argument vectors and a nonempty
stdin request failed closed. Controls mode invoked the real94-case suite,
matched every ID/expected reason/actual outcome against the fixed repository
inventory, and passed (97 frames,7876 stdout bytes, exit0).

Native MODE0/TARGET2 inventory passed with139422 constants,1584 modules and
10335275 canonical typed-name bytes, emitting141010 frames/15879202 bytes.
The probe independently recomputed header counts, contiguous IDs, exact Name
identities/bytes, owner relationships, forms/skips, and zero-work terminal
fields. These are observed counts, not hardcoded acceptance totals. The log is
`/tmp/lean4lean42-machine-probe.log`. This native binding check is not fresh
external coverage of the control baseline.

Source review then corrected mutual reconstruction: wire OWNER_IDS remain
sorted ordinals, while the actual reconstructed inductive declaration follows
the original InductiveVal.all order and checks identical ordered all-lists and
headers. Sorted wire order must not reorder generated recursor metadata. An
explicit remaining-counter underflow check was added as well. The revised real
executable build passed (55 jobs), retained in
`/tmp/lean4lean42-machine-build2.log`. Complete external model replays remain
unexecuted by this lane pending the coordinator's lease.

## First real fresh replay failure and prerequisite correction

Under the coordinator's explicit full-replay lease, both exact bundled sources
were compiled into `/tmp/lean4lean42-replay-validation` with the pinned compiler.
Native target0 inventory passed and reported87495 total/86647 eligible constants,
800 modules and33 required roots. Actual MODE1 then failed after56 successful
additions with `[1,10,"checker","unknown constant: Char.ofNat"]`; it emitted no
done and exited1. Target1 was not started after that failure. Retained failed
summaries/raw frames/input hashes are in the `failed-f48c78e` subdirectory.

The new replay branch had omitted upstream's checked Char.ofNat prerequisite
before String's primitive inductive addition. Char.ofNat is needed by kernel
string-literal support but is not a dependency of String's stored constructor
type. The correction explicitly schedules that genuine declaration through
replayId before adding String; it does not preload an unchecked constant or
weaken a primitive check. The revised executable build passed (55 jobs), logged
in `/tmp/lean4lean42-machine-build3.log`. Complete replay success remains pending
after this correction; the failed attempt is not counted as a positive result.
