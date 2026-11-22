//! The port-backed build's glue (cfg(vesper_port)): runs the parsers and
//! the policy check of the Vesper library `aegis` (`vesper/src/lib.ves`)
//! and converts their results and failures back to the crate's own types.
//! Each parser fills `w` with the integer fields it reads and `d` with the
//! byte arrays, both in reading order; [`Fields`] reads them back in the
//! same order. The Rust bodies these replace are the seed copy's.
use crate::cca::{
    CcaCollection, CcaEndorsement, CcaPlatformClaims, CcaPlatformProfile, CcaRealmClaims,
    CcaRealmProfile, CcaSwComponent, CcaTbbRotpk, CcaToken, CoseAlg, CoseSign1,
};
use crate::chain::ec_public_key_from_pem;
use crate::corim::{
    AttestKeyTriple, CcaAppraisal, CcaAppraisalOptions, CcaCorimStore, CcaPlatformReference,
    CcaRealmReference, CcaRotpkReference, CcaSwReference, ClassId, Comid, Corim, CorimEntity,
    CorimId, CorimProfile, CorimSigner, CorimValidity, CryptoKey, Digest, Environment, Instance,
    Measurement, MeasurementValues, Mkey, RawValue, ReferenceTriple,
};
use crate::error::{AegisVerifyError, AttestError, Format, ParseError};
pub(crate) use crate::port_binding as vesper;
use crate::time::unix_seconds;

/// The tail rule a whole-input parse passes to the port (`reader::tail`):
/// nothing may follow, the rest must be zero, or anything may follow.
pub(crate) const TAIL_EXACT: u64 = 0;
pub(crate) const TAIL_PADDED: u64 = 1;
pub(crate) const TAIL_PREFIX: u64 = 2;

/// `Format` in declaration order: the port raises the index.
const FORMATS: [Format; 6] = [
    Format::TdxQuoteV4,
    Format::SgxQuoteV3,
    Format::SnpReport,
    Format::Corim,
    Format::CcaToken,
    Format::ClaimsWire,
];

/// Field names of ParseError, by the index the port raises (`AErr::field`).
pub(crate) const FIELDS: [&str; 131] = [
    "ETCB bits 255:112",                     // 0
    "version",                               // 1
    "guest_svn",                             // 2
    "policy",                                // 3
    "family_id",                             // 4
    "image_id",                              // 5
    "vmpl",                                  // 6
    "signature_algo",                        // 7
    "current_tcb",                           // 8
    "platform_info",                         // 9
    "flags",                                 // 10
    "reserved0",                             // 11
    "report_data",                           // 12
    "measurement",                           // 13
    "host_data",                             // 14
    "id_key_digest",                         // 15
    "author_key_digest",                     // 16
    "report_id",                             // 17
    "report_id_ma",                          // 18
    "reported_tcb",                          // 19
    "cpuid",                                 // 20
    "reserved1 (v2 cpuid)",                  // 21
    "reserved1",                             // 22
    "chip_id",                               // 23
    "committed_tcb",                         // 24
    "current_version",                       // 25
    "reserved2",                             // 26
    "committed_version",                     // 27
    "reserved3",                             // 28
    "launch_tcb",                            // 29
    "launch_mit_vector",                     // 30
    "current_mit_vector",                    // 31
    "reserved4 (pre-v5 mitigation vectors)", // 32
    "reserved5",                             // 33
    "current_etcb",                          // 34
    "launch_etcb",                           // 35
    "committed_etcb",                        // 36
    "reserved6 (ETCBs: v6 Venice only)",     // 37
    "reserved7",                             // 38
    "signature.r",                           // 39
    "signature.s",                           // 40
    "signature.reserved",                    // 41
    "signature component high bytes",        // 42
    "qe_report.cpu_svn",                     // 43
    "qe_report.misc_select",                 // 44
    "qe_report.reserved1",                   // 45
    "qe_report.attributes",                  // 46
    "qe_report.mr_enclave",                  // 47
    "qe_report.reserved2",                   // 48
    "qe_report.mr_signer",                   // 49
    "qe_report.reserved3",                   // 50
    "qe_report.isv_prod_id",                 // 51
    "qe_report.isv_svn",                     // 52
    "qe_report.reserved4",                   // 53
    "qe_report.report_data",                 // 54
    "body.tee_tcb_svn",                      // 55
    "body.mr_seam",                          // 56
    "body.mr_signer_seam",                   // 57
    "body.seam_attributes",                  // 58
    "body.td_attributes",                    // 59
    "body.xfam",                             // 60
    "body.mr_td",                            // 61
    "body.mr_config_id",                     // 62
    "body.mr_owner",                         // 63
    "body.mr_owner_config",                  // 64
    "body.rtmr0",                            // 65
    "body.rtmr1",                            // 66
    "body.rtmr2",                            // 67
    "body.rtmr3",                            // 68
    "body.report_data",                      // 69
    "body.vm_index",                         // 70
    "body.td_id",                            // 71
    "body.dev_info_hash",                    // 72
    "body.init_service_td_hash",             // 73
    "body.init_service_td_attributes",       // 74
    "body.init_cpu_svn",                     // 75
    "body.init_tee_tcb_svn",                 // 76
    "body.init_tee_fmspc",                   // 77
    "body.cur_service_td_hash",              // 78
    "body.cur_service_td_attributes",        // 79
    "header.version",                        // 80
    "header.att_key_type",                   // 81
    "header.tee_type",                       // 82
    "header.qe_svn",                         // 83
    "header.pce_svn",                        // 84
    "header.qe_vendor_id",                   // 85
    "header.user_data",                      // 86
    "body.type",                             // 87
    "body.size",                             // 88
    "body",                                  // 89
    "body.tee_tcb_svn2",                     // 90
    "body.mr_service_td",                    // 91
    "signature_data_len",                    // 92
    "signature_data",                        // 93
    "quote_signature",                       // 94
    "attestation_key",                       // 95
    "cert_data.type",                        // 96
    "cert_data.size",                        // 97
    "cert_data",                             // 98
    "qe_report_signature",                   // 99
    "qe_auth_data.size",                     // 100
    "qe_auth_data",                          // 101
    "pck_cert_data.type",                    // 102
    "pck_cert_data.size",                    // 103
    "pck_cert_data",                         // 104
    "body.cpu_svn",                          // 105
    "body.misc_select",                      // 106
    "body.reserved1",                        // 107
    "body.isv_ext_prod_id",                  // 108
    "body.attributes",                       // 109
    "body.mr_enclave",                       // 110
    "body.reserved2",                        // 111
    "body.mr_signer",                        // 112
    "body.reserved3",                        // 113
    "body.config_id",                        // 114
    "body.isv_prod_id",                      // 115
    "body.isv_svn",                          // 116
    "body.config_svn",                       // 117
    "body.reserved4",                        // 118
    "body.isv_family_id",                    // 119
    "header.att_key_data_0",                 // 120
    "qe_report",                             // 121
    "tee_arch_tag",                          // 122
    "tcb_svn",                               // 123
    "enclave_flags",                         // 124
    "mr_enclave_sha384",                     // 125
    "loom_lock_blake3",                      // 126
    "olean_spec_mif",                        // 127
    "chan_binding_prefix",                   // 128
    "token",                                 // 129
    "CoRIM",                                 // 130
];

