//! The port-backed build's adapter (cfg(vesper_port)): the crate's public
//! types and borrowed views stay in Rust, and each scalar codec forwards to
//! the Vesper port in `vesper/` through the generated binding
//! (`port/binding.rs`). Only this module and the binding see the port.
use crate::port_binding as vp;
use std::sync::OnceLock;

/// The types the binding converts (`Port::rust_types("crate::port_types")`):
/// the crate's own boundary types, plus the port's result records.
pub(crate) mod types {
    pub(crate) use crate::index::Token;
    pub(crate) use crate::{Error, ErrorKind, Kind, Limits};

    pub(crate) use crate::archive::Limits as ArchiveLimits;
    pub(crate) use crate::encode::EncodeLimits;
    pub(crate) use crate::helpers::SwarFieldMatcher as Swar;
    pub(crate) use crate::streamvbyte::Limits as VbyteLimits;

    /// A decoded binary64 (vesper/src/lib.ves `Decoded`).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Decoded {
        pub path: u64,
        pub bits: u64,
    }

    /// `compute_escaped_mask_64`'s pair.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Escaped {
        pub mask: u64,
        pub carry: bool,
    }

    /// `JsonSizeBound` as (ceiling, dynamic).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Bound {
        pub n: usize,
        pub dynamic: bool,
    }

    /// An archive byte range.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Span {
        pub start: usize,
        pub end: usize,
    }

    /// `binary::ErrorKind` with the port's `B` prefix (Vesper enum
    /// variants share one namespace with `ErrorKind`'s).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum BinErrorKind {
        BTruncated,
        BTrailingBytes,
        BOutputTooSmall,
        BByteLimit,
        BCountLimit,
        BDepthLimit,
        BStringLimit,
        BWorkLimit,
        BAllocationLimit,
        BAllocationFailed,
        BOverflow,
        BMagic,
        BVersion,
        BSchema,
        BAlignment,
        BOffset,
        BLayout,
        BUtf8,
        BTree,
    }

    /// `binary::Error`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct BinError {
        pub kind: BinErrorKind,
        pub offset: usize,
    }

    /// A record-level failure (vesper/src/records.ves `Fault`).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Fault {
        pub code: u64,
        pub detail: u64,
        pub offset: usize,
        pub record: usize,
        pub line: usize,
    }

    /// A decoded telemetry point; the trace id is `trace_len` bytes of the
    /// four little-endian words.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct TilePoint {
        pub ts_unix_ms: i64,
        pub latency_us: u32,
        pub status_code: u16,
        pub flags: u16,
        pub trace_len: usize,
        pub trace0: u64,
        pub trace1: u64,
        pub trace2: u64,
        pub trace3: u64,
    }

    /// A checked EventEnvelope: the id, and the topic and raw_meta tokens.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct EventRow {
        pub id: u64,
        pub topic: usize,
        pub raw_meta: usize,
    }

    /// `ndjson::BatchLimits` without its per-record `Limits`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct BatchLimits {
        pub max_input_bytes: usize,
        pub max_records: usize,
        pub max_tokens: usize,
        pub max_index_bytes: usize,
        pub max_metadata_bytes: usize,
        pub max_work: usize,
    }

    /// NDJSON batch admission.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct NdPlan {
        pub count: usize,
        pub eof_line: usize,
        pub metadata_bytes: usize,
        pub work: usize,
    }

    /// The NDJSON frame cursor (vesper/src/lib.ves `NdCursor`).
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct NdCursor {
        pub start: u64,
        pub ordinal: u64,
        pub tokens: u64,
        pub index_bytes: u64,
        pub work: u64,
        pub end: u64,
        pub payload_end: u64,
        pub remaining_tokens: u64,
        pub remaining_index: u64,
        pub remaining_work: u64,
        pub max_input_bytes: u64,
        pub max_tokens: u64,
        pub max_index_bytes: u64,
        pub max_work: u64,
    }

    impl From<BinError> for crate::binary::Error {
        fn from(e: BinError) -> Self {
            use crate::binary::ErrorKind as K;
            let kind = match e.kind {
                BinErrorKind::BTruncated => K::Truncated,
                BinErrorKind::BTrailingBytes => K::TrailingBytes,
                BinErrorKind::BOutputTooSmall => K::OutputTooSmall,
                BinErrorKind::BByteLimit => K::ByteLimit,
                BinErrorKind::BCountLimit => K::CountLimit,
                BinErrorKind::BDepthLimit => K::DepthLimit,
                BinErrorKind::BStringLimit => K::StringLimit,
                BinErrorKind::BWorkLimit => K::WorkLimit,
                BinErrorKind::BAllocationLimit => K::AllocationLimit,
                BinErrorKind::BAllocationFailed => K::AllocationFailed,
                BinErrorKind::BOverflow => K::Overflow,
                BinErrorKind::BMagic => K::Magic,
                BinErrorKind::BVersion => K::Version,
                BinErrorKind::BSchema => K::Schema,
                BinErrorKind::BAlignment => K::Alignment,
                BinErrorKind::BOffset => K::Offset,
                BinErrorKind::BLayout => K::Layout,
                BinErrorKind::BUtf8 => K::Utf8,
                BinErrorKind::BTree => K::Tree,
            };
            crate::binary::Error::new(kind, e.offset)
        }
    }
}

