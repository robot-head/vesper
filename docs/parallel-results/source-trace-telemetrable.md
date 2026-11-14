# Source Beacon: `Trace` and `Telemetrable` in the checker (wave eight, lane 13)

Claim: #462. Base: `1094e649c95e4d81eef4e705d13882ab1339542d` (`origin/main`,
a descendant of the required `ea1ed29`). Design: §22 of
`docs/specification.txt` (Google Doc revision of 2026-11-13 04:46 UTC),
mainly §22.1 items 1, 2 and 5, §22.2 and §22.4.

## What was built

| Item | Where |
| --- | --- |
| Built-in Beacon prelude (`SpanContext`, linear `ActiveSpan`, `TelemetryAttr`, `linear effect Trace`, `trait Telemetrable`) | `src/trace_effects.rs` |
| `Trace` rules E0750–E0754 | `src/trace_effects/rules.rs` |
| `Telemetrable` coherence and redaction E0756–E0758 | `src/trace_effects/telemetry.rs` |
| Evaluator host that records spans through `vesper-beacon` and exports span forests | `src/trace_effects/host.rs` |
| Hooks: `check::check` adds the prelude and runs the rules; `pub mod trace_effects` | `src/check.rs`, `src/lib.rs` (own commit) |
| Fixtures (3 accepted with expected span trees, 14 rejected) and 16 tests | `tests/trace_effects.rs`, `tests/fixtures/trace_effects/**` |

### The prelude

A module that names `Trace`, `SpanContext`, `ActiveSpan`, `TelemetryAttr`
or `Telemetrable` without declaring any of them gets the declarations of
`trace_effects::PRELUDE` in front of its items (and the trait only when it
names `Telemetrable`). A module that declares any of those names keeps its own
declarations and none of the rules or the host apply, as with `Region`.

```vesper
copy struct SpanContext { trace_id_hi: u64, trace_id_lo: u64, span_id: u64, flags: u8 }
linear struct ActiveSpan { ctx: SpanContext, parent_span_id: u64, start_mono_ns: u64, schema_id: u32 }
copy enum TelemetryAttr { NoAttr, Count(u64), Bytes(u64), Redacted }
linear effect Trace {
    fn current_ctx() -> SpanContext;
    fn start_span(schema_id: u32, name: String) -> ActiveSpan;
    fn start_child(parent: SpanContext, schema_id: u32, name: String) -> ActiveSpan;
    fn emit_ok(sink span: ActiveSpan, attr_u64: u64) -> Unit;
    fn emit_err(sink span: ActiveSpan, err_code: u32, msg: String) -> Unit;
    fn emit_attr(sink span: ActiveSpan, attr: TelemetryAttr) -> Unit;
}
trait Telemetrable { fn telemetry_attr(in self) -> TelemetryAttr; }
```

Differences from §22.2, all additive or spelling: `StaticStr` is `String`
(the core has no static string type); `start_child` starts a span under an
explicit parent, which is what a `with_parent` handler forwards to (§22.3's
`handle Trace with_parent(ctx)` has no surface syntax, so it is written as an
ordinary `handle Trace` block, see `accept/with_parent.ves`); `emit_attr`
finishes a span with a typed attribute; `Telemetrable::telemetry_attr`
returns a `TelemetryAttr` instead of writing into an `AttrWriter`.

### Rules

The existing checker already gives `ActiveSpan` its `Mode::Linear` meaning,
and the fixtures pin that down: a span not finished on every path is E0309,
an abortable effect (`?`, `Fail`) while a span is live is E0402, and a call
into a `Trace` function from a body without `Trace` in its row or an
enclosing handler is E0401 (context propagates through calls only along
rows). On top of that, on the checked and elaborated World:

| Code | Rule |
| --- | --- |
| E0750 | A span cannot leave the function that started it: no function or closure result, ADT field or wrapped parameter mentions `ActiveSpan`; no expression wraps one (`(span, n)`, `Some(span)`); no closure captures one; labeled capabilities (`eff [t: Trace]`, `t.start_span(..)`) count as `Trace` operations; a call producing an `ActiveSpan` must be `Trace::start_span`/`start_child` (catches generic `id(span)`). |
| E0751 | An `ActiveSpan` literal, or an assignment into a span (including nested places such as `s.ctx.span_id = …`), outside a `Trace` handler clause forges a span. |
| E0752 | Destructuring an `ActiveSpan` outside a `Trace` operation clause (including a handler's `return` pattern) discards it without finishing it. |
| E0753 | Dropping a context: a `SpanContext` result as a statement or bound to `_`, or a `SpanContext` binding, parameter, closure parameter or `Trace` handler-clause parameter that is never read. |
| E0754 | A `handle Trace` installed where `Trace` is already in scope must propagate: its `current_ctx` and `start_span` clauses must perform an outer `Trace` operation or read a `SpanContext` bound outside the handler. A top-level `Trace` handler may start a root. |
| E0756 | At most one `Telemetrable` instance per type, counting the canonical `impl` and named `model`s together (heads that unify overlap, so `impl [T] … for Box[T]` and `model …: Telemetrable for Box[u64]` conflict), so a type has one exported encoding. |
| E0757 | No canonical `Telemetrable` impl for a secret type (`capability_effects::SECRET_TYPES` declared as an ADT) or for a bare type parameter (a blanket impl would cover secrets). Secrets need a named redacting model (§22.4's `RedactedTokenTelemetry`). |
| E0758 | A `Telemetrable` method may not read secret data: an expression whose type contains a secret type (through arguments or fields, the same notion the `ConstTime` checker uses) may only be the base of a projection, so `self.user_id` is fine and `f(self.password)` is not. Reading a secret's payload outside `ConstTime` is already E0707. |

### Evaluator host

`trace_effects::host::run_traced(&checked, entry, args, TraceConfig)` runs an
entry whose row is exactly `eff [Trace]` through `Runtime::begin_external`
and serves each `Trace` operation with `vesper-beacon`:

- `start_span`/`start_child` start a span in a `SpanPool<64>` under the
  current context (the most recently started open span, else the configured
  imported parent, else the trace's root marker `span_id` 0); the Beacon
  owner is held by the host until the program finishes the span.
- `emit_ok`/`emit_err`/`emit_attr` finish the owner into a
  `TelemetryRing<256>` (draining into the export when full) with
  `PrimaryAttribute::ItemCount`, `ByteCount`, `None` or the content-free
  `Redacted` marker.
- Time is a logical clock (one tick per operation), so trees and durations
  are deterministic.
- Typed failures (`TraceRunError`): schema ids outside Beacon's vocabulary
  (0 request, 1 storage append, 2 background), unknown error codes (0–3),
  invalid contexts, a span the host does not hold open (forged or finished
  twice through a handler), pool/ID/export limits, and spans left open at
  the end. Open owners are cancelled, never dropped silently.
- `TraceRun::tree()` renders the exported forest; accepted fixtures carry
  the expected rendering in `// span: ` header lines.

## Checks

Toolchains: stable 1.97 and 1.100.0 (clippy), x86_64 Linux.

| Command | Result |
| --- | --- |
| `cargo test --locked --test trace_effects` | 16 passed |
| `cargo +1.100.0 clippy --locked -p vesper --all-targets -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `vesper fmt --check tests/fixtures/trace_effects` | all unchanged |
| `cargo test --workspace --locked` | see the PR description |

The tests cover: every accepted fixture's exported tree; every rejected
fixture's code (each rule has one); parent links and trace ids; an imported
`traceparent` (parsed with `vesper_beacon::parse_traceparent_55`) becoming
the root's parent; secret non-interference on the host (changing the secret
leaves the exported spans identical); host typed errors; a handler that
re-sends a finished span; a handler that swallows a host span (reported
unclosed); entry and prelude activation rules; generic and blanket escapes.

## Decisions

- **Prelude in `check::check`.** The prelude is added before Pact
  elaboration because the `Telemetrable` trait must be visible to the
  frontend, which the `World::collect` builtins cannot provide. Callers that
  use `pact_frontend::check_package` directly do not get the prelude.
- **Strict escape rule.** §22.2 says spans are closed on every path; this
  lane reads it as "in the function that started them", so span-returning
  helpers are rejected and helpers take spans as `sink` parameters.
- **One instance per type** counts named models too, since an exported
  encoding must be coherent across scopes; the design's named redacting
  model for `Secret[...]` is then that type's single instance.
- Diagnostic codes E0750–E0758 were unused on main and in the open
  wave-seven PRs (#375 adds E0724/E0725/E0732).

## Remaining gaps

- No `@pii` attribute (needs §16.4 attribute syntax, held by Codex) and no
  `@span_schema` macro; schema ids are plain integers.
- E0756 detects overlap by unifying instance heads with each instance's type
  parameters as variables; repeated parameters are not tracked, so it can
  only over-report.
- E0754 is syntactic: a clause that reads an outer context counts as
  propagating even if it does not use it to parent spans.
- Secrets that were declassified through a `ConstTime` gate are public by
  definition and may be exported; there is no taint tracking outside
  `ConstTime`.
- No native/JIT or Wasm lowering of `Trace`: the evaluator host only.
  No EvidenceVector register propagation, fibers, OTLP-Arrow export or
  Lean proof (§22.4's theorem) in this lane; the host test of
  non-interference is a test, not a proof.
- Rust ownership in the host is not evidence that the Vesper typing rules
  are implemented; the rules are checked by the source checker and tested by
  fixtures.
