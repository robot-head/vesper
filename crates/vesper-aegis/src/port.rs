//! The port-backed build's glue (cfg(vesper_port)): runs the parsers and
//! the policy check of the Vesper library `aegis` (`vesper/src/lib.ves`)
//! and converts their results and failures back to the crate's own types.
//! Each parser fills `w` with the integer fields it reads and `d` with the
//! byte arrays, both in reading order; [`Fields`] reads them back in the
//! same order. The Rust bodies these replace are the seed copy's.
use crate::error::{AegisVerifyError, Format, ParseError};
pub(crate) use crate::port_binding as vesper;

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
pub(crate) const FIELDS: [&str; 129] = [
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
        kind => unreachable!("ParseError variant {kind} from the Vesper port"),
    }
}

/// The two field streams of one successful parse.
pub(crate) struct Parsed {
    w: Vec<u64>,
    d: Vec<u8>,
}

/// Run a port parser over fresh streams.
pub(crate) fn parse<T>(
    run: impl FnOnce(&mut Vec<u64>, &mut Vec<u8>) -> Result<T, vesper::AErr>,
) -> Result<(T, Parsed), ParseError> {
    let mut p = Parsed {
        w: Vec::new(),
        d: Vec::new(),
    };
    let out = run(&mut p.w, &mut p.d).map_err(parse_error)?;
    Ok((out, p))
}

impl Parsed {
    pub(crate) fn fields(&self) -> Fields<'_> {
        Fields {
            w: &self.w,
            d: &self.d,
        }
    }
}

/// A cursor over [`Parsed`]. The port writes exactly the fields its
/// documentation lists, so running out is a bug in the port, not bad
/// input.
pub(crate) struct Fields<'a> {
    w: &'a [u64],
    d: &'a [u8],
}

impl Fields<'_> {
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

    pub(crate) fn bytes(&mut self, n: usize) -> &[u8] {
        let (first, rest) = self.d.split_at(n);
        self.d = rest;
        first
    }

    pub(crate) fn array<const N: usize>(&mut self) -> [u8; N] {
        self.bytes(N).try_into().expect("N bytes")
    }

    /// A variable-length field: its length word, then its bytes.
    pub(crate) fn vec(&mut self) -> Vec<u8> {
        let n = self.word() as usize;
        self.bytes(n).to_vec()
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