/// The 128-bit power-of-five table, built once by the port.
fn pow5() -> &'static [u64] {
    static TABLE: OnceLock<Vec<u64>> = OnceLock::new();
    TABLE.get_or_init(vp::number_pow5)
}

/// `number::decode_f64` through the port; the undecided case falls back to
/// `str::parse` exactly as the Rust build does.
pub(crate) fn decode_f64(text: &str) -> Option<(f64, crate::lemire::DecodePath)> {
    use crate::lemire::DecodePath;
    let d = vp::number_decode(text.as_bytes(), pow5());
    match d.path {
        1 => Some((f64::from_bits(d.bits), DecodePath::Clinger)),
        2 => Some((f64::from_bits(d.bits), DecodePath::EiselLemire)),
        3 => Some((text.parse::<f64>().ok()?, DecodePath::Fallback)),
        _ => None,
    }
}

use crate::index::Token;
use crate::{Error, ErrorKind, IndexedJson, Limits};

/// The token tape of a document: the port's `Doc` handle, read in place.
pub(crate) struct Tape {
    pub(crate) doc: vp::Doc,
}

impl Tape {
    pub(crate) fn new() -> Self {
        Self { doc: vp::doc_new() }
    }
    pub(crate) fn len(&self) -> usize {
        self.doc.as_ref().tokens().len()
    }
    /// Tokens reserved (the tape's `try_reserve_exact` capacity).
    pub(crate) fn capacity(&self) -> usize {
        self.doc.as_ref().capacity() as usize
    }
    pub(crate) fn get(&self, i: usize) -> Token {
        self.doc
            .as_ref()
            .tokens()
            .get(i)
            .expect("tape index in bounds")
    }
}

impl core::fmt::Debug for Tape {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list()
            .entries(self.doc.as_ref().tokens().iter())
            .finish()
    }
}

fn limits(l: &Limits) -> vp::repr::Limits {
    vp::repr::Limits::from_value(l)
}

/// The `&str` view of input the port validated (std's safe check again,
/// so the adapter needs no `from_utf8_unchecked`).
fn whole_str(input: &[u8]) -> Result<&str, Error> {
    core::str::from_utf8(input).map_err(|e| Error::new(ErrorKind::Utf8, e.valid_up_to()))
}

/// `IndexedJson::parse`: the scalar recursive-descent parser of the port.
pub(crate) fn parse(input: &[u8], l: Limits) -> Result<IndexedJson<'_>, Error> {
    let mut tape = Tape::new();
    vp::doc_parse(&mut tape.doc, input, &limits(&l))?;
    let text = whole_str(input)?;
    let r = tape.doc.as_ref();
    let (work_used, index_entries) = (r.work() as usize, r.entries() as usize);
    Ok(IndexedJson {
        input: text,
        tokens: tape,
        work_used,
        index_entries,
    })
}

