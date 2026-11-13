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
