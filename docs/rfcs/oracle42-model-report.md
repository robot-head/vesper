# Executable arithmetic oracle model (#273)

Implemented against the frozen `oracle42-contracts.md` published under parent
#269 before writing. The owned source checkpoint is `9ba6e2b`. Only
`lean/ArithmeticOracle.lean` and this report belong to this lane.

The static module imports the existing `RewriteWitness` model and bundled
`Lean.Data.Json`. Its executable checker independently resolves primitive input
and earlier-local reads, checks literal parser/type ranges, intrinsic arity,
integer callees and the overflow effect requirement. Input invocation values
never become constant facts. Literal and earlier-local facts use the model's
`checkedValue`/`wrappingValue`, including the distinction between a known
overflow admitted with Fail and an operation independently shown safe without
Fail. Every initializer and the result are checked before execution.

The evaluator uses the same model operations for all numeric answers. It
executes local initializers strictly and once, including unused initializers;
local reads copy their primitive values. A shared node-visit count covers all
initializers and the result. Overflow, reference fuel exhaustion and an
internal invariant failure have distinct outcomes. Heterogeneous unused inputs
retain their actual typed data; there is no substitution of an invented input
value. Validated and checked wrappers have private constructors.

The IO server accepts only bounded array data. Four-byte little-endian lengths
are checked against 65536 before allocating payloads; both header and payload
reads loop over short reads. ASCII, array depth, scalar count, string size and
numeric-token limits are checked before the general JSON parser. Semantic
integer strings must be canonical and fit i128. Shape/depth/node/count and
input-kind range checks precede semantic checking. Framed responses are capped
at 512 bytes and flushed; fatal framing/sequence/invariant failures cannot end
with a normal BYE. A namespaced `run` and root `main` distinguish server
execution from ordinary `lean -o` model compilation.

## Actual validation

Executed from `/workspace/vesper-worktrees/oracle42-model`, with the installed
pinned Lean 4.24.0 (`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`):

```sh
mkdir -p /tmp/oracle42-model-check
/home/agent/.elan/bin/lean -o /tmp/oracle42-model-check/RewriteWitness.olean lean/RewriteWitness.lean
LEAN_PATH=/tmp/oracle42-model-check /home/agent/.elan/bin/lean -o /tmp/oracle42-model-check/ArithmeticOracle.olean lean/ArithmeticOracle.lean
python3 /tmp/oracle42-model-check/check_protocol.py
python3 /tmp/oracle42-model-check/check_bounds.py
git diff --cached --check
```

Both actual Lean compilation commands passed. The two Python probes are
lane-local `/tmp` drivers, not repository tests or replacements for independent
acceptance. Each starts the real pinned `lean --run` server, exchanges framed
data over pipes with deadlines, and inspects actual responses and exit status.
The complete production runner/corpus tests are owned by other lanes.

`check_protocol.py` passed one persistent session of 54 requests: all 15 frozen
isolated diagnostic cases; 16 overflows; 15 values; one deliberate fuel-zero
case; and seven malformed requests. Its exact terminal response was:

```json
[1,"bye",54,32,15,7,15,16,1]
```

The numeric probes cover all four kinds; checked add/subtract/multiply bounds;
wrapping add/subtract bounds; u64MAX squared wrapping to 1; i64MIN multiplied
by -1 wrapping to i64MIN; strict overflow in an unused local; three sequential
constant locals without Fail; and an unused differently typed input. Diagnostic
checks compare the frozen code matrix to independently computed Lean responses,
not request-supplied expected results.

`check_bounds.py` passed a second persistent session of 19 requests: 16 malformed
requests, two values and one intentional fuel exhaustion. Cases include scalar
count, control-token width, escapes, noncanonical/out-of-i128 decimals, invalid
input range, postfix underflow/extra roots, fuel/locals/aggregate-node/depth
caps, a complete 65536-byte frame, exact depth-eight/fuel-fifteen execution,
one-less fuel, and bytewise pipe writes. Its terminal response was:

```json
[1,"bye",19,3,0,16,2,0,1]
```

Four additional fresh server sessions confirmed fatal truncated header,
truncated payload, oversize-header rejection and a valid request with the wrong
sequence ID; each emitted the expected fatal category and exited nonzero.
Bytewise writes can coalesce in OS pipes; this probe does not claim forced
short-return behavior of the underlying Stream implementation. The source uses
exact-read loops regardless. Normal sessions closed at a header boundary,
received the exact BYE counters, then reached clean EOF and exit zero.

## Limits of this evidence

No Cargo, compiler-cache, Rust backend, complete eight-mode corpus, 100000-case,
performance, RSS or full-language proof was run or claimed by this lane. The
module is an executable independent finite-profile checker/evaluator, not a
proof that Rust source binding, the checker or machine code refines the model.
It adds no modal/place/handler metatheory and does not complete §42 or Lean4Lean.
Source/backend equivalence, process cleanup/fault injection and complete corpus
counts require the separately owned integration and acceptance runs. The
existing mathematical model source remains unchanged.

## Terminal sequence-bound follow-up

Independent review identified the terminal counter boundary: dispatching ID
9999999999 would increment `seen` to an eleven-digit BYE counter. The server
now rejects a new frame when `seen >= 9999999999`, before dispatch or increment.
EOF is handled first, so a session already at that terminal count can still
produce its normal ten-digit BYE and exit zero. The last dispatchable ID is
9999999998. The pure request API and ordinary suite behavior are unchanged.

After this change, reran the same actual oracle `lean -o` command and both
`check_protocol.py` / `check_bounds.py` probes above. All passed with the same
54- and 19-request counters and four fatal-session checks. No ten-billion-frame
session was executed or claimed; the extreme boundary correction is established
by the guarded increment and EOF ordering in source review.