/// `Node::field` over the document's tape.
pub(crate) fn field(
    doc: &IndexedJson<'_>,
    index: usize,
    key: &str,
    max_work: usize,
) -> Result<Option<usize>, Error> {
    vp::doc_field(
        &doc.tokens.doc,
        doc.input.as_bytes(),
        index as u64,
        key.as_bytes(),
        max_work as u64,
    )
    .map(|found| found.map(|i| i as usize))
}

/// `JsonString::equals`.
pub(crate) fn string_equals(raw: &str, text: &str) -> bool {
    vp::string_equals(raw.as_bytes(), text.as_bytes())
}

/// `JsonString::decode` into scratch that holds the decoded length.
pub(crate) fn string_decode(raw: &str, scratch: &mut [u8]) -> usize {
    vp::string_decode(raw.as_bytes(), scratch) as usize
}

use crate::simd::{IndexLimits, Isa, StructuralIndex};

/// Admission of src/stage2.rs (`admit`): the stage 1 limits.
pub(crate) fn admit(input: &[u8], l: &Limits) -> Result<IndexLimits, Error> {
    let entries = vp::doc_admit(input.len() as u64, &limits(l))?;
    Ok(IndexLimits {
        max_input_bytes: l.max_input_bytes,
        max_entries: entries as usize,
    })
}

/// Stage 2 of the port over a stage 1 index built by the Rust kernels.
fn build_into(
    tape: &mut Tape,
    input: &[u8],
    index: &StructuralIndex,
    l: &Limits,
) -> Result<(), Error> {
    vp::doc_build(
        &mut tape.doc,
        input,
        index.structurals(),
        index.escapes(),
        index.first_string_control().map_or(u64::MAX, u64::from),
        index.truncated_at().map_or(u64::MAX, u64::from),
        index.unclosed_string(),
        &limits(l),
    )
}

fn document<'a>(input: &'a [u8], text: &'a str, tape: Tape) -> IndexedJson<'a> {
    debug_assert_eq!(input.len(), text.len());
    let r = tape.doc.as_ref();
    let (work_used, index_entries) = (r.work() as usize, r.entries() as usize);
    IndexedJson {
        input: text,
        tokens: tape,
        work_used,
        index_entries,
    }
}

/// `IndexedJson::parse_with_index` through the port's stage 2.
pub(crate) fn parse_with_index<'a>(
    input: &'a [u8],
    l: Limits,
    isa: Isa,
    index: &mut StructuralIndex,
) -> Result<IndexedJson<'a>, Error> {
    let index_limits = admit(input, &l)?;
    index.build(input, isa, index_limits)?;
    let mut tape = Tape::new();
    build_into(&mut tape, input, index, &l)?;
    let text = whole_str(input)?;
    Ok(document(input, text, tape))
}

/// `SimdParser::parse` after stage 1: stage 2 into the parser's reusable
/// tape, which every outcome returns to `slot`.
pub(crate) fn parse_reusing<'a, R>(
    input: &'a [u8],
    l: Limits,
    index: &StructuralIndex,
    slot: &mut Option<Tape>,
    f: impl FnOnce(&IndexedJson<'a>) -> R,
) -> Result<R, Error> {
    let mut tape = slot.take().unwrap_or_else(Tape::new);
    let checked = build_into(&mut tape, input, index, &l).and_then(|()| whole_str(input));
    let text = match checked {
        Ok(text) => text,
        Err(e) => {
            *slot = Some(tape);
            return Err(e);
        }
    };
    let doc = document(input, text, tape);
    let result = f(&doc);
    *slot = Some(doc.tokens);
    Ok(result)
}

// Encoding (src/encode.rs): the borrowed value tree flattened into the word
// stream of vesper/src/encode.ves, with strings and raw text in one pool.
use crate::encode::{EncodeLimits, Value};

