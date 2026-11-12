# native-pulse experiment

Runs the `vesper-fiber-abi` prototype end to end: Cranelift-JIT entries park
and resume on real fiber stacks under the unmodified `vesper_pulse`
scheduler, then x86_64 and AArch64 relocatable objects are written. It then
shuts fibers down through `NativeRuntime`: a clean shutdown that cancels one
fiber before start, one after a yield and one during I/O; a recoverable
incomplete shutdown in which fibers whose cancellation edge parks again
exhaust a two-tick drain budget, come back owned, and finish on a second
drain; and, when io_uring is available, a kernel pipe read cancelled during
I/O.

```sh
cargo run -p native-pulse-experiment --release -- --out target/np --fibers 4
```

The output names the driver it used. `io_uring (kernel)` means real kernel
completions; `fixture` means `io_uring_setup` was refused and reads were
served in-process, so kernel I/O was not exercised. See
`docs/rfcs/native-pulse-abi.md` for the ABI and its limits.

Run this command from the repository root. The experiment shares the workspace lockfile and is covered by workspace formatting, Clippy and compilation checks. Its runtime protocol tests live in `vesper-fiber-abi`.
