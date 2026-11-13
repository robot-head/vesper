# Documentation link CI reconciliation (#181, stage5)

Coordinator importc2e384e contains the four preceding CI stages and latest main,
including the owner's Nexus implementation and BUILD fix. This final stage
follows the published `docs/rfcs/bazel-stack41-contracts.md` and leaves all those
source/build changes intact.

The new executable `scripts/test-doc-links.sh` obtains `//:bin/vesper` through the
landed Bazel helper or accepts `VESPER_DOC_LINKS_BINARY`. It passes only the
existing `--binary` option: this harness manages its own temporary directory and
has no `--work` option. BuildBuddy and macOS run it after proof, preview, parameter
and tuple acceptance. Windows retains Cargo and runs the same harness against
`target/debug/vesper.exe`. All five stages now have actual, unconditional
acceptance commands in each corresponding platform lane.

Validation passes:

- Actual read-only executable override:49commands against the coordinator's
  supplied root binary. SHA256 before and after the complete run is unchanged:
  `a01bb57a780c122c9a45aadfe4a08baff56c42736cafd7b9bd1332563bf04dbe`.
  This verifies actual local documentation behavior, not a fresh build of the
  imported main or a Bazel-produced executable.
- Shell syntax, whitespace, executable mode and workspace formatting checks.
- Duplicate-key-rejecting YAML parsing; complete, ordered, exactly-once five-stage
  calls on Linux/macOS/Windows; actual referenced-script existence; preserved
  proof/preview workflow removal.
- Locked no-dependency metadata discovers `doc_links`, `doc_links_cli` and the
  pure documentation `links` target. BUILD synchronization passes, including the
  imported Nexus BUILD. No additional BUILD or dependency change is needed.

No Cargo cache writes, installation, Bazel default-path/RBE or hosted platform
execution, source/native changes, credentials or administrative settings were
performed here. Earlier failed BuildBuddy proof checks remain under coordinator
investigation, with public-log access returning403; local acceptance does not
claim those failures or the required-context setup gap are resolved. Publication,
actual current-head protected checks and reviewed integration remain with the
coordinator. #181 stays reserved through that integration.