/// The flattened stream, built under the limits. The port's preflight
/// charges at least what `Flat` counts (one token and one work unit per
/// token, the bytes of each string and raw node, the depth of each
/// container), so once a count passes its limit the preflight fails at or
/// before that value: `Flat` stops there, and the stream never holds more
/// than the limits admit (shared `Value` slices cannot blow it up, and the
/// recursion stops at the depth limit).
struct Flat<'l> {
    limits: &'l EncodeLimits,
    words: Vec<u64>,
    pool: Vec<u8>,
    tokens: usize,
    strings: usize,
    work: usize,
    stopped: bool,
}

impl Flat<'_> {
    fn token(&mut self) {
        self.tokens = self.tokens.saturating_add(1);
        self.work = self.work.saturating_add(1);
    }
    /// Stop once a count is past its limit.
    fn check(&mut self) {
        if self.tokens > self.limits.max_tokens
            || self.work > self.limits.max_work
            || self.strings > self.limits.max_total_string_bytes
        {
            self.stopped = true;
        }
    }
    /// A string or key: its bytes are copied only within the per-string
    /// limit (the preflight fails on a longer one before reading it, and
    /// compares keys of equal length only).
    fn text(&mut self, t: &str) {
        self.words.push(self.pool.len() as u64);
        self.words.push(t.len() as u64);
        if t.len() > self.limits.max_string_bytes {
            self.stopped = true;
            return;
        }
        self.pool.extend_from_slice(t.as_bytes());
        self.strings = self.strings.saturating_add(t.len());
        self.work = self.work.saturating_add(t.len());
    }
    fn value(&mut self, value: &Value<'_>, depth: usize) {
        if self.stopped {
            return;
        }
        let deep = depth >= self.limits.max_depth || depth >= 128;
        match value {
            Value::Null => self.words.push(0),
            Value::Bool(b) => self.words.extend([1, u64::from(*b)]),
            Value::U64(n) => self.words.extend([2, *n]),
            Value::I64(n) => self.words.extend([3, *n as u64]),
            Value::F64(n) => self.words.extend([4, n.to_bits()]),
            Value::String(t) => {
                self.words.push(5);
                self.token();
                self.text(t);
            }
            Value::Array(values) => {
                self.token();
                if deep {
                    // The preflight fails here with DepthLimit.
                    self.words.extend([6, 0]);
                    self.stopped = true;
                    return;
                }
                self.words.extend([6, values.len() as u64]);
                for v in *values {
                    self.check();
                    self.value(v, depth + 1);
                }
            }
            Value::Object(fields) => {
                self.token();
                if deep {
                    self.words.extend([7, 0]);
                    self.stopped = true;
                    return;
                }
                self.words.extend([7, fields.len() as u64]);
                for (key, v) in *fields {
                    self.check();
                    if self.stopped {
                        return;
                    }
                    self.token();
                    self.text(key);
                    self.check();
                    self.value(v, depth + 1);
                }
            }
            Value::Raw(node) => {
                let raw = node.raw();
                self.words
                    .extend([8, self.pool.len() as u64, raw.len() as u64]);
                if self.work.saturating_add(raw.len()) > self.limits.max_work {
                    // The preflight charges the raw length first and fails
                    // with WorkLimit before it reads the text or its stats.
                    self.words.push(0);
                    self.stopped = true;
                    return;
                }
                self.pool.extend_from_slice(raw.as_bytes());
                self.work = self.work.saturating_add(raw.len());
                let at = self.words.len();
                self.words.push(0);
                let mut n = 0;
                for (kind, depth, bytes) in node.stats() {
                    self.words.extend([kind as u64, depth as u64, bytes as u64]);
                    self.token();
                    n += 1;
                }
                self.words[at] = n;
            }
        }
        if !matches!(
            value,
            Value::String(_) | Value::Array(_) | Value::Object(_) | Value::Raw(_)
        ) {
            self.token();
        }
        self.check();
    }
}

fn flattened(value: &Value<'_>, limits: &EncodeLimits) -> (Vec<u64>, Vec<u8>) {
    let mut flat = Flat {
        limits,
        words: Vec::new(),
        pool: Vec::new(),
        tokens: 0,
        strings: 0,
        work: 0,
        stopped: false,
    };
    flat.value(value, 0);
    (flat.words, flat.pool)
}

