# Semantic fault deadlines (#561)

Hosted macOS job `132486941210` tested merge
`838e1b253c0a48ca6e438d4dec3e290202c47f61` (endpoint head
`3240c7254f907a3400cf2473143f5dce4c6ce799` plus accepted main). Its first
hostile case, `wrong_ready`, expected `Protocol` but received
`Timeout("worker deadline")` under the existing 2000 ms fixture deadline.
The other seventeen endpoint tests passed. Later hostile cases were not
reached. This establishes an expired deadline, not its startup or scheduling
cause or a measured replacement minimum.

The fifteen semantic fault cases now use the established production request
default through `HarnessLimits::default().request_timeout_ms` (30000 ms).
All shutdown budgets stay 300 ms. Exact Protocol, Sequence, Quota, Crash and
CounterMismatch expectations remain mandatory: Timeout is never accepted in
their place. The intentional `stall` request remains 300 ms with its existing
five-second prompt-cleanup assertion. Separate blocked-stdin and Unix
BYE/EOF/live-child probes retain their original 300 ms clocks and five-second
assertions byte-for-byte. Production code, defaults and hard caps are unchanged.

For semantic cases, the total-duration assertion is derived from the declared
transport phases:

```text
(1 + request_count) * request_timeout_ms + 2 * shutdown_timeout_ms
```

`Conversation::attach` gives READY one request deadline. Each `evaluate`
exchange gives the write and response one shared request deadline. `finish`
shares one shutdown deadline across BYE, EOF and cleanup; an error before
retirement can add one `Transport::drop` cleanup deadline. The resulting
envelopes are 30600 ms for zero-request cases and 60600 ms for one-request
cases. This is conservative test phase accounting, not an OS scheduling,
process-spawn or measured-startup guarantee. The duration assertion remains
independent of the exact error-category assertion.

Scoped Rust 1.100 strict Clippy passes against the actual production endpoint
binary/test sources through a minimal genuine-dependency package (1.960 s).
Rustfmt, BUILD synchronization and whitespace checks pass. Exact source was
also compiled directly with Rust 1.100 and genuine dependency artifacts;
only the existing hostile transport test was selected. All sixteen table
cases plus two Unix timing controls pass with no runtime rustc on PATH
(1.420 s), with an interpreter path containing spaces (1.315 s), and through
the actual regular-copy launcher with `CARGO_MANIFEST_DIR="."` (1.430 s).
The launcher was compiled from its unchanged production source; this is
launcher coverage, not a Bazel build or test invocation.

Commands, environments, dependency/source/binary hashes, static comparisons
and logs are under `/tmp/resource-endpoint42-semantic-deadlines561-validation/`.
The initial launcher setup mistake (a source hash compared as a binary hash)
is retained separately; no runtime test started before it was corrected.
The original macOS log remains at
`/tmp/resource-endpoint498-macos-job132486941210-20261114T1712.log`, SHA256
`b1e819aee2fff69d18d1a29f0162a9e0e34af78e0173ee5a4d1618c4977d9c6f`.
No Lean/model/CLI runtime case ran here. Native macOS confirmation remains a
hosted gate; the inherited Windows Pulse failure is a separate issue.