/// A parse failure of the port.
pub(crate) fn parse_error(e: vesper::AErr) -> ParseError {
    let format = FORMATS[e.format as usize];
    let field = FIELDS[e.field as usize];
    match e.kind {
        0 => ParseError::Truncated {
            format,
            field,
            need: e.a as usize,
            have: e.b as usize,
        },
        1 => ParseError::TrailingBytes {
            format,
            extra: e.a as usize,
        },
        2 => ParseError::UnsupportedValue {
            format,
            field,
            value: e.a,
        },
        3 => ParseError::NonZeroReserved { format, field },
        4 => ParseError::OverBudget {
            format,
            field,
            limit: e.a as usize,
            value: e.b,
        },
        5 => ParseError::Cbor {
            field: CBOR_FIELDS[e.field as usize],
            reason: CBOR_REASONS[e.a as usize],
        },
        kind => unreachable!("ParseError variant {kind} from the Vesper port"),
    }
}

/// Field names of ParseError::Cbor, by the index the port raises
/// (vesper/src/cca.ves, cose.ves, corim.ves). A malformed item is raised
/// with field [`CBOR_WHAT`], the item the call decodes.
const CBOR_FIELDS: [&str; 85] = [
    "realm claims",                         // 0
    "platform claims",                      // 1
    "realm hash algo",                      // 2
    "RIM",                                  // 3
    "REMs",                                 // 4
    "REM",                                  // 5
    "realm challenge",                      // 6
    "realm personalization",                // 7
    "RAK",                                  // 8
    "RAK hash algo",                        // 9
    "lifecycle",                            // 10
    "software components",                  // 11
    "platform config",                      // 12
    "platform manufacturing config",        // 13
    "TBB RoTPK",                            // 14
    "platform challenge",                   // 15
    "instance id",                          // 16
    "implementation id",                    // 17
    "platform hash algo",                   // 18
    "TBB RoTPK name",                       // 19
    "TBB RoTPK index",                      // 20
    "TBB RoTPK hash",                       // 21
    "software component",                   // 22
    "software component measurement value", // 23
    "software component signer ID",         // 24
    "software component type",              // 25
    "software component version",           // 26
    "software component description",       // 27
    "platform profile",                     // 28
    "realm profile",                        // 29
    "token collection",                     // 30
    "platform token",                       // 31
    "realm token",                          // 32
    "COSE_Sign1",                           // 33
    "CoRIM",                                // 34
    "CoRIM tags",                           // 35
    "CoMID",                                // 36
    "profile",                              // 37
    "CoRIM id",                             // 38
    "CoRIM validity",                       // 39
    "entities",                             // 40
    "entity",                               // 41
    "entity roles",                         // 42
    "entity role",                          // 43
    "entity name",                          // 44
    "entity regid",                         // 45
    "CoMID tag identity",                   // 46
    "CoMID triples",                        // 47
    "CoMID tag id",                         // 48
    "tag version",                          // 49
    "reference triples",                    // 50
    "reference triple",                     // 51
    "measurements",                         // 52
    "attest-key triples",                   // 53
    "attest-key triple",                    // 54
    "attest keys",                          // 55
    "environment",                          // 56
    "class",                                // 57
    "class id",                             // 58
    "class vendor",                         // 59
    "class model",                          // 60
    "class layer",                          // 61
    "class index",                          // 62
    "instance",                             // 63
    "measurement",                          // 64
    "mkey",                                 // 65
    "measurement values",                   // 66
    "version",                              // 67
    "digests",                              // 68
    "digest",                               // 69
    "digest alg",                           // 70
    "digest value",                         // 71
    "raw value",                            // 72
    "masked raw value",                     // 73
    "raw value mask",                       // 74
    "measurement name",                     // 75
    "cryptokeys",                           // 76
    "crypto key",                           // 77
    "pkix-base64-key",                      // 78
    "corim-meta",                           // 79
    "corim-meta signer",                    // 80
    "signer name",                          // 81
    "signer uri",                           // 82
    "signature validity",                   // 83
    "signed CoRIM",                         // 84
];

/// The field index of a malformed CBOR item (cbor::WHAT).
const CBOR_WHAT: u64 = 1000;

/// Reasons of ParseError::Cbor, by the index the port raises.
const CBOR_REASONS: [&str; 60] = [
    "malformed CBOR",                           // 0
    "trailing bytes",                           // 1
    "expected map",                             // 2
    "non-integer claim key",                    // 3
    "duplicate claim key",                      // 4
    "missing claim",                            // 5
    "expected text",                            // 6
    "unsupported hash algorithm",               // 7
    "expected byte string",                     // 8
    "wrong byte-string length",                 // 9
    "length does not match hash algorithm",     // 10
    "expected array",                           // 11
    "expected four registers",                  // 12
    "expected unsigned integer",                // 13
    "too many components",                      // 14
    "too many entries",                         // 15
    "expected name, index and hash",            // 16
    "expected CM or DM",                        // 17
    "expected 0..=5",                           // 18
    "length is not 32, 48 or 64",               // 19
    "unknown key",                              // 20
    "unknown profile",                          // 21
    "wrong tag",                                // 22
    "missing tag 399 or 907",                   // 23
    "expected exactly two tokens",              // 24
    "malformed COSE_Sign1",                     // 25
    "detached payload",                         // 26
    "CMW record is not an array",               // 27
    "CMW content format is not an integer",     // 28
    "CMW content format is not 263",            // 29
    "CMW record is not [type, value]",          // 30
    "malformed tagged COSE_Sign1",              // 31
    "non-integer key",                          // 32
    "duplicate key",                            // 33
    "missing",                                  // 34
    "too many items",                           // 35
    "UUID is not 16 bytes",                     // 36
    "expected text or UUID",                    // 37
    "expected tag-1 integer time",              // 38
    "not-before after not-after",               // 39
    "expected URI",                             // 40
    "expected tagged UUID, OID or bytes",       // 41
    "expected tagged instance",                 // 42
    "empty",                                    // 43
    "out of range",                             // 44
    "unknown algorithm name",                   // 45
    "expected integer or text",                 // 46
    "expected tagged key",                      // 47
    "expected [alg, value]",                    // 48
    "expected [value, mask]",                   // 49
    "expected tag 560 or 563",                  // 50
    "negative integer",                         // 51
    "unsupported type",                         // 52
    "expected [env, measurements]",             // 53
    "expected [env, keys]",                     // 54
    "missing tag 501",                          // 55
    "untagged entry",                           // 56
    "expected tagged URI or OID",               // 57
    "content type is not application/rim+cbor", // 58
    "missing corim-meta",                       // 59
];

/// A failure of a port call that decodes the CBOR item `what`.
pub(crate) fn cbor_parse_error(mut e: vesper::AErr, what: usize) -> ParseError {
    if e.kind == 5 && e.field == CBOR_WHAT {
        e.field = what as u64;
    }
    parse_error(e)
}

/// The CBOR field index of `realm claims` and `platform claims`.
pub(crate) const CBOR_REALM_CLAIMS: usize = 0;
pub(crate) const CBOR_PLATFORM_CLAIMS: usize = 1;

/// High bit set on a chunk offset when bytes live in scratch `s` rather than `input`.
const SCRATCH_BIT: u64 = 1 << 62;
/// Retryable error raised by `cbor::need_scratch` when an indefinite-length
/// byte string must be materialized into scratch.
const K_NEED_SCRATCH: u64 = 6;

/// Run a port call that decodes `input` as CBOR without scratch (`n`, `st`, `w`).
pub(crate) fn cbor_call<T>(
    input: &[u8],
    what: usize,
    run: impl FnOnce(&mut [u64], &mut [u64], &mut [u64]) -> Result<T, vesper::AErr>,
) -> Result<(T, Vec<u64>), ParseError> {
    let mut n = vec![0u64; 4 * input.len() + 8];
    let mut st = [0u64; CBOR_STACK_WORDS];
    // Output words copy node words or add a presence or count word per
    // claim, well within twice the node buffer.
    let mut w = vec![0u64; 2 * n.len() + 512];
    let out = run(&mut n, &mut st, &mut w).map_err(|e| cbor_parse_error(e, what))?;
    let count = w[0] as usize;
    assert!(
        count < w.len(),
        "the port wrote {count} words, more than the adapter's buffer holds"
    );
    w.truncate(count + 1);
    w.remove(0);
    Ok((out, w))
}