/// `encode::encoded_size`.
pub(crate) fn encoded_size(value: &Value<'_>, limits: EncodeLimits) -> Result<usize, Error> {
    let (words, pool) = flattened(value, &limits);
    vp::value_size(&words, &pool, &vp::repr::EncodeLimits::from_value(&limits)).map(|n| n as usize)
}

/// `encode::encode_into`.
pub(crate) fn encode_into(
    value: &Value<'_>,
    output: &mut [u8],
    limits: EncodeLimits,
) -> Result<usize, Error> {
    let (words, pool) = flattened(value, &limits);
    vp::value_write(
        &words,
        &pool,
        output,
        &vp::repr::EncodeLimits::from_value(&limits),
    )
    .map(|n| n as usize)
}

// Schema helpers (src/helpers.rs) and `SwarKey` (src/tiles.rs).
use crate::helpers::{JsonSizeBound, SwarFieldMatcher};
use types::Bound;

pub(crate) fn matcher_compile(field_idx: u16, key: &str) -> Option<SwarFieldMatcher> {
    vp::matcher_compile(field_idx, key.as_bytes())
}

pub(crate) fn matcher_window(m: SwarFieldMatcher, window: &[u8]) -> bool {
    vp::matcher_window(&vp::repr::Swar::from_value(&m), window)
}

pub(crate) fn matcher_key(m: SwarFieldMatcher, raw: &str) -> bool {
    vp::matcher_key(&vp::repr::Swar::from_value(&m), raw.as_bytes())
}

pub(crate) fn escaped_mask(backslashes: u64, carry_in: bool) -> (u64, bool) {
    let e = vp::escape_mask(backslashes, carry_in).to_value();
    (e.mask, e.carry)
}

fn bound_in(b: JsonSizeBound) -> vp::repr::Bound {
    let (n, dynamic) = b.parts();
    vp::repr::Bound::from_value(&Bound { n, dynamic })
}

fn bound_out(b: vp::repr::Bound) -> JsonSizeBound {
    let b = b.to_value();
    if b.dynamic {
        JsonSizeBound::Dynamic {
            static_ceiling: b.n,
        }
    } else {
        JsonSizeBound::Bounded(b.n)
    }
}

pub(crate) fn bound_string(max_utf8_bytes: usize) -> Result<JsonSizeBound, Error> {
    vp::size_bound_string(max_utf8_bytes as u64).map(bound_out)
}

pub(crate) fn bound_field(
    object: JsonSizeBound,
    key_escaped_len: usize,
    field: JsonSizeBound,
    first: bool,
) -> Result<JsonSizeBound, Error> {
    vp::size_bound_field(
        &bound_in(object),
        key_escaped_len as u64,
        &bound_in(field),
        first,
    )
    .map(bound_out)
}

pub(crate) fn key_raw(words: [u64; 2], len: u8, raw: &[u8]) -> bool {
    vp::key_raw(words[0], words[1], u64::from(len), raw)
}

pub(crate) fn key_matches(words: [u64; 2], len: u8, escaped: bool, raw: &[u8]) -> bool {
    vp::key_matches(words[0], words[1], u64::from(len), escaped, raw)
}

// StreamVByte (src/streamvbyte.rs).
use crate::binary::Result as BinResult;
use crate::streamvbyte::Limits as VbyteLimits;

fn vbyte_limits(l: &VbyteLimits) -> vp::repr::VbyteLimits {
    vp::repr::VbyteLimits::from_value(l)
}

pub(crate) fn svb_max_size(count: usize) -> BinResult<usize> {
    Ok(vp::svb_max_size(count as u64)? as usize)
}

pub(crate) fn svb_size(values: &[u32], limits: VbyteLimits) -> BinResult<usize> {
    Ok(vp::svb_size(values, &vbyte_limits(&limits))? as usize)
}

