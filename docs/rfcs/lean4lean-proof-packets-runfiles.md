# Regular-file proof packet inputs (#429)

Bazel supplies Unix runfile inputs as leaf symlinks. The fixed proof tests
deliberately require regular input files. On #407, macOS job 132360688103
in run 43566266748 confirms the resulting failure at the original regular-file
guard: `regular fixed source: ./tools/lean4lean-foundations42/upstream/Lean4Lean/List.lean`.
The test failed in 0.01 seconds; the job reports 701 tests passing and this
one failing. Windows Cargo validation passed.

The root BUILD declaration now selects existing `copy_test_cwd` for the
foundation packet. `tools/bazel/test_launcher/main.rs` already implements this
mode: it follows declared runfile links into a regular-file copy before
executing the unchanged test. It removes its launcher-specific environment
variables before entering that test. No validator, input catalog, proof,
kernel/profile, axiom policy, timeout/cap or workflow source changed.
Original/projected artifact symlink rejection and every real refusal control
remain enforced after test startup.

The exact original source-loader function was extracted and compiled:
ordinary source passed; a leaf symlink to the same bytes failed at the same
guard. A full genuine foundation test compiled with Bazel's manifest `.` and
bare `vesper` PATH binding, through the ACTUAL repository launcher and a
105-leaf-symlink repository-input tree, reproduced the failure without copy
mode. With copy mode, the complete foundation matrix passed in 134.40 seconds
(134.894 seconds including launcher copy), including all corruption and
forbidden-alias controls. No expected-acceptance input was supplied.

The 49-root/356-declaration foundation object remains byte-identical:
5,058,968 bytes, BLAKE3
`4d1483789427ed0be66c1245852d00c584ffc7d84a673cc4966142ae00459d7e`.
The same real-launcher construction matrix also passed in 132.97 seconds
with its object unchanged. Remaining descendant packets have matching
regular-input declarations in their respective owned branches and are being
validated separately; this parent report does not claim their results.

Evidence is retained under `/tmp/lean4lean-proof-packets-runfiles-429/`:
receipt.json binds input/source/launcher/compiler/test-binary identities and
the exact compile commands; without-copy.log records the genuine failure;
the per-packet logs, evidence JSON and incremental summary.json record actual
executions. Tests use the existing pinned Lean and actual CLI. Direct rustc
uses the exact serde_json/blake3 dependencies selected from the successful
Cargo test fingerprint. This reproduces the real launcher's input topology,
not a complete local Bazel invocation. The hosted retry remains the final
confirmation of this BUILD declaration.
