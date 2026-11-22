# WIT component differential

Claim [#1561](https://github.com/robot-head/vesper/issues/1561). Appending an inert custom section to a compiler-produced core module changes section framing while preserving the command interface. The corpus covers section payload sizes 126–129 and 16,382–16,385 bytes, on both sides of the one-to-two and two-to-three byte unsigned LEB128 transitions.

The Vesper wrapper accepted all eight modules. Its output matched the validating Rust encoder byte for byte in all eight cases on x86-64 Linux. Rust removed: 0 lines. This changes the differential corpus only.
