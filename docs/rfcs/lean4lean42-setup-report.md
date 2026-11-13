# Explicit pinned setup lane (#301)

The frozen base `43b1d406` and public claim #301 preceded implementation.
Published amendments `2ac3337` (genuine pre-build source verification) and
`397481c` (strict exhaustive source walk) were read before dependent changes.
Owned paths are only `tools/lean4lean42/setup.py`, its README, and this report.
No patch, controls, pins, manifest, Rust, Cargo, existing CLI or CI file is
modified by this lane.

The stdlib-only Python script validates the exact fixed recipe, immutable
commits, ordered patch paths and controls overlay; selects Lean/Lake without
losing launcher basenames; obtains the actual 4.24 compiler prefix; and uses
private source/staging directories. The genuine Rust driver's exclusive
`--verify-setup-source` gate precedes the named Lake build. Only an observed
successful build creates the exact bounded setup-input/log files. The genuine
`--seal-setup` operation owns BLAKE3 hashes, imported-control baseline binding,
actual controls and the receipt. Publication is one exclusive atomic rename,
with no overwrite fallback or Python-generated success receipt.

The source walk includes all regular inputs, including unknown `.lake` entries,
with only the published Git/build/four-root-file exclusions. It rejects source
symlinks, nonregular entries and Windows reparse points before descent. Runtime
library aliases remain a separate Rust policy. Environment cleanup preserves
Cloud proxy/CA settings, strips loader/search injection, and fixes one Lean
thread. Commands enforce observed-exit deadlines and bounded concurrent output
readers. POSIX process groups and a suspended-start Windows Job Object provide
tree cleanup; the latter is authored source, not executed platform evidence.

Actual writer checks on Linux:

- `python3 -m py_compile tools/lean4lean42/setup.py`: passed.
- Twelve stdlib-only focused checks passed: exclusive Linux rename collision
  preserves the preexisting empty destination and staged marker; existing
  destination refusal; exact source exclusions; unknown Lake/native/arbitrary
  extension inclusion; source symlink refusal; actual deadline refusal; exact
  stdout-cap-plus-one refusal; stderr overflow refusal; already-finished child
  first observed after deadline refusal; proxy/CA preservation plus controlled
  environment; missing final pins refusal before tools/network; duplicate CLI
  refusal. Process checks observed actual child exit/reaping, closed pipe
  handles and joined readers. No fake successful compiler/checker was used.
- `git diff --check`: passed.

Final root-owned pins/source manifest and the assembled genuine Rust driver
were unavailable for a positive setup run at this checkpoint. No Git fetch,
Lean/Lake build, source preflight/seal, receipt, complete replay or positive
controls result is claimed by this lane. Windows Job Object/reparse and macOS
exclusive-publication branches have not executed here. Root coordinates the
real assembled setup and platform evidence. The same receipt limitations as
the frozen contract apply: no executable-derivation, hostile-host, axiom-hygiene
or finished implementation-verification claim follows from local setup.
