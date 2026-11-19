//! The Vesper build of QPACK: [`Encoder`] and [`Decoder`] forward to the
//! Vesper library `h3` (`vesper/`, port wave P3b lane 5, #1067) through the
//! generated binding (`port/binding.rs`). Only representation adapters live
//! here: field lists are flattened into one byte buffer and three words per
//! field on the way in, and decoded sections are read back from the
//! decoder's output buffers, which also carry the ready-queue events
//! (sections that became ready, and cancellations) of each call.

use std::collections::VecDeque;
use std::sync::OnceLock;

use super::{Decoded, DecoderConfig, EncoderConfig, EncoderStats, Field, FieldSection, QpackError};

/// The types at the library boundary (`--rust-types`).
pub(crate) mod abi {
    pub use crate::qpack::huffman::HuffmanError;
    pub use crate::qpack::prefix::PrefixError;
    pub use crate::qpack::{DecoderConfig, EncoderConfig, EncoderStats, QpackError, Reason};

    pub use crate::capsule::CapsuleError;
    pub use crate::frame::{FrameError, Settings};
    pub use crate::message::MessageError;

    /// A byte range of a flattened field list.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Span {
        pub lo: u64,
        pub hi: u64,
    }

    /// A validated request head, as ranges of the flattened field list;
    /// the first `pseudo` fields are the pseudo-headers.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct RequestParts {
        pub method: Span,
        pub scheme: Span,
        pub authority: Option<Span>,
        pub path: Span,
        pub protocol: Option<Span>,
        pub pseudo: u64,
        pub content_length: Option<u64>,
    }

    /// A validated response head.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ResponseParts {
        pub status: u16,
        pub pseudo: u64,
        pub content_length: Option<u64>,
    }
    pub use crate::priority::{Priority, PriorityUpdate};

    /// A frame from the reader: [`crate::frame::Frame`] without its
    /// payloads, which stay in the reader's output buffer.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FrameOut {
        None,
        Data,
        Headers,
        SettingsFrame(Settings),
        GoAway(u64),
        CancelPush(u64),
        MaxPushId(u64),
        PushPromise,
        PriorityUpdateFrame(PriorityUpdate),
        WebTransportStream(u64),
        Unknown(u64),
    }

    /// A capsule from the reader; its bytes stay in the reader's output
    /// buffer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum CapsuleOut {
        None,
        Datagram,
        CloseSession(u32),
        DrainSession,
        Unknown(u64),
    }

    /// A decoded prefixed integer and the bytes it used.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct IntRead {
        pub value: u64,
        pub used: u64,
    }
}

// Generated and committed, but kept outside src/ so formatters, which
// check every file under src/, never reflow it (see the porting recipe).
#[allow(unsafe_code, missing_docs, dead_code, clippy::all)]
#[rustfmt::skip]
#[path = "../../port/binding.rs"]
pub(crate) mod binding;

/// The Huffman and static tables every codec call reads, built once (the
/// Rust build keeps them in statics).
pub(crate) fn huff() -> &'static binding::Huff {
    static TABLES: OnceLock<binding::Huff> = OnceLock::new();
    TABLES.get_or_init(binding::Huff::new)
}

/// Allocations the Vesper library holds (for leak tests).
pub fn live_allocations() -> i64 {
    binding::live_allocations()
}

/// The QPACK encoder (RFC 9204 §2.1). See the Rust build for the policy;
/// this is the same encoder in Vesper.
pub struct Encoder {
    core: binding::EncoderCore,
    bytes: Vec<u8>,
    meta: Vec<u64>,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Self {
        Self {
            core: binding::EncoderCore::new(cfg),
            bytes: Vec::new(),
            meta: Vec::new(),
        }
    }

    /// Applies the peer's SETTINGS_QPACK_MAX_TABLE_CAPACITY and
    /// SETTINGS_QPACK_BLOCKED_STREAMS.
    pub fn apply_peer_settings(&mut self, max_table_capacity: u64, blocked_streams: u64) {
        self.core
            .apply_peer_settings(max_table_capacity, blocked_streams)
    }

    pub fn stats(&self) -> EncoderStats {
        self.core.as_ref().stats()
    }

    pub fn insert_count(&self) -> u64 {
        self.core.insert_count()
    }

    pub fn known_received_count(&self) -> u64 {
        self.core.as_ref().known_received()
    }

    pub fn table_capacity(&self) -> u64 {
        self.core.table_capacity()
    }

    pub fn outstanding_sections(&self) -> usize {
        self.core.outstanding_sections() as usize
    }

    pub fn blocking_streams(&self) -> usize {
        self.core.blocking_streams() as usize
    }

