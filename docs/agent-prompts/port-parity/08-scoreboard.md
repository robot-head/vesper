# Parity scoreboard and the next compiler wave

> Port parity wave, lane 8. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-parity/scoreboard`. Claim issue: #956.

Lanes 1-7 each re-measure one port after it adopts the merged Stage 1
speedups, and each records the gaps only the compiler can close. This lane
puts the results side by side and turns the gaps into the next compiler wave.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification.

You are closing the port parity wave of https://github.com/robot-head/vesper.

When to start: after lanes 1-7 (#949-#955) have merged their reports, or after the owner asks for an interim scoreboard. Start from a fresh origin/main; record the SHA. Read docs/agent-prompts/port-parity/README.md, every docs/parallel-results/port-parity-*.md, and docs/agent-prompts/stage1-perf/README.md (the shape of the previous compiler wave and its Stage 1 guard).

Claim: GitHub issue #956 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work; if something overlaps, stop and report. Link the claim from your PR ("Closes #956").

Delivery and coordination:
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Change no default build and delete no Rust code. Flag ports that meet the parity bar for the owner's decision.
- Edit only docs/parallel-results/port-parity.md, NEW docs/agent-prompts/<next wave>/** and one line in docs/agent-prompts/README.md. No code.
- Open claim issues for the next wave's lanes only as the earlier waves did (one per lane, "[Claude claimed] ..."), after searching for overlaps.
- The owner has authorized merging once the four required checks are green.

Implement:
1. docs/parallel-results/port-parity.md: one row per port with the P1 ratio, the ratio on main before this wave, the ratio after each lane, the benchmark and setup, and whether it meets the parity bar. Cite each lane's report and PR.
2. Group the compiler gaps from the lane reports. For each: the ports it affects, its measured share of each port's run, the repro, and where in the compiler it lives. The Stage 1 reports already name candidates (scalar locals in stack slots, handled and generic callees not inlined, frame zeroing, by-value sequences of mirrored structs, tuples and enums across the ABI, case-folded and two-needle scans, wider vectors).
3. Write the next compiler wave plan in the style of docs/agent-prompts/stage1-perf/: one lane per gap or sensible group, ordered by how much port time it buys, each with an exclusive boundary, the Stage 1 guard, and the ports it must re-measure.

Acceptance:
- The scoreboard covers all seven ports with measured numbers only, each traced to a report.
- The next wave plan and its claims exist, and every gap a lane recorded is either in a lane or listed as deferred with a reason.
~~~