pub(crate) fn svb_encode(
    values: &[u32],
    output: &mut [u8],
    limits: VbyteLimits,
) -> BinResult<usize> {
    Ok(vp::svb_encode(values, output, &vbyte_limits(&limits))? as usize)
}

pub(crate) fn svb_decode(
    input: &[u8],
    count: usize,
    output: &mut [u32],
    limits: VbyteLimits,
) -> BinResult<usize> {
    Ok(vp::svb_decode(input, count as u64, output, &vbyte_limits(&limits))? as usize)
}

// The VSPA archive (src/archive.rs).
use crate::archive::{Archive, Limits as ArchiveLimits, RelPtr32, RelSlice32, TreeNode};
use core::ops::Range;

fn archive_limits(l: &ArchiveLimits) -> vp::repr::ArchiveLimits {
    vp::repr::ArchiveLimits::from_value(l)
}

fn range(s: vp::repr::Span) -> Range<usize> {
    let s = s.to_value();
    s.start..s.end
}

pub(crate) fn vspa_validation_bytes(count: usize) -> BinResult<usize> {
    Ok(vp::vspa_validation_bytes(count as u64)? as usize)
}

pub(crate) fn vspa_between(field_offset: usize, target_offset: usize) -> BinResult<RelPtr32> {
    let offset = vp::vspa_between(field_offset as u64, target_offset as u64)?;
    Ok(RelPtr32 { offset })
}

pub(crate) fn vspa_resolve(
    p: RelPtr32,
    field_offset: usize,
    size: usize,
    alignment: usize,
    archive_len: usize,
) -> BinResult<Range<usize>> {
    Ok(range(vp::vspa_resolve(
        p.offset,
        field_offset as u64,
        size as u64,
        alignment as u64,
        archive_len as u64,
    )?))
}

pub(crate) fn vspa_resolve_slice(
    s: RelSlice32,
    field_offset: usize,
    item_size: usize,
    alignment: usize,
    archive_len: usize,
) -> BinResult<Range<usize>> {
    Ok(range(vp::vspa_resolve_slice(
        s.offset,
        s.count,
        field_offset as u64,
        item_size as u64,
        alignment as u64,
        archive_len as u64,
    )?))
}

