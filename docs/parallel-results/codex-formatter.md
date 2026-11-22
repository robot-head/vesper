# Formatter macro handoffs

Claim [#1565](https://github.com/robot-head/vesper/issues/1565). The formatter differential previously counted each standalone Vesper handoff as a match without running the public formatter. It now compares the public result with the frozen Rust seed, including output bytes, the changed flag, and refusal details.

The six Linux differential tests passed. The repository sweep covered 1,228 sources, including 64 macro-profile handoffs. The mutation sweep covered 1,500 cases, including 63 handoffs and 1,200 refusals. All comparisons agreed. Three output-limit cases around a valid macro's formatted size and three malformed macros also agreed.

This test-only change removed 0 production Rust lines. It does not change formatter execution time.
