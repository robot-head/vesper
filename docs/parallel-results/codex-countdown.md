# Count-down loop differentials

Claim [#1568](https://github.com/robot-head/vesper/issues/1568). Moving the decrement from before both sequence accesses to after them switched Rust bounds hoisting from a versioned loop to a scalar loop. The Vesper pass agreed with both complete IR results and their diagnostics.

Five additional vectorizer cases target the last valid destination, the first invalid destination, wrapping offset arithmetic, and starts on either side of the sequence's upper bound. The optimized and unoptimized evaluator and JIT agreed on the checksum in every case. x86-64 and AArch64 object generation also succeeded.

The two targeted Rust 1.100.0 suites passed on Linux. This test-only change removed 0 production Rust lines and changes no production timing path.
