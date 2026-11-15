//! Vesper Chroma (design §43): semantic, substructural syntax highlighting.
//!
//! * Tier 0 ([`highlight_tier0`]) classifies the lossless `vesper-syntax`
//!   CST of any snippet, complete or not, with no regular expressions and no
//!   grammar files. Every source byte, trivia included, belongs to exactly
//!   one [`HighlightSpan`] ([`verify_lossless_spans`]).
//!   [`highlight_tier0_tokens`] is the same classifier over the lexer's
//!   lossless token stream without building the CST (the wasm module's path).
//! * Tier 1 ([`highlight_tier1`]) decorates the Tier 0 spans with facts the
//!   checker produced for a snippet that checks ([`SemanticFacts`]): live
//!   `Mode::Linear` bindings from minting site to discharge, mutated versus
//!   read-only FieldTree leaves, and `linear effect` capabilities versus
//!   abortable effects. This crate does not run the checker: the facts are a
//!   transport DTO filled by the `vesper` CLI. They are evidence of what the
//!   checker reported, not a proof that the source typing rules hold.
//! * [`ChromaPalette`] defines Vesper Dusk and Vesper Dawn in OKLCH with sRGB
//!   fallbacks and audits them for WCAG 2.x contrast ([`ChromaPalette::verify_contrast_gates`])
//!   and APCA Lc ([`color::apca_lc`]).
//! * [`wire::ChromaTokenSpanWire`] is the 16-byte little-endian record (§43.2)
//!   shared with Lens and `vesper-chroma.wasm`.
//! * [`html`], [`ansi`], [`json`] and [`css`] render the spans.
//!
//! The `VesperTokenClass` discriminants are frozen: Lens and the wasm module
//! share them through the wire record.
#![deny(unsafe_code)]

pub mod ansi;
pub mod color;
pub mod css;
mod facts;
pub mod html;
pub mod json;
mod palette;
#[cfg(feature = "shot")]
pub mod shot;
mod tier0;
pub mod wire;

#[cfg(target_arch = "wasm32")]
mod wasm_abi;

pub use facts::{
    EffectFact, FallbackReason, FieldLeafFact, Highlight, HoverEntry, LinearBindingFact,
    SemanticFacts, Tier, highlight_tier1, tier0_fallback,
};
pub use palette::{ChromaPalette, ContrastAudit, SwatchAudit, SwatchRole, ThemeSwatch};
pub use tier0::{
    DEFAULT_MAX_SOURCE_BYTES, highlight_tier0, highlight_tier0_tokens,
    highlight_tier0_tokens_with_limit, highlight_tier0_with_limit,
};

/// Token classes of design §43.5. The `repr(u16)` values are part of the
/// wire format and never change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum VesperTokenClass {
    KeywordControl = 0,
    ModalSpecifier = 1,
    PassModeIn = 2,
    PassModeInOut = 3,
    PassModeSink = 4,
    LinearBinding = 5,
    FieldLeafMut = 6,
    FieldLeafRead = 7,
    EffectLinear = 8,
    EffectAbortable = 9,
    TypeNominal = 10,
    WireEndianScalar = 11,
    FunctionIdent = 12,
    LiteralScalar = 13,
    DocProofAnchor = 14,
    PunctLigature = 15,
    CommentTrivia = 16,
    Whitespace = 17,
}

impl VesperTokenClass {
    /// Every class, in discriminant order.
    pub const ALL: [Self; 18] = [
        Self::KeywordControl,
        Self::ModalSpecifier,
        Self::PassModeIn,
        Self::PassModeInOut,
        Self::PassModeSink,
        Self::LinearBinding,
        Self::FieldLeafMut,
        Self::FieldLeafRead,
        Self::EffectLinear,
        Self::EffectAbortable,
        Self::TypeNominal,
        Self::WireEndianScalar,
        Self::FunctionIdent,
        Self::LiteralScalar,
        Self::DocProofAnchor,
        Self::PunctLigature,
        Self::CommentTrivia,
        Self::Whitespace,
    ];

    /// Decodes a wire discriminant.
    pub fn from_u16(raw: u16) -> Result<Self, ChromaError> {
        Self::ALL
            .get(usize::from(raw))
            .copied()
            .ok_or(ChromaError::UnknownTokenClass { raw })
    }

    /// Stable lower-case name, used by `--json` and the CSS class names.
    pub fn name(self) -> &'static str {
        match self {
            Self::KeywordControl => "keyword-control",
            Self::ModalSpecifier => "modal-specifier",
            Self::PassModeIn => "pass-mode-in",
            Self::PassModeInOut => "pass-mode-inout",
            Self::PassModeSink => "pass-mode-sink",
            Self::LinearBinding => "linear-binding",
            Self::FieldLeafMut => "field-leaf-mut",
            Self::FieldLeafRead => "field-leaf-read",
            Self::EffectLinear => "effect-linear",
            Self::EffectAbortable => "effect-abortable",
            Self::TypeNominal => "type-nominal",
            Self::WireEndianScalar => "wire-endian-scalar",
            Self::FunctionIdent => "function-ident",
            Self::LiteralScalar => "literal-scalar",
            Self::DocProofAnchor => "doc-proof-anchor",
            Self::PunctLigature => "punct-ligature",
            Self::CommentTrivia => "comment-trivia",
            Self::Whitespace => "whitespace",
        }
    }
}