/// Run a port call that may need scratch storage for indefinite-length CBOR strings.
pub(crate) fn cbor_call_scratch<T>(
    input: &[u8],
    what: usize,
    mut run: impl FnMut(&mut [u8], &mut [u64], &mut [u64], &mut [u64]) -> Result<T, vesper::AErr>,
) -> Result<(T, Vec<u64>, Vec<u8>), ParseError> {
    let mut s = Vec::new();
    let mut n = vec![0u64; 4 * input.len() + 16];
    let mut st = [0u64; CBOR_STACK_WORDS];
    let mut w = vec![0u64; 2 * n.len() + 512];
    loop {
        n[0] = 0;
        w[0] = 0;
        match run(&mut s, &mut n, &mut st, &mut w) {
            Ok(out) => {
                let count = w[0] as usize;
                assert!(
                    count < w.len(),
                    "the port wrote {count} words, more than the adapter's buffer holds"
                );
                w.truncate(count + 1);
                w.remove(0);
                return Ok((out, w, s));
            }
            Err(e) if e.kind == K_NEED_SCRATCH => {
                let need = (e.a as usize).max(4 * input.len() + 64);
                let new_len = need.max(s.len().saturating_mul(2) + 64);
                s.resize(new_len, 0);
            }
            Err(e) => return Err(cbor_parse_error(e, what)),
        }
    }
}

/// A cursor over the output words of a CBOR call: strings come as their
/// chunks (cbor::put_str) into `input` or `scratch`.
pub(crate) struct CborFields<'a> {
    w: &'a [u64],
    input: &'a [u8],
    scratch: &'a [u8],
}

impl<'a> CborFields<'a> {
    pub(crate) fn new(w: &'a [u64], input: &'a [u8]) -> Self {
        CborFields {
            w,
            input,
            scratch: &[],
        }
    }

    pub(crate) fn with_scratch(w: &'a [u64], input: &'a [u8], scratch: &'a [u8]) -> Self {
        CborFields { w, input, scratch }
    }

    pub(crate) fn word(&mut self) -> u64 {
        cbor_word(&mut self.w)
    }

    /// A string: its chunk count, then each chunk's offset and length.
    pub(crate) fn bytes(&mut self) -> Vec<u8> {
        let k = self.word();
        cbor_chunks_s(&mut self.w, k, self.input, self.scratch)
    }

    pub(crate) fn text(&mut self) -> String {
        String::from_utf8(self.bytes()).expect("the port checked UTF-8")
    }

    pub(crate) fn opt_bytes(&mut self) -> Option<Vec<u8>> {
        (self.word() != 0).then(|| self.bytes())
    }

    pub(crate) fn opt_text(&mut self) -> Option<String> {
        (self.word() != 0).then(|| self.text())
    }

    pub(crate) fn array<const N: usize>(&mut self) -> [u8; N] {
        self.bytes()
            .try_into()
            .expect("the port checked the length")
    }

    pub(crate) fn hash(&mut self) -> crate::claims::RegisterHash {
        use crate::claims::RegisterHash;
        match self.word() {
            0 => RegisterHash::Sha256,
            1 => RegisterHash::Sha384,
            2 => RegisterHash::Sha512,
            h => unreachable!("hash algorithm {h} from the Vesper port"),
        }
    }
}

/// Room for the words of the longest parse (a TDX v5 quote with a TD 1.5
/// "ex" body writes 52), after the count in word 0.
const WORDS: usize = 96;
/// Room for the bytes a parser computes: the SNP signature, `r || s`.
const DERIVED: usize = 96;

/// The output of one successful parse: the words the port wrote, the bytes
/// it computed, and the input whose offsets the words give.
pub(crate) struct Parsed<'a> {
    w: [u64; WORDS],
    d: [u8; DERIVED],
    input: &'a [u8],
}

/// Run a port parser over `input` with fresh output buffers.
pub(crate) fn parse<'a, T>(
    input: &'a [u8],
    run: impl FnOnce(&mut [u64], &mut [u8]) -> Result<T, vesper::AErr>,
) -> Result<(T, Parsed<'a>), ParseError> {
    let mut p = Parsed {
        w: [0; WORDS],
        d: [0; DERIVED],
        input,
    };
    let out = run(&mut p.w, &mut p.d).map_err(parse_error)?;
    assert!(
        p.w[0] < WORDS as u64,
        "the port wrote {} words, more than the adapter's buffer holds",
        p.w[0]
    );
    Ok((out, p))
}

impl Parsed<'_> {
    pub(crate) fn fields(&self) -> Fields<'_> {
        Fields {
            w: &self.w[1..=self.w[0] as usize],
            d: &self.d,
            input: self.input,
        }
    }
}

/// A cursor over [`Parsed`]. The port writes exactly the fields its
/// documentation lists, so running out is a bug in the port, not bad
/// input.
pub(crate) struct Fields<'a> {
    w: &'a [u64],
    d: &'a [u8],
    input: &'a [u8],
}

impl<'a> Fields<'a> {
    pub(crate) fn word(&mut self) -> u64 {
        let (first, rest) = self.w.split_first().expect("the port wrote this word");
        self.w = rest;
        *first
    }

    pub(crate) fn u8(&mut self) -> u8 {
        self.word() as u8
    }

    pub(crate) fn u16(&mut self) -> u16 {
        self.word() as u16
    }

    pub(crate) fn u32(&mut self) -> u32 {
        self.word() as u32
    }

    /// `n` input bytes at the offset the next word gives.
    pub(crate) fn bytes(&mut self, n: usize) -> &'a [u8] {
        let at = self.word() as usize;
        &self.input[at..at + n]
    }

    pub(crate) fn array<const N: usize>(&mut self) -> [u8; N] {
        self.bytes(N).try_into().expect("N bytes")
    }

    /// A variable-length field: its length word, then its offset word.
    pub(crate) fn vec(&mut self) -> Vec<u8> {
        let n = self.word() as usize;
        self.bytes(n).to_vec()
    }

    /// The next `N` bytes the port computed.
    pub(crate) fn derived<const N: usize>(&mut self) -> [u8; N] {
        let (first, rest) = self.d.split_at(N);
        self.d = rest;
        first.try_into().expect("N bytes")
    }
}

/// `component` of `AegisVerifyError::StaleSnpTcbComponent`, by index.
const SNP_COMPONENTS: [&str; 4] = ["bootloader", "tee", "snp", "microcode"];

/// A policy rejection of the port.
pub(crate) fn verify_error(e: vesper::PErr) -> AegisVerifyError {
    use crate::claims::TeeArch;
    let arch = || TeeArch::from_tag(e.a as u16).expect("the port names a known architecture");
    match e.kind {
        0 => AegisVerifyError::DisallowedTeeArchitecture(arch()),
        1 => AegisVerifyError::EnclaveDebugModeEnabled,
        2 => AegisVerifyError::StaleTcbSvn {
            actual: e.a as u16,
            min_required: e.b as u16,
        },
        3 => AegisVerifyError::StaleSnpTcbComponent {
            component: SNP_COMPONENTS[e.a as usize],
            actual: e.b as u8,
            min_required: e.c as u8,
        },
        4 => AegisVerifyError::BootMeasurementMismatch,
        5 => AegisVerifyError::SupplyChainLoomLockMismatch,
        6 => AegisVerifyError::FormalOleanProofDigestMismatch,
        7 => AegisVerifyError::ChannelTranscriptBindingMismatch,
        8 => AegisVerifyError::SupplyChainNotRegisterMeasured(arch()),
        9 => AegisVerifyError::DisallowedVmpl {
            actual: e.a as u32,
            max_allowed: e.b as u32,
        },
        kind => unreachable!("AegisVerifyError variant {kind} from the Vesper port"),
    }
}

