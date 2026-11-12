# Current-source macro preview provider (#153)

The pure `lsp::macro_preview` codec borrows a real sealed database snapshot and
associates it with exact current source bytes, current version, and a URI label.
It verifies retained source identity and the successful compiler artifact's
original source/hash. No semantic World is copied or exposed, and no URI is
opened. This does not change the ordinary diagnostic or hover providers.

Successful `vesper.editor.macro-preview/1` responses expose only checked expanded
source, actual compiler byte provenance and invocation audits. Compiler refusals
retain actual stage, message, diagnostics and any rejected candidate; diagnostic
space is original only at parse stage, expanded only with a real candidate, and
otherwise none. Refusals always have empty maps/audits and omit accepted fields.

The constructor bounds source at 128 KiB and URI at 4096 bytes before comparison
or hashing. Rendering rejects zero/above-hard profiles, prepays a finite envelope
for JSON parsing, payload and geometric buffer copies, and charges serialization
bytes plus write calls from the remaining work allowance. A fallible bounded
writer caps encoded bytes before JSON Value materialization. Default output is
1 MiB and work is 16 Mi units (hard work 32 Mi); this is no RSS/latency claim.

Seven public tests use real database/compiler snapshots: actual success DTOs and
hashes/maps/audits; exact-source mismatch and arbitrary URI labels; actual E0303
linear rejection; parse/unsupported diagnostic spaces; repeated argument byte
provenance; all invalid/tight resource profiles and exact encoded-size boundary;
and oversized legacy input without retained bytes or hash promotion.

Validation pending actual database dependency and sequential use of an existing
facade cache; no stub compiler or manufactured snapshot is used. Root owns module
registration/session routing and integration acceptance.
