// The backend verifiers (SH4 lane 17, #1291), from `modules/backend_checks`,
// through the `verifiers` hook.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[rustfmt::skip]
#[allow(clippy::all)]
#[path = "../../port/backend_checks.rs"]
mod backend_checks_binding;

// The backend verifiers swap's encoder and decoder (#1291), shared with
// tests/self_host_backend_checks.rs.
#[path = "../../../../tests/common/backend_checks_input.rs"]
mod backend_checks_input;

use super::{Registry, Swap, env_off, on, open};

/// Backend verifier runs answered by Vesper (`@vesper/backend-checks`).
static VERIFIER_CALLS: AtomicU64 = AtomicU64::new(0);

/// The `verifiers` hook of `vesper::codegen_swap` from `modules/backend_checks`
/// (#1291): `@vesper/backend-checks` runs const-time, stack, realtime and
/// detfloat verification over emitted Cranelift functions.
fn vesper_verifiers(
    world: &vesper::check::World,
    functions: &[vesper::native::EmittedFunction],
    target: Option<&str>,
    verifiers: &std::collections::BTreeSet<vesper::native_verifiers::NativeVerifier>,
    policy: &vesper::native_verifiers::NativeVerifierPolicy,
) -> Option<Result<vesper::native_verifiers::NativeVerifierReport, String>> {
    let bytes = backend_checks_input::encode(
        world,
        functions,
        target,
        &backend_checks_input::Mode::Emitted(verifiers, policy),
        "",
        "",
    );
    let words = backend_checks_binding::verify_words(&bytes);
    VERIFIER_CALLS.fetch_add(1, Relaxed);
    Some(backend_checks_input::decode_report(&words))
}

/// The flip-gate rows of this file's swaps (`swaps::on`).
pub(super) const SWAPS: &[Swap] = &[open("verifiers", "no flip-gate configuration")];

pub(super) fn register(r: &mut Registry) {
    if on("verifiers") && !env_off("VESPER_HYBRID_VERIFIERS") {
        r.codegen.verifiers = Some(vesper_verifiers);
    }
}

pub(super) fn report() {
    eprintln!(
        "vesper-hybrid: backend verifiers ran {} times from Vesper",
        VERIFIER_CALLS.load(Relaxed),
    );
}