/// Decoded CBOR value produced by `__port_cbor_value`.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub enum PortCborValue {
    Integer(i128),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<PortCborValue>),
    Map(Vec<(PortCborValue, PortCborValue)>),
    Tag(u64, Box<PortCborValue>),
    Float(f64),
    Bool(bool),
    Null,
}

/// One CBOR item decoded by the port (`cbor::decode`): the value and the
/// bytes it used, or `None` when the input is not CBOR ciborium accepts.
pub(crate) fn cbor_value(input: &[u8]) -> Option<(PortCborValue, usize)> {
    // Every node takes at least one input byte and at most four words.
    let mut w = vec![0u64; 4 * input.len() + 8];
    let mut st = [0u64; CBOR_STACK_WORDS];
    let used = vesper::cbor_decode(input, &mut w, &mut st).ok()?;
    assert!(
        (w[0] as usize) < w.len(),
        "the port wrote {} CBOR words, more than the adapter's buffer holds",
        w[0]
    );
    let mut nodes = &w[1..=w[0] as usize];
    let v = cbor_node(&mut nodes, input);
    debug_assert!(nodes.is_empty(), "the port wrote one CBOR item");
    Some((v, used as usize))
}

/// The decoder's stack: four words for each of 256 open containers.
const CBOR_STACK_WORDS: usize = 4 * 256;

fn cbor_word(w: &mut &[u64]) -> u64 {
    let (first, rest) = w.split_first().expect("the port wrote this CBOR word");
    *w = rest;
    *first
}

fn cbor_chunks_s(w: &mut &[u64], n: u64, input: &[u8], scratch: &[u8]) -> Vec<u8> {
    if n == 1 {
        let at = cbor_word(w);
        let len = cbor_word(w) as usize;
        return if at >= SCRATCH_BIT {
            let off = (at - SCRATCH_BIT) as usize;
            scratch[off..off + len].to_vec()
        } else {
            let off = at as usize;
            input[off..off + len].to_vec()
        };
    }
    let mut out = Vec::new();
    for _ in 0..n {
        let at = cbor_word(w);
        let len = cbor_word(w) as usize;
        if at >= SCRATCH_BIT {
            let off = (at - SCRATCH_BIT) as usize;
            out.extend_from_slice(&scratch[off..off + len]);
        } else {
            let off = at as usize;
            out.extend_from_slice(&input[off..off + len]);
        }
    }
    out
}

fn cbor_node(w: &mut &[u64], input: &[u8]) -> PortCborValue {
    let kind = cbor_word(w);
    let a = cbor_word(w);
    match kind {
        0 => PortCborValue::Integer(a.into()),
        1 => PortCborValue::Integer(-1 - i128::from(a)),
        2 => PortCborValue::Bytes(cbor_chunks_s(w, a, input, &[])),
        3 => PortCborValue::Text(
            String::from_utf8(cbor_chunks_s(w, a, input, &[])).expect("the port checked UTF-8"),
        ),
        4 => PortCborValue::Array((0..a).map(|_| cbor_node(w, input)).collect()),
        5 => PortCborValue::Map(
            (0..a)
                .map(|_| (cbor_node(w, input), cbor_node(w, input)))
                .collect(),
        ),
        6 => PortCborValue::Tag(a, Box::new(cbor_node(w, input))),
        7 => PortCborValue::Float(f64::from_bits(a)),
        8 => PortCborValue::Bool(a != 0),
        9 => PortCborValue::Null,
        10 => {
            let hi = cbor_word(w);
            let lo = cbor_word(w);
            let raw = (u128::from(hi) << 64 | u128::from(lo)).to_be_bytes();
            let first = raw.iter().position(|&b| b != 0).unwrap_or(raw.len());
            PortCborValue::Tag(a, Box::new(PortCborValue::Bytes(raw[first..].to_vec())))
        }
        k => unreachable!("CBOR node kind {k} from the Vesper port"),
    }
}

pub(crate) fn read_platform_claims(f: &mut CborFields<'_>) -> CcaPlatformClaims {
    let lifecycle = f.word();
    let sw_components = (0..f.word())
        .map(|_| CcaSwComponent {
            measurement_type: f.opt_text(),
            measurement_value: f.bytes(),
            version: f.opt_text(),
            signer_id: f.bytes(),
            measurement_description: f.opt_text(),
        })
        .collect();
    let config = f.opt_bytes();
    let manufacturing_config = f.opt_bytes();
    let tbb_rotpks = (0..f.word())
        .map(|_| CcaTbbRotpk {
            name: f.text(),
            index: f.word(),
            hash: f.bytes(),
        })
        .collect();
    CcaPlatformClaims {
        sw_components,
        config,
        manufacturing_config,
        tbb_rotpks,
        challenge: f.bytes(),
        instance_id: f.bytes(),
        implementation_id: f.bytes(),
        lifecycle,
        hash_algo: f.hash(),
    }
}

pub(crate) fn read_realm_claims(f: &mut CborFields<'_>) -> CcaRealmClaims {
    let hash_algo = f.hash();
    let initial_measurement = f.bytes();
    let extensible_measurements = std::array::from_fn(|_| f.bytes());
    CcaRealmClaims {
        challenge: f.array(),
        personalization: f.array(),
        hash_algo,
        public_key: f.bytes(),
        public_key_hash_algo: f.hash(),
        initial_measurement,
        extensible_measurements,
    }
}

fn read_sign1(f: &mut CborFields<'_>) -> CoseSign1 {
    let protected_raw = f.bytes();
    let alg = match (f.word(), f.word()) {
        (0, _) => CoseAlg::None,
        (1, v) => CoseAlg::Assigned(v as i64),
        (2, v) => CoseAlg::Private(v as i64),
        (3, _) => CoseAlg::Text,
        (k, _) => unreachable!("COSE alg_kind {k} from the Vesper port"),
    };
    let content_type = f.opt_text();
    let has_meta = f.word();
    let meta_is_bytes = f.word();
    let corim_meta = match (has_meta, meta_is_bytes) {
        (0, _) => None,
        (1, 0) => Some(Err(())),
        (1, _) => Some(Ok(f.bytes())),
        _ => unreachable!("corim_meta flags ({has_meta}, {meta_is_bytes})"),
    };
    let payload = f.opt_bytes();
    let signature = f.bytes();
    CoseSign1 {
        protected_raw,
        alg,
        content_type,
        corim_meta,
        payload,
        signature,
    }
}

pub(crate) const CBOR_SIGNED_CORIM: usize = 84;
pub(crate) const CBOR_REASON_MALFORMED_SIGNED_COSE: u64 = 31;

pub(crate) fn cose_sign1_parse(
    bytes: &[u8],
    field: usize,
    reason: u64,
) -> Result<CoseSign1, ParseError> {
    let ((), w, s) = cbor_call_scratch(bytes, field, |s, n, st, w| {
        vesper::cose_sign1_parse(bytes, s, n, st, w, field as u64, reason)
    })?;
    let mut f = CborFields::with_scratch(&w, bytes, &s);
    Ok(read_sign1(&mut f))
}

pub(crate) fn cose_sig_structure(protected: &[u8], aad: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; protected.len() + aad.len() + payload.len() + 48];
    let n = vesper::cose_sig_structure(protected, aad, payload, &mut out) as usize;
    out.truncate(n);
    out
}

