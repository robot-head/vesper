# Integration B: CLI and compiler hooks for the wave-five crates

Suggested branch: `wave6/integration-b`. Claim issue: #136 (claimed in wave five).
Starts once Silicon (#149) and Chorus (#151) merge; eBPF (#163) and lanes 1, 5-11 are already on main.

~~~text
You are wiring the wave-five host libraries into the Vesper compiler and CLI in https://github.com/robot-head/vesper. This is integration B from docs/agent-prompts/wave-five/README.md and docs/rfcs/wave-five-contracts.md.

Start from a fresh origin/main that contains the merges of PRs #149 (Silicon), #151 (Chorus) and #163 (eBPF); record the SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/bazel.md, docs/rfcs/wave-five-contracts.md, docs/rfcs/wave-six-contracts.md and the wave-five reports in docs/parallel-results/ (vault, chorus, silicon, ebpf, crucible, nexus, chrono, resonance, aegis, kinetic, migrate) before editing.

Claim: issue #136. Check the open issues and PRs for overlap before you start; link "Closes #136" from your PR.

Exclusive ownership during wave six:
- src/main.rs routing, NEW src/*_commands.rs modules for the commands below, src/lib.rs module lines.
- A NEW verifier-hook module (for example src/native_verifiers.rs) plus narrow hook calls in src/native.rs behind explicit flags. Lane 23 later adds coverage hooks next to yours; keep your hook point small and documented.
- Root Cargo.toml dependencies of the root package on the wave-five crates, Cargo.lock, root BUILD.bazel data if needed.
- NEW tests/*_cli.rs for these commands, docs/parallel-results/integration-b.md.
Do NOT edit src/check.rs or src/capability_effects* (lane 15), src/lsp* (lane 8), src/doc*/doctest*/lint_commands.rs (lane 14), src/bench_commands.rs (lane 13), src/optimize* (other agents, §42), or any crates/** source (the owning wave-six lanes edit those crates concurrently; use only APIs on main).

Implement:
1. `vesper fuzz <file.ves> --target <fn>`: run Crucible over a source function through the evaluator (and the native backend when `--native`), with corpus, shrinking and `--differential` against a `vesper-migrate` shadow where given; emit found failures as `@test` regression cases into a user-named file.
2. `vesper verify --refute <file.ves>`: run Crucible refutation search against a function's stated contract before Lean proving; report a counterexample or "no counterexample found within budget".
3. `vesper migrate --source <c-or-rust path>`: lift signatures through `vesper-migrate` and write .vsys/.ves output.
4. `vesper replay <bundle.vchrono>`: inspect, verify and replay a Chrono bundle (timeline, reverse watch query, obligation lineage).
5. `vesper ebpf check <object.o | asm>`: run the eBPF pre-flight verifier and report typed findings; `--load` uses the kernel path where available.
6. Stage-5 verifier hooks: behind explicit flags (`--verify-const-time`, `--verify-stack`, `--verify-realtime`, `--verify-detfloat` or one documented `--verify=<list>`), run the Vault, Silicon, Resonance and Kinetic verifiers on the Cranelift functions `src/native.rs` emits, with policies read from a policy file for now (lane 24 later derives them from source effects). Map findings to function names and, where available, source spans.
7. Merge the Silicon (lane 3) and eBPF (lane 4) stack analyzers behind one `CallGraph` stack-bound report, as the wave-five contracts require, without editing those crates: put the adapter in the root.
8. `vesper build --tee` RTMR measurement extension through Aegis if it fits; otherwise record it as remaining.

Rules: the common rules in docs/agent-prompts/wave-six/01-bridge-pointers-mpk.md ("Delivery and coordination") apply, including the Bazel, fmt, Lean, unavailable-hardware and merge-main-before-merging rules. Existing programs must compile to identical code when no new flag is given (test it). The required CI checks are `check`, `macOS (AArch64)` and `Windows (x86_64 MSVC)`; the user authorized merging once they are green and review comments are addressed.

Acceptance:
- End-to-end CLI tests for each command on small .ves fixtures (canonically formatted), including failure exits and typed messages.
- A test that each verifier flag rejects a planted violation in emitted Cranelift and accepts a clean function, on x86_64 and AArch64.
- Report: docs/parallel-results/integration-b.md, naming the hook module and its API so lanes 23 and 24 can build on it.
~~~
