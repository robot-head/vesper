# Strict proof packets and Bazel runfiles (#429)

macOS job 132360688103 in run 43566266748 confirms #407's regular-source panic
on `./tools/lean4lean-foundations42/upstream/Lean4Lean/List.lean`, caused by
Bazel leaf-symlink runfile inputs. That test failed in 0.01s; 701 other tests
passed. Windows Cargo validation passed. The Linux all-target check also
failed; its separate BuildBuddy log endpoint is outside the environment's
network allowlist.

Enable the ALREADY IMPLEMENTED `copy_test_cwd` mode for the five fixed proof
packets. The unchanged `tools/bazel/test_launcher/main.rs` materializes
declared input files before entering each unchanged strict test. It removes
its launcher flags before executing the test. All source/provenance, original/
projection, axiom, alias, corruption, regular-artifact and budget checks stay
byte-identical. Artifact symlink refusals occur after startup and remain
enforced. No Rust/Lean validator, proof, source manifest, projector/auditor,
profile, workflow, timeout/cap or acceptance-policy change.

An extracted EXACT source-loader function reproduced regular-file success
and leaf-symlink rejection. Full genuine test binaries were then compiled
with Bazel's manifest `.` and bare `vesper` PATH binding, using the exact
serde_json/blake3 dependencies selected from the successful Cargo test
fingerprint. The ACTUAL repository launcher ran them against 105 declared
repository-input leaf symlinks and the actual CLI/pinned Lean.

Without copy mode, the actual full foundation binary exits 101 at the exact
remote regular-source guard. With copy mode, all five complete real matrices
pass, including every source/artifact corruption and forbidden-alias control:

| Packet | Complete test seconds | Including launcher copy |
| --- | ---: | ---: |
| Foundations | 134.40 | 134.894 |
| Construction | 132.97 | 133.021 |
| Order | 132.71 | 132.813 |
| Map | 144.14 | 144.226 |
| Recursive construction | 181.31 | 181.361 |

All five projected objects are byte-identical to ordinary Cargo execution.
The existing finite 300-second budgets need no change. These are measured
results, not golden acceptance inputs. There is no supplied expected answer
or arbitrary proof-module override.

Evidence under `/tmp/lean4lean-proof-packets-runfiles-429/` includes receipt.json
(exact compiler/dependency/launcher/binary/input identities and commands),
the genuine without-copy failure, all five complete logs/evidence JSON, and
summary.json. The launcher and test sources were not altered. This validates
the real launcher's declared input topology, not a complete local Bazel
invocation; the hosted BUILD retry remains a separate gate.

The corresponding BUILD selections are pushed in the owned foundation,
construction, order and map branches; the recursive branch adds its fifth
entry. Parent #407's hosted retry starts at `5e19c99`. Existing validators
and proof source-byte associations remain unchanged throughout.

The foundation association retained from the parent report is 49 audited roots,
356 canonical declarations, and a 5,058,968-byte object with BLAKE3
`4d1483789427ed0be66c1245852d00c584ffc7d84a673cc4966142ae00459d7e`.