pub(crate) fn cose_key_p384(encoded: &[u8]) -> Result<[u8; 97], AttestError> {
    let mut n = vec![0u64; 4 * encoded.len() + 8];
    let mut st = [0u64; CBOR_STACK_WORDS];
    let mut sec1 = [0u8; 97];
    vesper::cose_key_p384(encoded, &mut n, &mut st, &mut sec1)
        .map_err(|e| attest_error(e, encoded))?;
    Ok(sec1)
}

pub(crate) fn cca_token_parse(bytes: &[u8]) -> Result<CcaToken, ParseError> {
    let ((), w, s) = cbor_call_scratch(bytes, 30, |s, n, st, w| {
        vesper::cca_token_parse(bytes, s, n, st, w)
    })?;
    let mut f = CborFields::with_scratch(&w, bytes, &s);
    let collection = match f.word() {
        0 => CcaCollection::Tag399,
        1 => CcaCollection::Cmw907,
        c => unreachable!("collection {c} from the Vesper port"),
    };
    let platform = read_sign1(&mut f);
    let realm = read_sign1(&mut f);
    let platform_claims = read_platform_claims(&mut f);
    let realm_claims = read_realm_claims(&mut f);
    let platform_profile = match f.word() {
        0 => CcaPlatformProfile::CcaSsd1,
        1 => CcaPlatformProfile::Platform2023,
        2 => CcaPlatformProfile::Platform2024,
        p => unreachable!("platform profile {p} from the Vesper port"),
    };
    let realm_profile = match f.word() {
        0 => CcaRealmProfile::Unspecified,
        1 => CcaRealmProfile::Realm2023,
        2 => CcaRealmProfile::Realm2024,
        p => unreachable!("realm profile {p} from the Vesper port"),
    };
    Ok(CcaToken {
        collection,
        platform_profile,
        realm_profile,
        platform,
        platform_claims,
        realm,
        realm_claims,
    })
}

fn read_corim_id(f: &mut CborFields<'_>) -> CorimId {
    match f.word() {
        0 => CorimId::Text(f.text()),
        1 => CorimId::Uuid(f.array::<16>()),
        k => unreachable!("CorimId kind {k}"),
    }
}

fn read_corim_validity(f: &mut CborFields<'_>) -> CorimValidity {
    let not_before = (f.word() != 0).then(|| f.word());
    let not_after = f.word();
    CorimValidity {
        not_before,
        not_after,
    }
}

fn read_crypto_key(f: &mut CborFields<'_>) -> CryptoKey {
    match f.word() {
        0 => CryptoKey::PkixBase64Key(f.text()),
        1 => CryptoKey::Bytes(f.bytes()),
        2 => CryptoKey::Other(f.word()),
        k => unreachable!("CryptoKey kind {k}"),
    }
}

fn read_env(f: &mut CborFields<'_>) -> Environment {
    let mut env = Environment {
        has_class: f.word() != 0,
        ..Environment::default()
    };
    if env.has_class {
        env.class_id = match f.word() {
            0 => None,
            1 => Some(ClassId::Uuid(f.array::<16>())),
            2 => Some(ClassId::Oid(f.bytes())),
            3 => Some(ClassId::Bytes(f.bytes())),
            k => unreachable!("ClassId kind {k}"),
        };
        env.vendor = f.opt_text();
        env.model = f.opt_text();
        env.layer = (f.word() != 0).then(|| f.word());
        env.index = (f.word() != 0).then(|| f.word());
    }
    env.instance = match f.word() {
        0 => None,
        1 => Some(Instance::Ueid(f.bytes())),
        2 => Some(Instance::Uuid(f.array::<16>())),
        3 => Some(Instance::Bytes(f.bytes())),
        4 => Some(Instance::Other(f.word())),
        k => unreachable!("Instance kind {k}"),
    };
    env.has_group = f.word() != 0;
    env
}

fn read_measurement(f: &mut CborFields<'_>) -> Measurement {
    let mkey = match f.word() {
        0 => None,
        1 => Some(Mkey::Text(f.text())),
        2 => Some(Mkey::Uint(f.word())),
        3 => Some(Mkey::Uuid(f.array::<16>())),
        4 => Some(Mkey::Oid(f.bytes())),
        k => unreachable!("Mkey kind {k}"),
    };
    let mut values = MeasurementValues::default();
    let count = f.word();
    for _ in 0..count {
        match f.word() {
            0 => {
                values.version = Some(f.text());
                values.version_has_scheme = f.word() != 0;
            }
            1 => {
                let dc = f.word();
                for _ in 0..dc {
                    let alg = f.word() as i64;
                    let value = f.bytes();
                    values.digests.push(Digest { alg, value });
                }
            }
            2 => {
                let value = f.bytes();
                let mask = f.opt_bytes();
                values.raw_value = Some(RawValue { value, mask });
            }
            3 => values.name = Some(f.text()),
            4 => {
                let kc = f.word();
                values.cryptokeys = (0..kc).map(|_| read_crypto_key(f)).collect();
            }
            5 => values.other_keys.push(f.word() as i64),
            k => unreachable!("mval entry tag {k}"),
        }
    }
    let has_authorized_by = f.word() != 0;
    Measurement {
        mkey,
        values,
        has_authorized_by,
    }
}

fn read_comid(f: &mut CborFields<'_>) -> Comid {
    let tag_id = read_corim_id(f);
    let tag_version = f.word();
    let mut reference_values = Vec::new();
    let mut attest_keys = Vec::new();
    let mut other_triples = Vec::new();
    let tcount = f.word();
    for _ in 0..tcount {
        match f.word() {
            0 => {
                let rc = f.word();
                for _ in 0..rc {
                    let environment = read_env(f);
                    let mc = f.word();
                    let measurements = (0..mc).map(|_| read_measurement(f)).collect();
                    reference_values.push(ReferenceTriple {
                        environment,
                        measurements,
                    });
                }
            }
            1 => {
                let ac = f.word();
                for _ in 0..ac {
                    let environment = read_env(f);
                    let kc = f.word();
                    let keys = (0..kc).map(|_| read_crypto_key(f)).collect();
                    let has_conditions = f.word() != 0;
                    attest_keys.push(AttestKeyTriple {
                        environment,
                        keys,
                        has_conditions,
                    });
                }
            }
            2 => other_triples.push(f.word() as i64),
            k => unreachable!("triple tag {k}"),
        }
    }
    Comid {
        tag_id,
        tag_version,
        reference_values,
        attest_keys,
        other_triples,
    }
}

pub(crate) fn corim_parse(bytes: &[u8]) -> Result<Corim, ParseError> {
    let ((), w, s) = cbor_call_scratch(bytes, 34, |s, n, st, w| {
        vesper::corim_parse(bytes, s, n, st, w)
    })?;
    let mut f = CborFields::with_scratch(&w, bytes, &s);
    let other_tags = f.word() as usize;
    let comid_count = f.word();
    let comids = (0..comid_count).map(|_| read_comid(&mut f)).collect();
    let profile = match f.word() {
        0 => None,
        1 => Some(CorimProfile::Uri(f.text())),
        2 => Some(CorimProfile::Oid(f.bytes())),
        k => unreachable!("CorimProfile kind {k}"),
    };
    let id = read_corim_id(&mut f);
    let validity = (f.word() != 0).then(|| read_corim_validity(&mut f));
    let entity_count = f.word();
    let entities = (0..entity_count)
        .map(|_| {
            let rc = f.word();
            let roles = (0..rc).map(|_| f.word()).collect();
            let name = f.text();
            let regid = f.opt_text();
            CorimEntity { name, regid, roles }
        })
        .collect();
    Ok(Corim {
        id,
        profile,
        validity,
        entities,
        comids,
        other_tags,
        signer: None,
    })
}