/// Nodes as five words each (value, label offset and length, children
/// offset and count) into one label pool and one child pool, built under the
/// limits. The port's preflight (`archive_size`) fails on the node count
/// first, so at most `max_nodes + 1` nodes are flattened (the count still
/// fails); then, node by node, on the children count, the edge total and
/// the label length before it reads that node's label or children. Once a
/// node passes one of those limits its bytes and every later node's are not
/// copied, only their five words, so the pools never hold more than the
/// limits admit.
fn nodes_flat(nodes: &[TreeNode<'_>], limits: &ArchiveLimits) -> (Vec<u64>, Vec<u8>, Vec<u32>) {
    let nodes = &nodes[..nodes.len().min(limits.max_nodes.saturating_add(1))];
    let mut words = Vec::with_capacity(nodes.len() * 5);
    let (mut labels, mut children) = (Vec::new(), Vec::new());
    let mut stopped = false;
    let mut edges = 0usize;
    for n in nodes {
        words.extend([
            u64::from(n.value),
            labels.len() as u64,
            n.label.len() as u64,
            children.len() as u64,
            n.children.len() as u64,
        ]);
        if stopped {
            continue;
        }
        edges = edges.saturating_add(n.children.len());
        if n.children.len() > limits.max_children_per_node
            || edges > limits.max_edges
            || n.label.len() > limits.max_label_bytes
            || labels.len().saturating_add(n.label.len()) > limits.max_total_label_bytes
        {
            stopped = true;
            continue;
        }
        labels.extend_from_slice(n.label.as_bytes());
        children.extend_from_slice(n.children);
    }
    (words, labels, children)
}

pub(crate) fn vspa_size(nodes: &[TreeNode<'_>], limits: ArchiveLimits) -> BinResult<usize> {
    let (words, labels, children) = nodes_flat(nodes, &limits);
    Ok(vp::vspa_size(&words, &labels, &children, &archive_limits(&limits))? as usize)
}

pub(crate) fn vspa_encode(
    nodes: &[TreeNode<'_>],
    output: &mut [u8],
    limits: ArchiveLimits,
) -> BinResult<usize> {
    let (words, labels, children) = nodes_flat(nodes, &limits);
    Ok(vp::vspa_encode(&words, &labels, &children, output, &archive_limits(&limits))? as usize)
}

pub(crate) fn vspa_validate(bytes: &[u8], limits: ArchiveLimits) -> BinResult<Archive<'_>> {
    let count = vp::vspa_validate(bytes, &archive_limits(&limits))? as usize;
    Ok(Archive { bytes, count })
}

// Record-level paths (vesper/src/records.ves).
pub(crate) use types::{EventRow, Fault, NdPlan, TilePoint};

/// `Node::as_u64`.
pub(crate) fn node_u64(doc: &IndexedJson<'_>, index: usize) -> Result<u64, Error> {
    vp::doc_u64(&doc.tokens.doc, doc.input.as_bytes(), index as u64)
}

/// `Node::as_i64`.
pub(crate) fn node_i64(doc: &IndexedJson<'_>, index: usize) -> Result<i64, Error> {
    vp::doc_i64(&doc.tokens.doc, doc.input.as_bytes(), index as u64)
}

/// The telemetry decode of src/tiles.rs.
pub(crate) fn tile_decode(doc: &IndexedJson<'_>, index: usize) -> Result<TilePoint, Fault> {
    vp::doc_tile(&doc.tokens.doc, doc.input.as_bytes(), index as u64).map(|p| p.to_value())
}

/// `checked_event` of src/event_columns.rs.
pub(crate) fn event_check(
    doc: &IndexedJson<'_>,
    index: usize,
    remaining: &mut usize,
    passes: usize,
) -> Result<EventRow, Fault> {
    let mut left = *remaining as u64;
    let row = vp::doc_event(
        &doc.tokens.doc,
        doc.input.as_bytes(),
        index as u64,
        &mut left,
        passes as u64,
    );
    *remaining = left as usize;
    row.map(|r| r.to_value())
}

/// The NDJSON frame cursor, read and updated in place.
pub(crate) type NdCursor = vp::repr::NdCursor;

fn batch(l: &crate::ndjson::BatchLimits) -> (vp::repr::BatchLimits, vp::repr::Limits) {
    let b = types::BatchLimits {
        max_input_bytes: l.max_input_bytes,
        max_records: l.max_records,
        max_tokens: l.max_tokens,
        max_index_bytes: l.max_index_bytes,
        max_metadata_bytes: l.max_metadata_bytes,
        max_work: l.max_work,
    };
    (vp::repr::BatchLimits::from_value(&b), limits(&l.record))
}

pub(crate) fn batch_validate(l: &crate::ndjson::BatchLimits) -> Result<(), Fault> {
    let (b, r) = batch(l);
    vp::batch_validate(&b, &r)
}

pub(crate) fn batch_admit(
    input: &[u8],
    l: &crate::ndjson::BatchLimits,
    record_size: usize,
) -> Result<NdPlan, Fault> {
    let (b, _) = batch(l);
    vp::batch_admit(input, &b, record_size as u64).map(|p| p.to_value())
}

pub(crate) fn batch_cursor(work: usize) -> NdCursor {
    NdCursor::from_value(&types::NdCursor {
        work: work as u64,
        ..Default::default()
    })
}

pub(crate) fn batch_frame(
    input: &[u8],
    cursor: &mut NdCursor,
    l: &crate::ndjson::BatchLimits,
) -> Result<(), Fault> {
    let (b, r) = batch(l);
    vp::batch_frame(input, cursor, &b, &r)
}

pub(crate) fn batch_account(
    cursor: &mut NdCursor,
    tokens: usize,
    index: usize,
    work: usize,
) -> Result<(), Fault> {
    vp::batch_account(cursor, tokens as u64, index as u64, work as u64)
}
