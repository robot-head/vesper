# Bound views and platform interop

Claim [#1559](https://github.com/robot-head/vesper/issues/1559).

- §13. A view of an aggregate's sequence field can be sliced again while a sibling field is written. The evaluator, optimized and unoptimized JIT, and Linux executable all returned 6. Writing the borrowed field before block exit produced E1531 even after the last read.
- §16.3 foreign memory. The checker accepted a foreign call after both pinned views left scope. It refused the same call with BufferMisuse when only the derived view had left scope.
- §16.3 assembly objects. The test runner refused a test calling the package's assembly-backed library and identified the package and the executable-build requirement.

The three Linux suites passed 29 tests with one existing timing test ignored. Rust removed: 0 lines. The changes add test oracles only.