pub(crate) fn corim_meta_parse(bytes: &[u8]) -> Result<CorimSigner, ParseError> {
    let ((), w) = cbor_call(bytes, 79, |n, st, w| {
        vesper::corim_meta_parse(bytes, n, st, w)
    })?;
    let mut f = CborFields::new(&w, bytes);
    Ok(CorimSigner {
        name: f.text(),
        uri: f.opt_text(),
        signature_validity: (f.word() != 0).then(|| read_corim_validity(&mut f)),
    })
}

fn push_bytes(b: &mut Vec<u8>, s: &[u8]) -> (u64, u64) {
    let off = b.len() as u64;
    b.extend_from_slice(s);
    (off, s.len() as u64)
}

fn pack_plat_refs(c: &Comid, w: &mut Vec<u64>, b: &mut Vec<u8>) {
    w.push(c.reference_values.len() as u64);
    for t in &c.reference_values {
        w.push(u64::from(t.environment.has_group));
        w.push(u64::from(t.environment.instance.is_some()));
        w.push(u64::from(t.environment.has_class));
        match &t.environment.class_id {
            None => {
                w.push(0);
                w.push(0);
            }
            Some(ClassId::Bytes(bytes)) => {
                w.push(1);
                w.push(bytes.len() as u64);
            }
            Some(_) => {
                w.push(2);
                w.push(0);
            }
        }
        pack_measurements(&t.measurements, false, w, b);
    }
}

fn pack_realm_comid(c: &Comid, w: &mut Vec<u64>, b: &mut Vec<u8>) {
    w.push(c.attest_keys.len() as u64);
    w.push(c.reference_values.len() as u64);
    for t in &c.reference_values {
        w.push(u64::from(t.environment.has_group));
        w.push(u64::from(t.environment.instance.is_some()));
        w.push(u64::from(t.environment.has_class));
        match &t.environment.class_id {
            None => {
                w.push(0);
                w.push(0);
                w.push(0);
            }
            Some(ClassId::Bytes(bytes)) => {
                let (off, len) = push_bytes(b, bytes);
                w.push(1);
                w.push(off);
                w.push(len);
            }
            Some(_) => {
                w.push(2);
                w.push(0);
                w.push(0);
            }
        }
        pack_measurements(&t.measurements, true, w, b);
    }
}

fn pack_measurements(ms: &[Measurement], copy_digests: bool, w: &mut Vec<u64>, b: &mut Vec<u8>) {
    w.push(ms.len() as u64);
    for m in ms {
        w.push(u64::from(m.has_authorized_by));
        match &m.mkey {
            None => {
                w.push(0);
                w.push(0);
                w.push(0);
            }
            Some(Mkey::Text(t)) => {
                let (off, len) = push_bytes(b, t.as_bytes());
                w.push(1);
                w.push(off);
                w.push(len);
            }
            Some(_) => {
                w.push(2);
                w.push(0);
                w.push(0);
            }
        }
        let v = &m.values;
        w.push(u64::from(!v.other_keys.is_empty()));
        w.push(u64::from(v.version.is_some()));
        w.push(u64::from(v.version_has_scheme));
        w.push(v.digests.len() as u64);
        for d in &v.digests {
            w.push(d.alg as u64);
            if copy_digests {
                let (off, len) = push_bytes(b, &d.value);
                w.push(off);
                w.push(len);
            } else {
                w.push(0);
                w.push(d.value.len() as u64);
            }
        }
        match &v.raw_value {
            None => {
                w.extend_from_slice(&[0, 0, 0, 0]);
            }
            Some(rv) => {
                w.push(1);
                w.push(rv.value.len() as u64);
                match &rv.mask {
                    Some(mask) => {
                        w.push(1);
                        w.push(mask.len() as u64);
                    }
                    None => {
                        w.push(0);
                        w.push(0);
                    }
                }
            }
        }
        w.push(u64::from(v.name.is_some()));
        w.push(v.cryptokeys.len() as u64);
        for ck in &v.cryptokeys {
            match ck {
                CryptoKey::Bytes(bytes) => {
                    w.push(0);
                    w.push(bytes.len() as u64);
                }
                CryptoKey::PkixBase64Key(_) => {
                    w.push(1);
                    w.push(0);
                }
                CryptoKey::Other(_) => {
                    w.push(2);
                    w.push(0);
                }
            }
        }
    }
}

fn pack_plat_ak(i: usize, t: &AttestKeyTriple, w: &mut Vec<u64>) {
    w.push(2);
    w.push(i as u64);
    w.push(u64::from(t.has_conditions));
    w.push(u64::from(t.environment.has_group));
    w.push(u64::from(t.environment.has_class));
    match &t.environment.class_id {
        None => {
            w.push(0);
            w.push(0);
        }
        Some(ClassId::Bytes(bytes)) => {
            w.push(1);
            w.push(bytes.len() as u64);
        }
        Some(_) => {
            w.push(2);
            w.push(0);
        }
    }
    match &t.environment.instance {
        None => w.extend_from_slice(&[0, 0, 0]),
        Some(Instance::Ueid(u)) => {
            w.push(1);
            w.push(u.len() as u64);
            w.push(u.first().copied().map(u64::from).unwrap_or(0));
        }
        Some(_) => w.extend_from_slice(&[2, 0, 0]),
    }
    w.push(t.keys.len() as u64);
    let first_pkix = matches!(t.keys.first(), Some(CryptoKey::PkixBase64Key(_)));
    w.push(u64::from(first_pkix));
}

pub(crate) fn corim_validate_cca_and_add(
    store: &mut CcaCorimStore,
    corim: &Corim,
    now_ns: u64,
) -> Result<(), AttestError> {
    let now_sec = unix_seconds(now_ns);
    let total_aks: usize = corim.comids.iter().map(|c| c.attest_keys.len()).sum();
    let mut w = Vec::new();
    let mut b = Vec::new();
    w.push(0);
    match corim.validity {
        None => w.push(0),
        Some(v) => {
            w.push(1);
            w.push(u64::from(v.not_before.is_some()));
            w.push(v.not_before.unwrap_or(0));
            w.push(v.not_after);
        }
    }
    match corim.profile_uri() {
        Some(u) => {
            let (off, len) = push_bytes(&mut b, u.as_bytes());
            w.push(1);
            w.push(off);
            w.push(len);
        }
        None => w.extend_from_slice(&[0, 0, 0]),
    }
    let is_realm = matches!(
        corim.profile_uri(),
        Some(crate::corim::CCA_REALM_ENDORSEMENT_PROFILE)
    );
    let run_comids = is_realm || total_aks == 0;
    w.push(u64::from(run_comids));
    if run_comids {
        w.push(corim.comids.len() as u64);
        for c in &corim.comids {
            if is_realm {
                pack_realm_comid(c, &mut w, &mut b);
            } else {
                pack_plat_refs(c, &mut w, &mut b);
            }
        }
    }
    let mode = vesper::corim_validate_cca(now_sec, &w, &b).map_err(|e| attest_error(e, &b))?;
    let mut next = store.clone();
    if mode == 0 {
        for c in &corim.comids {
            if !run_comids {
                w.clear();
                b.clear();
                w.push(1);
                pack_plat_refs(c, &mut w, &mut b);
                vesper::corim_validate_cca(now_sec, &w, &b).map_err(|e| attest_error(e, &b))?;
            }
            for t in &c.reference_values {
                next.platform.push(build_plat_ref(t));
            }
            for (i, t) in c.attest_keys.iter().enumerate() {
                w.clear();
                pack_plat_ak(i, t, &mut w);
                vesper::corim_validate_cca(now_sec, &w, &[]).map_err(|e| attest_error(e, &[]))?;
                let impl_id = match &t.environment.class_id {
                    Some(ClassId::Bytes(bytes)) => bytes.clone(),
                    _ => unreachable!(),
                };
                let instance_id = match &t.environment.instance {
                    Some(Instance::Ueid(u)) => u.clone(),
                    _ => unreachable!(),
                };
                let pem = match t.keys.as_slice() {
                    [CryptoKey::PkixBase64Key(p)] => p,
                    _ => unreachable!(),
                };
                let cpak = ec_public_key_from_pem(pem)?;
                next.endorsements.push(CcaEndorsement {
                    instance_id,
                    implementation_id: Some(impl_id),
                    cpak,
                });
            }
        }
    } else {
        for c in &corim.comids {
            for t in &c.reference_values {
                next.realm.push(build_realm_ref(t));
            }
        }
    }
    *store = next;
    Ok(())
}