    /// Encodes one field section for `stream_id`.
    pub fn encode(&mut self, stream_id: u64, fields: &[Field]) -> Vec<u8> {
        self.bytes.clear();
        self.meta.clear();
        for f in fields {
            self.bytes.extend_from_slice(&f.name);
            self.bytes.extend_from_slice(&f.value);
            self.meta.extend_from_slice(&[
                f.name.len() as u64,
                f.value.len() as u64,
                u64::from(f.sensitive),
            ]);
        }
        self.core.encode(huff(), stream_id, &self.bytes, &self.meta)
    }

    /// Feeds bytes from the peer's decoder stream.
    pub fn on_decoder_stream(&mut self, bytes: &[u8]) -> Result<(), QpackError> {
        self.core.on_decoder_stream(bytes)
    }

    /// Encoder-stream bytes to send.
    pub fn take_encoder_stream(&mut self) -> Vec<u8> {
        self.core.take_encoder_stream()
    }
}

/// The QPACK decoder (RFC 9204 §2.2), in Vesper. Sections that unblock wait
/// here, in the same queue the Rust build keeps.
pub struct Decoder {
    core: binding::DecoderCore,
    ready: VecDeque<Result<FieldSection, QpackError>>,
}

impl Decoder {
    pub fn new(cfg: DecoderConfig) -> Self {
        Self {
            core: binding::DecoderCore::new(cfg),
            ready: VecDeque::new(),
        }
    }

    pub fn config(&self) -> DecoderConfig {
        self.core.as_ref().cfg()
    }

    pub fn insert_count(&self) -> u64 {
        self.core.insert_count()
    }

    pub fn table_size(&self) -> u64 {
        self.core.table_size()
    }

    pub fn table_capacity(&self) -> u64 {
        self.core.table_capacity()
    }

    pub fn table_len(&self) -> usize {
        self.core.table_len() as usize
    }

    pub fn blocked_streams(&self) -> usize {
        self.core.blocked_streams() as usize
    }

    /// Feeds encoder-stream bytes; sections they unblock become ready.
    pub fn on_encoder_stream(&mut self, bytes: &[u8]) -> Result<(), QpackError> {
        let r = self.core.on_encoder_stream(huff(), bytes);
        self.read_out();
        r
    }

    /// Decodes one field section, or holds it until the entries it needs
    /// arrive.
    pub fn decode_section(&mut self, stream_id: u64, data: &[u8]) -> Result<Decoded, QpackError> {
        let r = self.core.decode_section(huff(), stream_id, data);
        let section = self.read_out();
        match r? {
            true => Ok(Decoded::Fields(
                section.expect("the port records the decoded section"),
            )),
            false => Ok(Decoded::Blocked),
        }
    }

    /// The next section that unblocked (or failed the size limit) since.
    pub fn poll_unblocked(&mut self) -> Option<Result<FieldSection, QpackError>> {
        self.ready.pop_front()
    }

    /// Drops a stream's blocked or ready sections and tells the encoder.
    pub fn cancel_stream(&mut self, stream_id: u64) {
        self.core.cancel_stream(stream_id);
        self.read_out();
    }

    /// Decoder-stream bytes to send.
    pub fn take_decoder_stream(&mut self) -> Vec<u8> {
        self.core.take_decoder_stream()
    }

    /// Applies the records of the last call (see vesper/src/decoder.ves),
    /// returning the section `decode_section` produced, if any.
    fn read_out(&mut self) -> Option<FieldSection> {
        let core = self.core.as_ref();
        let sec = core.sec();
        let heads = sec.heads();
        let heads = heads.as_slice();
        let meta = sec.fields();
        let meta = meta.as_slice();
        let bytes = sec.bytes();
        let bytes = bytes.as_slice();
        let (mut at, mut field) = (0usize, 0usize);
        let mut direct = None;
        for &[kind, stream_id, ric, count] in heads.as_chunks::<4>().0 {
            match kind {
                0 | 3 => {
                    let mut fields = Vec::with_capacity(count as usize);
                    for &[n, v, sensitive] in meta[field * 3..(field + count as usize) * 3]
                        .as_chunks::<3>()
                        .0
                    {
                        let (n, v) = (n as usize, v as usize);
                        fields.push(Field {
                            name: bytes[at..at + n].to_vec(),
                            value: bytes[at + n..at + n + v].to_vec(),
                            sensitive: sensitive != 0,
                        });
                        at += n + v;
                    }
                    field += count as usize;
                    let section = FieldSection {
                        stream_id,
                        fields,
                        required_insert_count: ric,
                    };
                    if kind == 0 {
                        self.ready.push_back(Ok(section));
                    } else {
                        direct = Some(section);
                    }
                }
                1 => self
                    .ready
                    .push_back(Err(QpackError::FieldSectionTooLarge { stream_id })),
                _ => self
                    .ready
                    .retain(|r| !matches!(r, Ok(s) if s.stream_id == stream_id)),
            }
        }
        direct
    }
}
