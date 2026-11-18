# Re-measure every port and update the parity scoreboard

> Port gaps wave, lane 9. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-gaps/rescore`. Claim issue: #992.

Lanes 1-8 and PR #983 change the compiler; this lane puts their new builtins
into the ports and measures every port again, as the port parity wave did.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification.

You are re-measuring the Vesper ports of https://github.com/robot-head/vesper after the port gaps wave.

When to start: after PR #983 (#972) and lanes 1-8 (#984-#991) merge, or after the owner asks for an interim score. Start from a fresh origin/main; record the SHA. Read docs/agent-prompts/port-gaps/README.md, docs/agent-prompts/port-parity/README.md (the port-only rules and the parity bar), docs/parallel-results/port-parity.md and every docs/parallel-results/port-gaps-*.md.

Claim: GitHub issue #992 claims this work. Check open issues and PRs for overlap first; if something overlaps, stop and report. Link the claim from your PR ("Closes #992").

Delivery and coordination:
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Change no default build and delete no Rust code. Flag ports that meet the parity bar for the owner's decision. (Aura's Rust build is already gone, by the owner's choice in #979; measure it against its last Rust commit with bench-abba.sh.)
- Port-only edits, under the port parity wave's rules: crates/vesper-<port>/** for the seven ports, plus docs/parallel-results/port-parity.md and docs/parallel-results/port-gaps-rescore.md. No compiler edits. One PR per port is fine if that is easier to review; each links #992.
- The owner has authorized merging once the four required checks are green.

Implement:
1. Measure every port on main before any port change, with its committed ABBA benchmark. That number is what the compiler work bought.
2. Adopt the new builtins and undo the workarounds the lane reports name (bit counts in warp-wheel, class scans and case folding in Prism and Quarry, `copy_within`/`swap` in Aura, directive placement, Quarry's `free_d`, Aura's exact-size handles). Keep every port test, divergence test and leak test green.
3. Re-measure after each port's changes, same machine and run, with the 95% interval and setup.
4. Update docs/parallel-results/port-parity.md: a new column per port and the gaps that remain.

Acceptance:
- Every port has a before/after number from this lane, traced to the commit it measured.
- The scoreboard says which ports meet the parity bar, and lists the remaining compiler gaps with repros.
- Report: docs/parallel-results/port-gaps-rescore.md.
~~~