fn build_plat_ref(t: &ReferenceTriple) -> CcaPlatformReference {
    let implementation_id = match &t.environment.class_id {
        Some(ClassId::Bytes(b)) => b.clone(),
        _ => unreachable!(),
    };
    let mut r = CcaPlatformReference {
        implementation_id,
        sw_components: Vec::new(),
        config: None,
        manufacturing_config: None,
        rotpks: Vec::new(),
    };
    for m in &t.measurements {
        let key = match &m.mkey {
            Some(Mkey::Text(s)) => s.as_str(),
            _ => unreachable!(),
        };
        let v = &m.values;
        match key {
            "cca.software-component" => {
                let signer_id = match v.cryptokeys.as_slice() {
                    [CryptoKey::Bytes(b)] => b.clone(),
                    _ => unreachable!(),
                };
                r.sw_components.push(CcaSwReference {
                    name: v.name.clone(),
                    version: v.version.clone(),
                    digests: v.digests.clone(),
                    signer_id,
                });
            }
            "cca.platform-config" => r.config = v.raw_value.clone(),
            "cca.platform-manufacturing-config" => r.manufacturing_config = v.raw_value.clone(),
            k => {
                let (name, idx) = k
                    .strip_prefix("cca.rotpk.")
                    .and_then(|s| s.split_once('.'))
                    .expect("validated rotpk mkey");
                let index = idx.parse().expect("validated rotpk index");
                let hashes = v
                    .cryptokeys
                    .iter()
                    .map(|ck| match ck {
                        CryptoKey::Bytes(b) => b.clone(),
                        _ => unreachable!(),
                    })
                    .collect();
                r.rotpks.push(CcaRotpkReference {
                    name: name.to_owned(),
                    index,
                    hashes,
                });
            }
        }
    }
    r
}

fn build_realm_ref(t: &ReferenceTriple) -> CcaRealmReference {
    let rim = match &t.environment.class_id {
        Some(ClassId::Bytes(b)) => b.clone(),
        _ => unreachable!(),
    };
    let mut r = CcaRealmReference {
        rim,
        rems: Default::default(),
        personalization: None,
    };
    for m in &t.measurements {
        let key = match &m.mkey {
            Some(Mkey::Text(s)) => s.as_str(),
            _ => unreachable!(),
        };
        match key {
            "cca.rem0" => r.rems[0] = Some(m.values.digests[0].value.clone()),
            "cca.rem1" => r.rems[1] = Some(m.values.digests[0].value.clone()),
            "cca.rem2" => r.rems[2] = Some(m.values.digests[0].value.clone()),
            "cca.rem3" => r.rems[3] = Some(m.values.digests[0].value.clone()),
            "cca.rim" => {}
            "cca.rpv" => r.personalization = m.values.raw_value.as_ref().map(|rv| rv.value.clone()),
            _ => unreachable!(),
        }
    }
    r
}

pub(crate) fn cca_appraise_with_options(
    verified: &crate::cca::VerifiedCcaToken,
    store: &CcaCorimStore,
    opts: &CcaAppraisalOptions,
) -> Result<CcaAppraisal, AttestError> {
    let tok = &verified.token;
    let pc = &tok.platform_claims;
    let rc = &tok.realm_claims;
    let mut w = Vec::with_capacity(256);
    let mut b = Vec::with_capacity(2048);

    let push_opt = |w: &mut Vec<u64>, b: &mut Vec<u8>, s: Option<&[u8]>| match s {
        Some(bytes) => {
            let (off, len) = push_bytes(b, bytes);
            w.extend_from_slice(&[1, off, len]);
        }
        None => w.extend_from_slice(&[0, 0, 0]),
    };

    w.push(u64::from(opts.compare_signer_id));
    let (off, len) = push_bytes(&mut b, &pc.implementation_id);
    w.push(off);
    w.push(len);

    w.push(pc.sw_components.len() as u64);
    for sw in &pc.sw_components {
        let (vo, vl) = push_bytes(&mut b, &sw.measurement_value);
        w.extend_from_slice(&[vo, vl]);
        let (so, sl) = push_bytes(&mut b, &sw.signer_id);
        w.extend_from_slice(&[so, sl]);
        push_opt(
            &mut w,
            &mut b,
            sw.measurement_type.as_deref().map(str::as_bytes),
        );
        push_opt(&mut w, &mut b, sw.version.as_deref().map(str::as_bytes));
        push_opt(
            &mut w,
            &mut b,
            sw.measurement_description.as_deref().map(str::as_bytes),
        );
    }
    push_opt(&mut w, &mut b, pc.config.as_deref());
    push_opt(&mut w, &mut b, pc.manufacturing_config.as_deref());
    w.push(pc.tbb_rotpks.len() as u64);
    for r in &pc.tbb_rotpks {
        let (no, nl) = push_bytes(&mut b, r.name.as_bytes());
        w.extend_from_slice(&[no, nl, r.index]);
        let (ho, hl) = push_bytes(&mut b, &r.hash);
        w.extend_from_slice(&[ho, hl]);
    }

    let (ro, rl) = push_bytes(&mut b, &rc.initial_measurement);
    w.extend_from_slice(&[ro, rl]);
    for rem in &rc.extensible_measurements {
        let (mo, ml) = push_bytes(&mut b, rem);
        w.extend_from_slice(&[mo, ml]);
    }
    let (po, pl) = push_bytes(&mut b, &rc.personalization);
    w.extend_from_slice(&[po, pl]);

    w.push(store.platform.len() as u64);
    for p in &store.platform {
        let body_slot = w.len();
        w.push(0);
        let body_start = w.len();
        let (io, il) = push_bytes(&mut b, &p.implementation_id);
        w.extend_from_slice(&[io, il]);
        w.push(p.sw_components.len() as u64);
        for sw in &p.sw_components {
            push_opt(&mut w, &mut b, sw.name.as_deref().map(str::as_bytes));
            push_opt(&mut w, &mut b, sw.version.as_deref().map(str::as_bytes));
            let (so, sl) = push_bytes(&mut b, &sw.signer_id);
            w.extend_from_slice(&[so, sl]);
            w.push(sw.digests.len() as u64);
            for d in &sw.digests {
                let (doff, dlen) = push_bytes(&mut b, &d.value);
                w.extend_from_slice(&[d.alg as u64, doff, dlen]);
            }
        }
        push_opt(
            &mut w,
            &mut b,
            p.config.as_ref().map(|r| r.value.as_slice()),
        );
        push_opt(
            &mut w,
            &mut b,
            p.manufacturing_config.as_ref().map(|r| r.value.as_slice()),
        );
        w.push(p.rotpks.len() as u64);
        for r in &p.rotpks {
            let (no, nl) = push_bytes(&mut b, r.name.as_bytes());
            w.extend_from_slice(&[no, nl, r.index, r.hashes.len() as u64]);
            for h in &r.hashes {
                let (ho, hl) = push_bytes(&mut b, h);
                w.extend_from_slice(&[ho, hl]);
            }
        }
        w[body_slot] = (w.len() - body_start) as u64;
    }

    w.push(store.realm.len() as u64);
    for r in &store.realm {
        let (ro, rl) = push_bytes(&mut b, &r.rim);
        w.extend_from_slice(&[ro, rl]);
        for rem in &r.rems {
            push_opt(&mut w, &mut b, rem.as_deref());
        }
        push_opt(&mut w, &mut b, r.personalization.as_deref());
    }

    let mut w_out = [0u64; 16];
    vesper::cca_appraise(&w, &b, &mut w_out).map_err(|e| attest_error(e, &b))?;
    assert_eq!(w_out[0], 11);
    Ok(CcaAppraisal {
        platform_reference: w_out[1] as usize,
        sw_components_matched: w_out[2] as usize,
        config_matched: w_out[3] != 0,
        manufacturing_config_matched: w_out[4] != 0,
        rotpks_matched: w_out[5] as usize,
        realm_reference: w_out[6] as usize,
        rems_constrained: [w_out[7] != 0, w_out[8] != 0, w_out[9] != 0, w_out[10] != 0],
        personalization_matched: w_out[11] != 0,
    })
}

