# Explicit Lean4Lean 4.24 setup

This developer tool prepares the fixed external checker for the two bundled
models. It builds pinned source, asks the genuine Rust driver to validate and
seal it, then publishes the directory once. It does not install Lean, build
Vesper, run a floating package update, or establish whole-checker verification.

Python 3.11 or later is required; setup uses only its standard library. Supply
an already-built genuine `lean4lean42` Rust binary from this checkout and an
installed Lean/Lake 4.24.0 toolchain. Root-owned `pins.json` and
`source-manifest.json` must be finalized before setup can run. Missing inputs
are hard failures, not a successful partial setup.

```sh
python3 tools/lean4lean42/setup.py \
  --directory /absolute/path/to/NEW_TOOL_ROOT \
  --lean /path/to/lean --lake /path/to/lake --git /path/to/git \
  --driver /path/to/vesper/target/debug/lean4lean42
```

Every option is required and may appear once. The destination's parent must
already exist; the destination itself must be absent, including absent as a
broken symlink. No existing directory is reused, overwritten or repaired.
Bare executable names are searched through PATH. A launcher's final basename
is preserved when invoking it; setup never executes a manager shim after
renaming its resolved target. Initial Lean/Lake probes select actual
`SYSROOT/bin/lean` and `SYSROOT/bin/lake` from one canonical prefix. Lean must
report release 4.24.0 and commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`; the initial and actual Lake versions
must agree with that toolchain. The actual sysroot paths are recorded for the
sealer and used for the named build.

Setup performs these operations in private sibling staging/scratch directories:

1. Fetch and detach exactly Lean4Lean commit
   `4faf51403bc4cb35cf6666d91fc9c938ae46d3aa` from
   `https://github.com/digama0/lean4lean.git`; verify the actual HEAD.
2. Apply the fixed repository patches in order: `toolchain.patch`,
   `kernel.patch`, `replay.patch`. Git checks indexed exact context before each
   application; no three-way/fuzz/reject fallback is used. Both Lake dependency
   declarations must pin Batteries
   `8da40b72fece29b7d3fe3d768bac4c8910ce9bee`.
3. Fetch that exact Batteries commit into `.lake/packages/batteries`, verify
   HEAD, and copy only the fixed controls umbrella to
   `Lean4Lean/VesperControls.lean`.
4. Run `DRIVER --verify-setup-source STAGING_DIRECTORY`. This genuine Rust
   preflight verifies embedded BLAKE3 pins and the complete expected source
   tree before executing build inputs. It runs no compiler/checker and writes
   no receipt. Python supplies no substitute BLAKE3 implementation or digest.
5. Execute exactly `ACTUAL_LAKE build lean4lean`, with one Lean thread. No
   default/proof target or `lake update` is selected.
6. Write the observed successful build's bounded logs and exact
   `setup-inputs.json`, then run `DRIVER --seal-setup STAGING_DIRECTORY`.
   The genuine Rust sealer repeats source checks, binds compiler libraries and
   generated outputs, inventories the controls' imported baseline, and runs
   actual profile/controls before creating a receipt. A source-preflight
   confirmation or a prior receipt cannot replace those gates.
7. Require the sealer's bounded JSON and actual regular receipt to agree, then
   publish with one atomic exclusive rename. Linux uses `renameat2` with
   `RENAME_NOREPLACE`; macOS uses `renamex_np` with `RENAME_EXCL`; Windows uses
   its non-replacing `os.rename`. Missing APIs/unsupported systems fail closed;
   there is no replacing fallback. A concurrently created empty destination is
   preserved. Failure removes only setup's private staging/scratch directories.

The setup-input object has exactly `schema`, `lean`, `lake`, `git`, `build_argv`,
`build_exit`, `build_stdout`, and `build_stderr`. Schema is 1; build exit is the
observed zero exit, not a placeholder. Log names are exactly root `build.stdout`
and `build.stderr`. The other fixed root names are `setup-inputs.json` and
`setup-receipt.json`. The Rust driver owns all receipt hashes and control
evidence; Python never manufactures successful control records.

## Source and process bounds

The shared source walk includes every regular file recursively, regardless of
extension, including all unknown `.lake` entries. It excludes only:

- A `.git` metadata entry at any depth.
- The exact root `.lake/build` subtree and the pinned Batteries
  `.lake/packages/batteries/.lake/build` subtree.
- The four fixed setup root filenames listed above.

It includes all other package/native/configuration inputs. New files, including
unknown Lake caches, must match the embedded expected manifest; an extra cache
exception needs a published contract amendment. Selected source symlinks,
nonregular files and Windows reparse points/junctions are rejected before
descent. These rules do not change the Rust sealer's separate explicit policy
for selected in-sysroot runtime-library aliases. Paths are sorted by normalized
relative UTF-8 bytes. Python checks at most 8192 source files, 256 MiB of stated
source lengths, 4096 UTF-8 bytes per path, and conservatively at most 32768 walk
entries. Rust independently streams actual contents and verifies full hashes.

The whole explicit operation has an 1800-second deadline. Tool probes have
30-second deadlines. Exit is accepted only when observed before the deadline.
Each command's stdout/stderr is capped at 64 KiB, except the sealer's receipt
stdout, capped at 4 MiB. Readers drain concurrently in bounded chunks; output
overflow fails even when excess bytes are not retained. Build logs are never
unbounded. Receipt and setup-input caps are 4 MiB and 16 KiB respectively.

POSIX children run in a private process group. Windows children start suspended,
are assigned to a kill-on-close Job Object, then resume through the documented
thread APIs, so compiler descendants cannot escape before assignment. Cleanup
kills the owned process tree, waits/reaps the child, and joins both pipe readers;
an unfinished reader is a failure, never successful cleanup. Windows Job Object
and macOS publication branches require actual platform validation; Linux-only
checks are not evidence that they have executed.

Ambient Lean/search/loader and Git configuration overrides are removed. The
controlled environment fixes `LEAN_NUM_THREADS=1`; it preserves required OS
settings, inherited standard proxy variables (upper/lower case), and CA trust
settings with TLS verification enabled. Setup does not print proxy values or
dump the environment. Proxy values are redacted from reported command errors.
On Windows the controlled loader PATH contains only the selected sysroot's bin
and required OS directories. Git hooks/global configuration are disabled for
the immutable fetch/checkouts.

Ordinary `lean4lean42 --tool-root ...` is a distinct offline operation; it does
not run this setup, fetch packages or silently repair missing inputs. A receipt
associates observed local source/compiler/build/control bytes. It does not
prove machine-code derivation, host integrity, axiom hygiene or completion of
Lean4Lean's unfinished implementation proofs. Complete model-closure replay
and axiom/implementation-proof audits remain separately reported operations.
