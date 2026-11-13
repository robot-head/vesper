//! Typed errors. Untrusted bytes never panic: every malformed, ambiguous,
//! overflowed or over-budget input maps to one of these.

use crate::claims::TeeArch;
use std::fmt;

/// Which evidence format a parse error came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    TdxQuoteV4,
    SnpReport,
    CcaToken,
    ClaimsWire,
}

/// A bounds or value failure while decoding untrusted evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Fewer bytes than the field needs.
    Truncated {
        format: Format,
        field: &'static str,
        need: usize,
        have: usize,
    },
    /// Bytes left over after a structure that must end the input.
    TrailingBytes { format: Format, extra: usize },
    /// A field holds a value this implementation does not accept.
    UnsupportedValue {
        format: Format,
        field: &'static str,
        value: u64,
    },
    /// A reserved field is not zero.
    NonZeroReserved { format: Format, field: &'static str },
    /// A length field is larger than the configured bound.
    OverBudget {
        format: Format,
        field: &'static str,
        limit: usize,
        value: u64,
    },
    /// CBOR/COSE structure is malformed, ambiguous or of the wrong shape.
    Cbor {
        field: &'static str,
        reason: &'static str,
    },
    /// PEM certificate data is malformed.
    Pem(&'static str),
}

/// Failure verifying hardware evidence (signatures, chains, bindings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttestError {
    Parse(ParseError),
    /// Certificate path building to the configured root failed.
    CertificateChain(String),
    /// A certificate's key is not of the type the format requires.
    UnexpectedKeyType {
        what: &'static str,
    },
    /// A signature did not verify.
    BadSignature {
        what: &'static str,
    },
    /// The signature algorithm in the evidence is not supported.
    UnsupportedAlgorithm {
        what: &'static str,
        value: i64,
    },
    /// An internal binding between evidence parts did not hold.
    BindingMismatch {
        what: &'static str,
    },
    /// A runtime measurement register did not match the replayed event log.
    RegisterReplayMismatch {
        register: &'static str,
    },
    /// A certificate in the path is listed on a CRL, or a TCB level / QE
    /// level the evidence matches is marked `Revoked`.
    Revoked {
        what: &'static str,
    },
    /// Collateral needed for the check was not supplied (or a CRL does not
    /// cover a certificate whose revocation status must be known).
    MissingCollateral {
        what: &'static str,
    },
    /// Collateral (CRL, TCB info, QE identity) is outside its validity
    /// window at the verification time.
    CollateralExpired {
        what: &'static str,
    },
    /// A certificate in the path is past its `notAfter`.
    CertificateExpired,
    /// A certificate in the path is before its `notBefore`.
    CertificateNotYetValid,
    /// A CRL is malformed.
    Crl(String),
    /// Vendor collateral (TCB info, QE identity JSON, certificate
    /// extensions) is malformed, ambiguous or of an unsupported version.
    Collateral {
        what: &'static str,
        reason: String,
    },
    /// The evidence does not match its collateral (FMSPC, PCE ID, QE
    /// identity, TDX module identity, VCEK chip ID or TCB).
    CollateralMismatch {
        what: &'static str,
    },
    /// No TCB level in the collateral matches the evidence.
    TcbLevelNotFound {
        what: &'static str,
    },
    /// A pinned vendor certificate does not have its recorded SHA-256.
    PinnedRootDigestMismatch {
        name: &'static str,
    },
}

impl From<ParseError> for AttestError {
    fn from(e: ParseError) -> Self {
        AttestError::Parse(e)
    }
}

/// Relying-party policy rejections (design §33.5, plus the corrections
/// recorded in `docs/parallel-results/aegis.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AegisVerifyError {
    DisallowedTeeArchitecture(TeeArch),
    EnclaveDebugModeEnabled,
    StaleTcbSvn {
        actual: u16,
        min_required: u16,
    },
    /// A per-component AMD SEV-SNP TCB floor was not met.
    StaleSnpTcbComponent {
        component: &'static str,
        actual: u8,
        min_required: u8,
    },
    BootMeasurementMismatch,
    SupplyChainLoomLockMismatch,
    FormalOleanProofDigestMismatch,
    ChannelTranscriptBindingMismatch,
    /// The policy requires hardware register replay for supply-chain
    /// digests, but this evidence binds them through report data only.
    SupplyChainNotRegisterMeasured(TeeArch),
    /// SEV-SNP guest ran at a VMPL the policy does not allow.
    DisallowedVmpl {
        actual: u32,
        max_allowed: u32,
    },
    HypervisorGpaDoubleAccept {
        gpa_page_4k: u64,
    },
    UnalignedOrZeroGpaRange,
    /// The vendor TCB status is not one the relying party accepts
    /// (`Revoked` never is).
    TcbStatusNotAccepted(crate::intel::TcbStatus),
}

/// Errors from the confidential-VM runtime surface (page acceptance and
/// host bounce buffers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeeError {
    /// GPA range is unaligned, empty, or overflows the address space.
    UnalignedOrZeroGpaRange { gpa_base: u64, page_count_4k: u64 },
    /// The host re-announced or the guest re-accepted pages already accepted.
    HypervisorGpaDoubleAccept { gpa_page_4k: u64 },
    /// The range partially overlaps accepted or pending pages.
    OverlappingGpaRange { gpa_page_4k: u64 },
    /// A read or write touched a page that is not accepted private memory.
    UnacceptedPageAccess { gpa_page_4k: u64 },
    /// The (simulated) host tried to write an accepted private page.
    HostWriteToPrivatePage { gpa_page_4k: u64 },
    /// Split point outside `1..page_count`.
    BadSplit { at: u32, page_count_4k: u32 },
    /// Merge of ranges that are not adjacent, or not from the same tracker.
    NotAdjacent,
    /// A range owner from a different tracker was presented.
    ForeignOwner,
    /// The page-acceptance backend (TDG.MEM.PAGE.ACCEPT / PVALIDATE) failed.
    AcceptBackend { gpa_page_4k: u64, code: u64 },
    /// Host-reported length exceeds the bounce slot capacity.
    HostLengthOutOfRange { reported: u64, cap: u64 },
    /// Destination private region has no room for the snapshot.
    PrivateRegionExhausted { need: usize, free: usize },
    /// Ciphertext does not fit the bounce slot.
    CiphertextTooLarge { len: usize, cap: usize },
    /// Bounce slot index out of range or already granted.
    BadBounceSlot { slot_idx: u32 },
    /// A grant from another aperture was presented.
    ForeignGrant,
    /// Host mapping reply overlaps an existing mapping.
    HostMmapOverlap { addr: u64, len: u64 },
    /// Host mapping reply is unaligned, empty, overflows or leaves the window.
    HostMmapInvalid { addr: u64, len: u64 },
    /// Host clock went backwards.
    HostTimeNonMonotonic { previous: u64, reported: u64 },
    /// Host clock jumped further than the configured bound.
    HostTimeJump {
        previous: u64,
        reported: u64,
        max_step: u64,
    },
    /// The operation needs TEE hardware this host does not have (no TDX,
    /// SEV-SNP or CCA guest interface is present or implemented).
    HardwareUnavailable { what: &'static str },
    /// Host reported more bytes transferred than were requested.
    HostByteCountExceedsRequest { reported: u64, requested: u64 },
}

macro_rules! display_via_debug {
    ($($t:ty),*) => {$(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(self, f)
            }
        }
        impl std::error::Error for $t {}
    )*};
}
display_via_debug!(ParseError, AttestError, AegisVerifyError, TeeError);
