//! Vesper Aegis (design §33): confidential-computing guest libraries.
//!
//! - [`tdx`], [`snp`], [`cca`]: bounds-checked parsers and end-to-end
//!   verifiers for Intel TDX quote v4, AMD SEV-SNP attestation reports and
//!   Arm CCA tokens, with real ECDSA/COSE verification and certificate
//!   paths to a configured root.
//! - [`claims`]: normalized claims, the 128-byte wire struct, report-data
//!   binding and register replay.
//! - [`policy`]: the RA-TLS relying-party policy verifier.
//! - [`gpa`]: GPA page-acceptance tracker with `UnacceptedGpaRange` owners.
//! - [`host`]: `UntrustedHostVal`, `HostBounceSlice`/`PrivateRegion`
//!   partition, single-fetch snapshots and Iago validators.
//! - [`gate`]: the §33.3 inference gate over a simulated shared ring.
//!
//! - [`roots`], [`intel`], [`amd`]: pinned Intel/AMD vendor roots, Intel
//!   DCAP collateral (TCB info, QE identity, CRLs, TCB status) and AMD
//!   VCEK/VLEK collateral, verified against real published samples in
//!   `tests/vendor_samples.rs`.
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
pub mod error;
pub mod gate;
pub mod gpa;
pub mod host;
pub mod intel;
mod linear;
pub mod policy;
mod reader;
pub mod roots;
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
