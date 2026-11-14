# Windows read-only attribute lint expectation (#544)

Base: #498 at `ca6d41791ab3f7110ad5f2650ea3b47d8f9227ab`.
Only three Windows-compiled `set_readonly(false)` calls gain a statement-scoped
`#[expect(clippy::permissions_set_readonly_false)]`, each explaining that only
the Windows read-only attribute changes and ACL permissions remain unchanged.
Unix owner-mode handling, cleanup behavior and the corruption control are
unchanged. No command-line warning threshold or whole-file allowance changes.

## Actual failure and API decision

[Windows job 132459336442](https://github.com/robot-head/vesper/actions/runs/43598941381/job/132459336442)
failed strict workspace Clippy on Rust 1.100.0. Full diagnostics (timestamps
and terminal colour removed):

```text
error: call to `set_readonly` with argument `false`
   --> src\bin\resource_oracle42\process.rs:281:17
    |
281 |                 permissions.set_readonly(false);
    |                 ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    |
    = note: on Unix platforms this results in the file being world writable
    = help: you can set the desired permissions using `PermissionsExt`. For more information, see
            https://doc.rust-lang.org/std/os/unix/fs/trait.PermissionsExt.html
    = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.100.0/index.html#permissions_set_readonly_false
    = note: `-D clippy::permissions-set-readonly-false` implied by `-D warnings`
    = help: to override `-D warnings` add `#[allow(clippy::permissions_set_readonly_false)]`

error: call to `set_readonly` with argument `false`
   --> src\bin\resource_oracle42\process.rs:292:25
    |
292 |                         permissions.set_readonly(false);
    |                         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    |
    = note: on Unix platforms this results in the file being world writable
    = help: you can set the desired permissions using `PermissionsExt`. For more information, see
            https://doc.rust-lang.org/std/os/unix/fs/trait.PermissionsExt.html
    = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.100.0/index.html#permissions_set_readonly_false

error: could not compile `vesper` (bin "resource_oracle42" test) due to 2 previous errors
```

The corresponding Windows-only integration-test call is covered by the same
narrow expectation.

- [Pinned Clippy source](https://github.com/rust-lang/rust-clippy/blob/rust-1.100.0/clippy_lints/src/permissions_set_readonly_false.rs#L33)
  matches literal false on every target; its documented hazard is Unix
  world-writable permission bits.
- [Rust 1.100 platform documentation](https://github.com/rust-lang/rust/blob/1.100.0/library/std/src/fs.rs#L2557)
  distinguishes that Unix behavior from clearing Windows
  `FILE_ATTRIBUTE_READONLY`.
- [Windows implementation](https://github.com/rust-lang/rust/blob/1.100.0/library/std/src/sys/fs/windows.rs#L1147)
  clears that attribute bit. It does not alter ACL security descriptors.
- [Windows `PermissionsExt`](https://github.com/rust-lang/rust/blob/1.100.0/library/std/src/os/windows/fs.rs#L524)
  exists but remains unstable (`windows_permissions_ext`, issue 152956), so
  it is not a stable-toolchain replacement.

## Validation and limits

Rust/cargo 1.100.0 and rustfmt 1.11.0 were used. Targeted rustfmt and whitespace
checks passed. A temporary minimal package points directly at this checkout's
actual CLI and integration-test source files, using the existing exact serde
1.0.229 (`derive`, `rc`), serde_json 1.0.154 and blake3 1.8.7 dependencies:

```sh
cargo +1.100.0 clippy --manifest-path /tmp/resource-endpoint42-windows-readonly-clippy-100/Cargo.toml --locked --all-targets -- -D warnings
```

This passed in 1.98 seconds on Linux; it is not a whole-workspace check.
A separate metadata-only Clippy compilation for `x86_64-pc-windows-msvc`
uses the exact actual cleanup function and permission-test fragment. With
`-D warnings -D clippy::permissions_set_readonly_false -D unfulfilled_lint_expectations`,
the original extracts fail with exactly three read-only lints; the annotated
extracts pass. No linking or execution occurs. The first temporary wrapper
used `&Path` rather than the actual `PathBuf` local, producing two unrelated
needless-borrow lints; only the wrapper type was corrected. That failed log
is retained under `/tmp/resource-endpoint42-windows-readonly-lint-v1/`.
Final source hashes, exact commands and results are in the corresponding
`-lint-v2/receipt.json`.

No Lean, endpoint runtime matrix or Windows runtime test ran for this change.
The hosted full Windows workspace retry remains required. The inaccessible
Linux Test-all failure is not attributed to these Windows-only calls.
