# Optimizer differential boundaries

Claim [#1569](https://github.com/robot-head/vesper/issues/1569).

A recorded, valid rewrite of `wrapping_add(x, 0)` was checked at one work unit below, at, and one unit above its final charge. Both certificate checkers refused the first budget and accepted the other two. The result codes and all three spent counters matched.

Two IR programs read one scalar field, write either that field or its sibling, then read the original field again. The Rust and Vesper policies agreed on the elimination count and complete output IR for each program.

Both targeted tests, Rust 1.100.0 Clippy and rustfmt passed on Linux. The new cases produced 0 differences and removed 0 production Rust lines. They do not change the optimizer's execution time.
