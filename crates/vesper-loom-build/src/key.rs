//! Canonical derivation keys for build steps.
//!
//! The key is a `vesper-loom-cas` [`DerivationInputs`] (schema V1), whose
//! canonical encoding and BLAKE3 derive-key are defined by that crate.
//! This module fixes what goes into it:
//!
//! ```text
//! output_domain   = Artifact
//! builder         = "vesper-loom-build"
//! builder_version = "<crate version>"
//! target          = request.target
//! options:
//!   vlb.key-schema            "1"
//!   vlb.request-version       request.version
//!   vlb.step                  "build-script"
//!   vlb.network               "denied"
//!   builder.source / cas.source / sandbox.source      source digests
//!   sandbox.protocol / sandbox.wit-package / sandbox.wit-blake3
//!   engine.dependencies / engine.rustc / engine.host-target
//!   determinism.{wall-clock-ns,monotonic-ns,random-seed}
//!   limits.{fuel,max-memory-bytes,...,wall-clock-backstop-ns}
//!   limits.transport.{max-tokens,...}
//!   user.<key>                every request option
//! inputs:
//!   component                 Artifact blob of the exact component bytes
//!   invocation                Source blob of the canonical invocation JSON
//!   mount:<guest path>        Source tree of each input snapshot
//! ```
//!
//! Interface identities and package-version strings are never substitutes
//! for the component bytes or the input contents.

use vesper_loom_cas::{DerivationInputs, Domain, ObjectId};
use vesper_loom_sandbox::{Determinism, Invocation, ResourceLimits, TransportLimits};

use crate::identity::ExecutionIdentity;
use crate::request::BuildRequest;

pub const KEY_SCHEMA_VERSION: u32 = 1;
pub const BUILDER_NAME: &str = "vesper-loom-build";

/// Canonical bytes of an invocation. `serde_json` emits struct fields in
/// declaration order and sequences in order, so the encoding is a pure
/// function of the value for a fixed sandbox transport definition (whose
/// sources are bound separately through `sandbox.source`).
pub fn canonical_invocation(invocation: &Invocation) -> Vec<u8> {
    serde_json::to_vec(invocation).expect("invocation serialization is infallible")
}

pub(crate) fn derivation_inputs(
    request: &BuildRequest,
    identity: &ExecutionIdentity,
    component: ObjectId,
    invocation: ObjectId,
    mounts: &[(String, ObjectId)],
) -> DerivationInputs {
    let mut d = DerivationInputs::new(Domain::Artifact, BUILDER_NAME, &identity.builder_version);
    if let Some(t) = &request.target {
        d = d.with_target(t);
    }
    let mut opt = |k: &str, v: String| {
        d.options.insert(k.to_owned(), v);
    };
    opt("vlb.key-schema", KEY_SCHEMA_VERSION.to_string());
    opt("vlb.request-version", request.version.to_string());
    opt("vlb.step", "build-script".into());
    opt("vlb.network", "denied".into());
    for (k, v) in identity.key_options() {
        opt(k, v);
    }
    for (k, v) in determinism_options(&request.determinism) {
        opt(k, v);
    }
    for (k, v) in limit_options(&request.limits) {
        opt(k, v);
    }
    for (k, v) in &request.options {
        opt(&format!("user.{k}"), v.clone());
    }
    d = d
        .with_input("component", component)
        .with_input("invocation", invocation);
    for (guest, tree) in mounts {
        d = d.with_input(format!("mount:{guest}"), *tree);
    }
    d
}

// The destructuring below has no `..`: adding a field to the sandbox's
// limit or determinism types fails to compile until the key binds it.

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
