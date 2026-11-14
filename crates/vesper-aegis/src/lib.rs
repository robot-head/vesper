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

#![forbid(unsafe_code)]

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
mod reader;
pub mod roots;
pub mod sgx;
pub mod snp;
pub mod tdx;
pub mod time;

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
