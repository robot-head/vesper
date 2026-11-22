//! Vesper Aegis (design §33): confidential-computing guest libraries.
//!
//! - [`tdx`], [`sgx`], [`snp`], [`cca`]: bounds-checked parsers and
//!   end-to-end verifiers for Intel TDX quotes v4 and v5 (TD 1.0, 1.5 and
//!   1.5-ex bodies), Intel SGX DCAP quotes v3 (enclave report body, SGX
//!   TCB info v3 and QE identity v2), AMD SEV-SNP attestation reports
//!   versions 2–6 (Milan, Genoa, Turin; version 6 extended TCBs on Venice)
//!   and Arm CCA tokens (tag 399 and CMW tag 907 collections), with real
//!   ECDSA/COSE verification and certificate paths to a configured root or
//!   endorsed CCA platform keys.
//! - [`corim`]: CoRIM/CoMID parsing (unsigned tag 501 and COSE-signed),
//!   and appraisal of CCA platform and realm tokens against the Arm CCA
//!   endorsement profiles' reference values and attest-key triples.
//! - [`claims`]: normalized claims, the 128-byte wire struct, report-data
//!   binding and register replay.
//! - [`policy`]: the RA-TLS relying-party policy verifier.
//! - [`gpa`]: GPA page-acceptance tracker with `UnacceptedGpaRange` owners.
//! - [`host`]: `UntrustedHostVal`, `HostBounceSlice`/`PrivateRegion`
//!   partition, single-fetch snapshots and Iago validators.
//! - [`gate`]: the §33.3 inference gate over a simulated shared ring.
//!
//! - [`roots`], [`intel`], [`amd`]: pinned Intel/AMD vendor roots, Intel
//!   DCAP collateral (TCB info, QE identity, CRLs, TCB status, Platform CA
//!   platform-instance PCK data, anti-rollback floor) and AMD VCEK/VLEK
//!   collateral (KDS cert chains, ARK CRL floor), verified against real
//!   published samples in `tests/vendor_samples.rs` and
//!   `tests/vendor_samples_rest.rs`. `lean/AegisCollateral.lean` models the
//!   TCB evaluation order and proves it monotone, and proves reference-value
//!   appraisal sound (about the model only).
//!
//! No TEE hardware is exercised: real vendor samples are verified as data,
//! negative cases use generated chains, and `attest_channel_binding`
//! returns `TeeError::HardwareUnavailable`.
//! Linear owners are Rust host-library discipline, not the Vesper source
//! typing rules for `linear struct`.

// Unsafe code is allowed only in the generated binding to the Vesper
// port.
#![deny(unsafe_code)]

pub mod amd;
pub mod cca;
pub mod chain;
pub mod claims;
pub mod corim;
pub mod error;
pub mod gate;
pub mod gpa;
pub mod host;
pub mod intel;
mod linear;
pub mod policy;
// The quote, report and claims parsers, the timestamp parser and the
// policy check are Vesper (vesper/, the Loom package @vesper/aegis); the
// Rust items forward to it through src/port.rs (SH6 lane 8, #1403: the
// Rust implementations were deleted when the port became the only build).
mod port;
// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, dead_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;
pub mod roots;
pub mod sgx;
pub mod snp;
pub mod tdx;
pub mod time;

/// Which build this is: always `"vesper"`, the Vesper port (the Rust
/// parsers were deleted; the name stays for callers that report it).
pub const IMPLEMENTATION: &str = "vesper";

/// Live sequence allocations of the Vesper port (for leak tests).
#[doc(hidden)]
pub fn port_live_allocations() -> i64 {
    port_binding::live_allocations()
}

pub use chain::{Curve, EcPublicKey, TrustRoot};
pub use claims::{
    AegisAttestationClaimsWire, AttestationClaims, ENCLAVE_FLAG_DEBUG_DISABLED,
    ENCLAVE_FLAG_LEAN_VERIFIED, ENCLAVE_FLAG_SMT_MITIGATED, GuestRuntimeClaims,
    SupplyChainEvidence, TeeArch,
};
pub use error::{AegisVerifyError, AttestError, ParseError, TeeError};
pub use gpa::{GpaAcceptanceTracker, PrivatePageSlab, UnacceptedGpaRange};
pub use host::{HostBounceGrant, HostBounceSlice, PrivateRegion, PrivateSlice, UntrustedHostVal};
pub use policy::{RelyingPartyPolicy, verify_attestation_claims, verify_ra_tls};

/// The port's CBOR decoder: one item at the start of `input` and the bytes
/// it used, or `None` when ciborium would reject it. For
/// tests/cbor_differential.rs only.
#[doc(hidden)]
pub fn __port_cbor_value(input: &[u8]) -> Option<(coset::cbor::value::Value, usize)> {
    port::cbor_value(input)
}
