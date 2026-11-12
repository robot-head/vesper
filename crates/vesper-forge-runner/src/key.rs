//! The execution key: a `vesper-loom-cas` [`DerivationInputs`] binding
//! everything that can change what a macro component observably returns.
//!
//! ```text
//! output_domain   = Artifact
//! builder         = "vesper-forge-runner"
//! builder_version = "<crate version>"
//! options:
//!   vfr.key-schema / vfr.step / vfr.network
//!   host.*, sandbox.*, vfr.record-schema       (HostIdentity)
//!   determinism.{wall-clock-ns,monotonic-ns,random-seed}
//!   limits.{fuel,...,wall-clock-backstop-ns}
//!   limits.transport.{max-tokens,...}
//! inputs:
//!   component          Artifact blob of the exact component bytes
//!   invocation         Source blob of the canonical invocation JSON
//!                      (step kind, entry, args, input token text/kinds/spans)
//!   mount:<guest path> Source tree of each input snapshot
//! ```
//!
//! Nothing about approval is in the key: registry, environments, graph,
//! policies, ceilings and component bindings are rechecked by every session.

use vesper_loom_cas::{DerivationInputs, Domain, ObjectId};
use vesper_loom_sandbox::{Determinism, Invocation, ResourceLimits, TransportLimits};

use crate::identity::HostIdentity;
use crate::request::MacroRequest;

pub const KEY_SCHEMA_VERSION: u32 = 1;
pub const RUNNER_NAME: &str = "vesper-forge-runner";

/// Canonical bytes of an invocation: `serde_json` emits fields in
/// declaration order and sequences in order, and the transport definition
/// is bound through the executable digest.
pub fn canonical_invocation(invocation: &Invocation) -> Vec<u8> {
    serde_json::to_vec(invocation).expect("invocation serialization is infallible")
}

pub(crate) fn derivation_inputs(
    request: &MacroRequest,
    identity: &HostIdentity,
    component: ObjectId,
    invocation: ObjectId,
    mounts: &[(String, ObjectId)],
) -> DerivationInputs {
    let mut d = DerivationInputs::new(Domain::Artifact, RUNNER_NAME, &identity.runner_version);
    let mut opt = |k: &str, v: String| {
        d.options.insert(k.to_owned(), v);
    };
    opt("vfr.key-schema", KEY_SCHEMA_VERSION.to_string());
    opt("vfr.step", request.kind.as_str().into());
    opt("vfr.network", "denied".into());
    for (k, v) in identity.key_options() {
        opt(k, v);
    }
    for (k, v) in determinism_options(&request.determinism) {
        opt(k, v);
    }
    for (k, v) in limit_options(&request.limits) {
        opt(k, v);
    }
    d = d
        .with_input("component", component)
        .with_input("invocation", invocation);
    for (guest, tree) in mounts {
        d = d.with_input(format!("mount:{guest}"), *tree);
    }
    d
}

// No `..` in these destructurings: a new sandbox setting fails to compile
// until it is bound into the key.

fn determinism_options(d: &Determinism) -> [(&'static str, String); 3] {
    let Determinism {
        wall_clock_ns,
        monotonic_ns,
        random_seed,
    } = *d;
    [
        ("determinism.wall-clock-ns", wall_clock_ns.to_string()),
        ("determinism.monotonic-ns", monotonic_ns.to_string()),
        ("determinism.random-seed", random_seed.to_string()),
    ]
}

fn limit_options(l: &ResourceLimits) -> Vec<(&'static str, String)> {
    let ResourceLimits {
        fuel,
        max_memory_bytes,
        max_table_elements,
        max_instances,
        max_memories,
        max_tables,
        max_stdio_bytes,
        max_output_bytes,
        max_output_entries,
        wall_clock_backstop,
        transport,
    } = l;
    let TransportLimits {
        max_tokens,
        max_text_bytes,
        max_scopes_per_token,
        max_total_scopes,
        max_diagnostics,
    } = transport;
    vec![
        ("limits.fuel", fuel.to_string()),
        ("limits.max-memory-bytes", max_memory_bytes.to_string()),
        ("limits.max-table-elements", max_table_elements.to_string()),
        ("limits.max-instances", max_instances.to_string()),
        ("limits.max-memories", max_memories.to_string()),
        ("limits.max-tables", max_tables.to_string()),
        ("limits.max-stdio-bytes", max_stdio_bytes.to_string()),
        ("limits.max-output-bytes", max_output_bytes.to_string()),
        ("limits.max-output-entries", max_output_entries.to_string()),
        (
            "limits.wall-clock-backstop-ns",
            wall_clock_backstop.as_nanos().to_string(),
        ),
        ("limits.transport.max-tokens", max_tokens.to_string()),
        (
            "limits.transport.max-text-bytes",
            max_text_bytes.to_string(),
        ),
        (
            "limits.transport.max-scopes-per-token",
            max_scopes_per_token.to_string(),
        ),
        (
            "limits.transport.max-total-scopes",
            max_total_scopes.to_string(),
        ),
        (
            "limits.transport.max-diagnostics",
            max_diagnostics.to_string(),
        ),
    ]
}