/// `semantic_flags` bits of the wire record (§43.2). Bits 0-2 are the design's;
/// bit 3 is an additive extension.
pub mod flags {
    /// Live `Mode::Linear` obligation: draw the underline.
    pub const LINEAR_UNDERLINE: u16 = 1 << 0;
    /// A FieldTree leaf written through an `inout` projection.
    pub const MUTATED_LEAF: u16 = 1 << 1;
    /// A multi-character operator a programming font may draw as a ligature.
    pub const LIGATURE: u16 = 1 << 2;
    /// The token sits in a lexer error or a syntax-recovery region.
    pub const RECOVERED: u16 = 1 << 3;
    /// Every bit this version defines; decoders reject the others.
    pub const KNOWN: u16 = LINEAR_UNDERLINE | MUTATED_LEAF | LIGATURE | RECOVERED;
}

/// One classified, contiguous byte range of the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub class: VesperTokenClass,
    /// Live linear obligation (wire flag bit 0).
    pub is_live_linear: bool,
    /// The other [`flags`] bits; bit 0 is carried by `is_live_linear`.
    pub flags: u16,
    /// Index into the hover table of the [`Highlight`] it belongs to.
    pub hover_sym_id: Option<u32>,
}

impl HighlightSpan {
    pub fn new(start: usize, end: usize, class: VesperTokenClass) -> Self {
        Self {
            start,
            end,
            class,
            is_live_linear: false,
            flags: 0,
            hover_sym_id: None,
        }
    }
    /// All wire flag bits, including the linear underline.
    pub fn semantic_flags(&self) -> u16 {
        (self.flags & !flags::LINEAR_UNDERLINE)
            | if self.is_live_linear {
                flags::LINEAR_UNDERLINE
            } else {
                0
            }
    }
    pub fn len(&self) -> usize {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Typed failures. None of the public entry points panic on untrusted input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChromaError {
    DiscontiguousTokenSpan {
        expected_offset: usize,
        actual_start: usize,
    },
    IncompleteSourceCoverage {
        covered_end: usize,
        src_len: usize,
    },
    InsufficientContrast {
        srgb_hex: u32,
    },
    /// The source exceeds the highlighter's byte budget.
    SourceTooLarge {
        len: usize,
        limit: usize,
    },
    /// The parser's CST did not reproduce the source (a parser bug; reported,
    /// never papered over).
    CstMismatch {
        cst_len: usize,
        src_len: usize,
    },
    /// A wire buffer whose length is not a multiple of 16 bytes.
    InvalidWireLength {
        len: usize,
    },
    UnknownTokenClass {
        raw: u16,
    },
    UnknownSemanticFlags {
        raw: u16,
    },
    /// A span offset or length does not fit the 32-bit wire fields.
    WireFieldOverflow {
        start: usize,
        end: usize,
    },
    /// A semantic fact names a range outside the source or off a UTF-8
    /// character boundary.
    InvalidFact {
        start: usize,
        end: usize,
    },
    /// More hover entries than the wire's 32-bit index can address.
    HoverTableOverflow {
        len: usize,
    },
    /// A swatch's OKLCH coordinates do not reproduce its sRGB fallback within
    /// one 8-bit step per channel.
    OklchFallbackMismatch {
        srgb_hex: u32,
        computed_hex: u32,
    },
}

impl core::fmt::Display for ChromaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DiscontiguousTokenSpan {
                expected_offset,
                actual_start,
            } => write!(
                f,
                "span starts at byte {actual_start}, expected {expected_offset}"
            ),
            Self::IncompleteSourceCoverage {
                covered_end,
                src_len,
            } => write!(f, "spans cover {covered_end} of {src_len} source bytes"),
            Self::InsufficientContrast { srgb_hex } => {
                write!(f, "#{srgb_hex:06X} fails its contrast gate")
            }
            Self::SourceTooLarge { len, limit } => {
                write!(f, "source has {len} bytes; the limit is {limit}")
            }
            Self::CstMismatch { cst_len, src_len } => write!(
                f,
                "CST text has {cst_len} bytes but the source has {src_len}"
            ),
            Self::InvalidWireLength { len } => {
                write!(f, "wire buffer of {len} bytes is not a multiple of 16")
            }
            Self::UnknownTokenClass { raw } => write!(f, "unknown token class {raw}"),
            Self::UnknownSemanticFlags { raw } => write!(f, "unknown semantic flags {raw:#06x}"),
            Self::WireFieldOverflow { start, end } => {
                write!(f, "span {start}..{end} does not fit the 32-bit wire fields")
            }
            Self::InvalidFact { start, end } => {
                write!(f, "semantic fact range {start}..{end} is not in the source")
            }
            Self::HoverTableOverflow { len } => write!(f, "hover table has {len} entries"),
            Self::OklchFallbackMismatch {
                srgb_hex,
                computed_hex,
            } => write!(
                f,
                "OKLCH coordinates give #{computed_hex:06X}, fallback is #{srgb_hex:06X}"
            ),
        }
    }
}

impl std::error::Error for ChromaError {}

/// Checks that `spans` partition `0..src_len`: they start at 0, each starts
/// where the previous one ended, none runs backwards, and the last ends at
/// `src_len`. `lean/ChromaHighlightSafety.lean` proves the arithmetic fact
/// this establishes (the span lengths sum to `src_len`) for the same model.
pub fn verify_lossless_spans(src_len: usize, spans: &[HighlightSpan]) -> Result<(), ChromaError> {
    let mut cursor = 0usize;
    for sp in spans {
        if sp.start != cursor || sp.end < sp.start {
            return Err(ChromaError::DiscontiguousTokenSpan {
                expected_offset: cursor,
                actual_start: sp.start,
            });
        }
        cursor = sp.end;
    }
    if cursor != src_len {
        return Err(ChromaError::IncompleteSourceCoverage {
            covered_end: cursor,
            src_len,
        });
    }
    Ok(())
}