fn attest_error(e: vesper::AtErr, b_in: &[u8]) -> AttestError {
    match e.kind {
        0 => AttestError::CollateralExpired {
            what: match e.what {
                0 => "CoRIM validity",
                w => unreachable!("CollateralExpired what {w}"),
            },
        },
        1 => {
            let prefix = || match e.what {
                1 => format!("platform reference value {}", e.index),
                2 => format!("platform attestation key {}", e.index),
                3 => format!("realm reference value {}", e.index),
                w => unreachable!("profile prefix {w}"),
            };
            let bstr = || {
                std::str::from_utf8(&b_in[e.off as usize..(e.off + e.len) as usize])
                    .expect("UTF-8 mkey")
            };
            let dig_what = || {
                if e.len > 0 {
                    format!("{} {}", prefix(), bstr())
                } else {
                    prefix()
                }
            };
            let reason = match e.reason {
                0 => "not a CCA platform or realm endorsement profile".into(),
                1 => "realm CoMID with attestation keys".into(),
                2 => format!("{}: instance/group not allowed", prefix()),
                3 => format!("{}: environment.class is required", prefix()),
                4 => format!("{}: class id is required", prefix()),
                5 => format!("{}: class id must be tagged bytes", prefix()),
                6 => format!("{}: implementation-id must be 32 bytes", prefix()),
                7 => format!("measurement {}: mkey is mandatory", e.index),
                8 => format!("measurement {}: mkey must be a string", e.index),
                9 => {
                    const FIELDS_LIST: [&str; 5] =
                        ["version", "digests", "raw-value", "name", "cryptokeys"];
                    format!(
                        "{}: {} is not allowed here",
                        prefix(),
                        FIELDS_LIST[e.extra as usize]
                    )
                }
                10 => format!("{}: digests field is mandatory", dig_what()),
                11 => format!("{}: exactly one digest required", dig_what()),
                12 => format!("{}: digest length vs algorithm", dig_what()),
                13 => format!(
                    "{}: unsupported digest algorithm {}",
                    dig_what(),
                    e.extra as i64
                ),
                14 => format!("{}: version-scheme must not be present", prefix()),
                15 => format!("{}: signer-id length", prefix()),
                16 => format!("{}: signer-id is mandatory", prefix()),
                17 => format!("{}: cryptokeys (signer-id) must be of type bytes", prefix()),
                18 => format!("{}: exactly one signer-id", prefix()),
                19 => format!("{}: raw-value is mandatory", bstr()),
                20 => format!("{}: raw-value and mask lengths differ or are empty", bstr()),
                21 => format!("{}: raw-value must be masked (tag 563)", bstr()),
                22 => format!("{}: duplicate {}", prefix(), bstr()),
                23 => format!("{}: invalid mkey {:?}", prefix(), bstr()),
                24 => format!("{}: RoTPK keys count", prefix()),
                25 => format!("{}: RoTPK keys must be hash bytes", prefix()),
                26 => format!(
                    "{}: at least one software component measurement is required",
                    prefix()
                ),
                27 => format!("{}: instance-id must be 33 bytes starting 0x01", prefix()),
                28 => format!("{}: instance-id must be of type ueid", prefix()),
                29 => format!(
                    "{}: environment.instance (instance-id) is required",
                    prefix()
                ),
                30 => format!(
                    "{}: verification-key must be of type pkix-base64-key",
                    prefix()
                ),
                31 => format!("{}: exactly one verification key", prefix()),
                32 => format!(
                    "{}: environment.class is required for CCA Realm profile",
                    prefix()
                ),
                33 => format!("{}: RIM must be 32, 48 or 64 bytes", prefix()),
                34 => format!("{}: RIM must be of type bytes", prefix()),
                35 => format!("{}: class id (RIM) is required", prefix()),
                36 => format!("{}: duplicate cca.rim", prefix()),
                37 => format!("{}: cca.rim differs from the class id", prefix()),
                38 => format!("{}: raw-value is mandatory for cca.rpv", prefix()),
                39 => format!("{}: cca.rpv length", prefix()),
                40 => format!("{}: duplicate cca.rpv", prefix()),
                41 => format!(
                    "{}: RIM (cca.rim) measurement is mandatory but not found",
                    prefix()
                ),
                r => unreachable!("Collateral reason {r}"),
            };
            AttestError::Collateral {
                what: "CCA CoRIM profile",
                reason,
            }
        }
        2 => AttestError::UnsupportedReferenceValue {
            what: match e.what {
                0 => "CCA measurement with authorized-by",
                1 => "CCA measurement with measurement-value keys this verifier does not evaluate",
                2 => "CCA attestation-key triple with conditions or group",
                3 => "masked cca.rpv",
                4 => "software component measurement description (hash algorithm)",
                w => unreachable!("UnsupportedReferenceValue what {w}"),
            },
        },
        3 => AttestError::ReferenceValueMissing {
            what: match e.what {
                0 => "CCA platform reference values for the implementation ID",
                1 => "CCA platform token has no software components",
                2 => "CCA platform configuration",
                3 => "CCA platform manufacturing configuration",
                4 => "CCA TBB RoTPK claim",
                5 => "CCA realm reference values for the RIM",
                w => unreachable!("ReferenceValueMissing what {w}"),
            },
        },
        4 => AttestError::ReferenceValueMismatch {
            what: match e.what {
                0 => "CCA platform software component",
                1 => "CCA platform configuration",
                2 => "CCA platform manufacturing configuration",
                3 => "CCA TBB RoTPK",
                4 => "CCA realm personalization value",
                5 => "CCA realm extensible measurement",
                w => unreachable!("ReferenceValueMismatch what {w}"),
            },
        },
        5 => AttestError::UnexpectedKeyType {
            what: match e.what {
                0 => "RAK is neither COSE_Key nor SEC1",
                1 => "RAK COSE_Key kty is not EC2",
                2 => "RAK COSE_Key curve is not P-384",
                3 => "RAK COSE_Key coordinate",
                w => unreachable!("UnexpectedKeyType what {w}"),
            },
        },
        k => unreachable!("AtErr kind {k} from the Vesper port"),
    }
}
