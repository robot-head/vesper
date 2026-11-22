# Rust-exit PR review

Claim [#1558](https://github.com/robot-head/vesper/issues/1558). Source review of the open queue on November 22, 2026.

| PR and reviewed head | Finding |
|---|---|
| [#1533](https://github.com/robot-head/vesper/pull/1533#pullrequestreview-6014606244) `80eade47` | P1. The WASI directory-sync shim can acknowledge a rename without flushing the parent directory. P2. Truncating the preopen table can direct a write to a different mounted directory. |
| [#1553](https://github.com/robot-head/vesper/pull/1553#pullrequestreview-6014615393) `012b7a42` | P1. The generated callback safety contract omits the lifetime of the target. P2. The manuscript promises safe Rust cannot construct a callback handle, while the new ABI test exercises two safe constructors. |
| [#1556](https://github.com/robot-head/vesper/pull/1556#pullrequestreview-6014595969) `5c3d908b` | P2. The replacement hash driver drops the malformed-key and UTF-8 refusal cases. |
| [#1556](https://github.com/robot-head/vesper/pull/1556#pullrequestreview-6014626190) `c1cd8f5b` | P1 merge blocker. The generated census still differs from main alongside the implementation changes, which triggers the repository's merge gate. |
| [#1557](https://github.com/robot-head/vesper/pull/1557#pullrequestreview-6014613316) `b6375f7d` | P2. Tensor validation changes which typed error wins. A pre-dispatch shape error also retains buffers and kernels on the recording. |
| [#1554](https://github.com/robot-head/vesper/pull/1554#pullrequestreview-6014628693) `99a5aa50` | P2. Interning distinct custom effects takes quadratic work before confinement verification begins. |
| [#1562](https://github.com/robot-head/vesper/pull/1562#pullrequestreview-6014651495) `877b8c6c` | P2. The parser accepts a declaration that shadows the built-in CStrList name. P2. Native lowering allocates eight pointer-array bytes per source byte even for a single string. |
| [#1372](https://github.com/robot-head/vesper/pull/1372#pullrequestreview-6014594953) `871b26d3` | P2. The hybrid CLI interprets a Windows drive-relative PATH entry as the root of the drive. |

On [the updated #1553 head](https://github.com/robot-head/vesper/pull/1553/commits/2a07d851e5fd310518c20f3dacf975d9be40da54), I verified that the generated contract now covers calls made after the pointer is retained, and that the manuscript acknowledges construction from an arbitrary word.

In #1556, `0e42f446` adds the invalid-key and UTF-8 cases and drops the generated census change from the PR diff. In #1557, `e1fbf292` validates tensor arguments before checking ownership or retaining resources. The reviewed head of #1533 merged with both WASI findings outstanding. [Follow-up issue #1566](https://github.com/robot-head/vesper/issues/1566)

For [#1552](https://github.com/robot-head/vesper/pull/1552) (`8b8ea6a7`), the census generated on `cf28be7a` matched byte for byte. I also reviewed the wasm handler, shift and resize changes in [#1551](https://github.com/robot-head/vesper/pull/1551) (`ce9868c7`) and the callback descriptor changes in [#1555](https://github.com/robot-head/vesper/pull/1555) (`e1a1b9f1`). I have no additional inline finding on those heads.

This review removed 0 lines of Rust. The review changes documentation only.
