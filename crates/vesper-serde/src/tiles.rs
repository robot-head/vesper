//! §20.5 showcase: NDJSON telemetry decoded straight into 16-wide AoSoA tiles.
//!
//! Each line is indexed by stage 1 (SIMD when available), validated by the
//! stage 2 tape builder of a reusable [`SimdParser`], and only the five schema
//! fields are decoded, their keys matched a 64-bit word at a time ([`SwarKey`]);
//! unknown members are skipped over their subtree ends.
//! Hot fields live in 64-byte-aligned [`HotTile`]s, the cold trace id in a
//! parallel [`ColdTile`] array. The parser's buffers and the tile storage are
//! reused, so once capacity is warm, ingesting further batches of no larger
//! records makes no heap allocation (checked by a counting allocator test).
//!
//! This is a fixed host schema, not `@derive(JsonDecode)` or `tiled(16)`
//! source lowering, and Rust ownership here is not Vesper Region typing.
use crate::simd::Isa;
use crate::{ErrorKind as JsonErrorKind, JsonString, Kind, Limits, Node, SimdParser};

/// Points per tile.
pub const TILE_WIDTH: usize = 16;

/// A string of at most 32 UTF-8 bytes stored inline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InlineStr32 {
    len: u8,
    bytes: [u8; 32],
}
impl InlineStr32 {
    pub fn new(text: &str) -> Option<Self> {
        let mut bytes = [0; 32];
        bytes
            .get_mut(..text.len())?
            .copy_from_slice(text.as_bytes());
        Some(Self {
            len: text.len() as u8,
            bytes,
        })
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).expect("built from &str")
    }
}

/// One decoded NDJSON record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TelemetryPoint {
    pub ts_unix_ms: i64,
    pub latency_us: u32,
    pub status_code: u16,
    pub flags: u16,
    pub trace_id_hex: InlineStr32,
}

/// Hot columns of 16 points: 256 bytes, four cache lines.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C, align(64))]
pub struct HotTile {
    pub ts_unix_ms: [i64; TILE_WIDTH],
    pub latency_us: [u32; TILE_WIDTH],
    pub status_code: [u16; TILE_WIDTH],
    pub flags: [u16; TILE_WIDTH],
}

/// Cold columns of 16 points.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ColdTile {
    pub trace_id_hex: [InlineStr32; TILE_WIDTH],
}

/// Fixed-capacity tiled sequence. `push` never reallocates.
#[derive(Debug)]
pub struct TelemetryTiles {
    hot: Vec<HotTile>,
    cold: Vec<ColdTile>,
    len: usize,
    capacity: usize,
}

impl TelemetryTiles {
    /// Reserve room for `points` points, rounded up to whole tiles.
    pub fn with_capacity(points: usize) -> Result<Self, TileError> {
        let tiles = points.div_ceil(TILE_WIDTH);
        let mut hot = Vec::new();
        let mut cold = Vec::new();
        hot.try_reserve_exact(tiles)
            .and_then(|_| cold.try_reserve_exact(tiles))
            .map_err(|_| TileError::at(TileErrorKind::AllocationFailed, 0, 0))?;
        Ok(Self {
            hot,
            cold,
            len: 0,
            capacity: tiles * TILE_WIDTH,
        })
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// Tiles in use; the last may be partly filled (unused lanes are zero).
    pub fn hot_tiles(&self) -> &[HotTile] {
        &self.hot
    }
    pub fn cold_tiles(&self) -> &[ColdTile] {
        &self.cold
    }
    pub fn get(&self, index: usize) -> Option<TelemetryPoint> {
        if index >= self.len {
            return None;
        }
        let (t, lane) = (index / TILE_WIDTH, index % TILE_WIDTH);
        let hot = &self.hot[t];
        Some(TelemetryPoint {
            ts_unix_ms: hot.ts_unix_ms[lane],
            latency_us: hot.latency_us[lane],
            status_code: hot.status_code[lane],
            flags: hot.flags[lane],
            trace_id_hex: self.cold[t].trace_id_hex[lane],
        })
    }
    /// Append one point, or `false` when full.
    pub fn push(&mut self, point: TelemetryPoint) -> bool {
        if self.len == self.capacity {
            return false;
        }
        let (t, lane) = (self.len / TILE_WIDTH, self.len % TILE_WIDTH);
        if lane == 0 {
            self.hot.push(HotTile::default());
            self.cold.push(ColdTile::default());
        }
        let hot = &mut self.hot[t];
        hot.ts_unix_ms[lane] = point.ts_unix_ms;
        hot.latency_us[lane] = point.latency_us;
        hot.status_code[lane] = point.status_code;
        hot.flags[lane] = point.flags;
        self.cold[t].trace_id_hex[lane] = point.trace_id_hex;
        self.len += 1;
        true
    }
    /// Shrink to `len` points, zeroing the vacated lanes (an O(1)-per-point
    /// rewind used to make failed ingestion transactional).
    pub fn truncate(&mut self, len: usize) {
        while self.len > len {
            self.len -= 1;
            let (t, lane) = (self.len / TILE_WIDTH, self.len % TILE_WIDTH);
            if lane == 0 {
                self.hot.pop();
                self.cold.pop();
            } else {
                let hot = &mut self.hot[t];
                hot.ts_unix_ms[lane] = 0;
                hot.latency_us[lane] = 0;
                hot.status_code[lane] = 0;
                hot.flags[lane] = 0;
                self.cold[t].trace_id_hex[lane] = InlineStr32::default();
            }
        }
    }
    pub fn clear(&mut self) {
        self.hot.clear();
        self.cold.clear();
        self.len = 0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryField {
    TsUnixMs,
    LatencyUs,
    StatusCode,
    Flags,
    TraceIdHex,
}
const FIELDS: [(&str, TelemetryField); 5] = [
    ("ts_unix_ms", TelemetryField::TsUnixMs),
    ("latency_us", TelemetryField::LatencyUs),
    ("status_code", TelemetryField::StatusCode),
    ("flags", TelemetryField::Flags),
    ("trace_id_hex", TelemetryField::TraceIdHex),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileErrorKind {
    Json(JsonErrorKind),
    BlankLine,
    NotObject,
    MissingField(TelemetryField),
    /// Wrong JSON kind for the field.
    FieldType(TelemetryField),
    /// A number outside the field's integer range, or a trace id over 32 bytes.
    FieldRange(TelemetryField),
    Full,
    AllocationFailed,
}

/// `offset` is the absolute byte offset of the failing line (or the JSON
/// error inside it); `line` is 1-based.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TileError {
    pub kind: TileErrorKind,
    pub offset: usize,
    pub line: usize,
}
impl TileError {
    fn at(kind: TileErrorKind, offset: usize, line: usize) -> Self {
        Self { kind, offset, line }
    }
}
impl core::fmt::Display for TileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:?} at byte {} (line {})",
            self.kind, self.offset, self.line
        )
    }
}
impl std::error::Error for TileError {}

/// Reusable NDJSON-to-tiles decoder.
#[derive(Debug)]
pub struct TelemetryIngestor {
    parser: SimdParser,
    limits: Limits,
}

impl TelemetryIngestor {
    /// Fastest stage 1 path; `limits` apply to each line.
    pub fn new(limits: Limits) -> Self {
        Self {
            parser: SimdParser::new(),
            limits,
        }
    }
    pub fn with_isa(isa: Isa, limits: Limits) -> Result<Self, crate::Error> {
        Ok(Self {
            parser: SimdParser::with_isa(isa)?,
            limits,
        })
    }
    pub fn isa(&self) -> Isa {
        self.parser.isa()
    }

    /// Decode every line of `ndjson` (LF or CRLF; final newline optional;
    /// blank lines rejected) and append the points. Transactional: on any
    /// error `tiles` is restored to its length on entry.
    pub fn ingest(
        &mut self,
        ndjson: &[u8],
        tiles: &mut TelemetryTiles,
    ) -> Result<usize, TileError> {
        let start = tiles.len();
        let result = self.ingest_all(ndjson, tiles);
        if result.is_err() {
            tiles.truncate(start);
        }
        result.map(|()| tiles.len() - start)
    }

    fn ingest_all(&mut self, ndjson: &[u8], tiles: &mut TelemetryTiles) -> Result<(), TileError> {
        if ndjson.is_empty() {
            return Ok(());
        }
        let mut offset = 0usize;
        for (ordinal, frame) in ndjson.split_inclusive(|b| *b == b'\n').enumerate() {
            let line = ordinal + 1;
            let payload = match frame.strip_suffix(b"\n") {
                Some(body) => body.strip_suffix(b"\r").unwrap_or(body),
                None => frame,
            };
            if payload.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r')) {
                return Err(TileError::at(TileErrorKind::BlankLine, offset, line));
            }
            if tiles.len() == tiles.capacity() {
                return Err(TileError::at(TileErrorKind::Full, offset, line));
            }
            let point = self
                .parser
                .parse(payload, self.limits, |doc| decode(doc.root()))
                .map_err(|e| TileError::at(TileErrorKind::Json(e.kind), offset + e.offset, line))?
                .map_err(|kind| TileError::at(kind, offset, line))?;
            let pushed = tiles.push(point);
            debug_assert!(pushed, "capacity checked above");
            offset += frame.len();
        }
        Ok(())
    }
}

/// A JSON object key of at most 16 bytes compiled to two little-endian
/// 64-bit words, compared a word at a time (SWAR) instead of byte by byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwarKey {
    words: [u64; 2],
    len: u8,
}

impl SwarKey {
    /// Keys of 1 to 16 printable ASCII bytes other than `"` and `\\`, the
    /// only ones whose unescaped spelling is unique.
    pub const fn compile(key: &str) -> Option<Self> {
        let bytes = key.as_bytes();
        if bytes.is_empty() || bytes.len() > 16 {
            return None;
        }
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b < 0x20 || b > 0x7e || b == b'"' || b == b'\\' {
                return None;
            }
            i += 1;
        }
        Some(Self {
            words: pack(bytes),
            len: bytes.len() as u8,
        })
    }
    /// Whether the raw (unescaped) key content `raw` is this key: a length
    /// check and two word compares.
    #[inline]
    pub fn matches_raw(self, raw: &[u8]) -> bool {
        raw.len() == self.len as usize && pack(raw) == self.words
    }
    /// Whether a validated key is this key, decoding escaped spellings such
    /// as `"\\u0069d"` without allocating.
    pub fn matches(self, key: JsonString<'_>) -> bool {
        if key.is_escaped() {
            let bytes = unpack(self.words);
            let name =
                core::str::from_utf8(&bytes[..self.len as usize]).expect("compiled ASCII key");
            key.equals(name)
        } else {
            self.matches_raw(key.raw_content().as_bytes())
        }
    }
}

/// Up to 16 bytes as two zero-padded little-endian words; longer input
/// packs only its first 16 bytes (callers compare lengths first).
#[inline]
const fn pack(bytes: &[u8]) -> [u64; 2] {
    let mut buf = [0u8; 16];
    let n = if bytes.len() < 16 { bytes.len() } else { 16 };
    let mut i = 0;
    while i < n {
        buf[i] = bytes[i];
        i += 1;
    }
    let (lo, hi) = buf.split_at(8);
    [
        u64::from_le_bytes([lo[0], lo[1], lo[2], lo[3], lo[4], lo[5], lo[6], lo[7]]),
        u64::from_le_bytes([hi[0], hi[1], hi[2], hi[3], hi[4], hi[5], hi[6], hi[7]]),
    ]
}

fn unpack(words: [u64; 2]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&words[0].to_le_bytes());
    out[8..].copy_from_slice(&words[1].to_le_bytes());
    out
}

const fn key(name: &str) -> SwarKey {
    match SwarKey::compile(name) {
        Some(k) => k,
        None => panic!("telemetry keys are short printable ASCII"),
    }
}

/// The five §20.5 keys, compiled at build time.
const KEYS: [SwarKey; 5] = [
    key(FIELDS[0].0),
    key(FIELDS[1].0),
    key(FIELDS[2].0),
    key(FIELDS[3].0),
    key(FIELDS[4].0),
];

fn field_slot(name: JsonString<'_>) -> Option<usize> {
    if name.is_escaped() {
        return KEYS.iter().position(|k| k.matches(name));
    }
    let raw = name.raw_content().as_bytes();
    if raw.len() > 16 {
        return None;
    }
    let words = pack(raw);
    KEYS.iter()
        .position(|k| k.len as usize == raw.len() && k.words == words)
}

fn decode(root: Node<'_, '_>) -> Result<TelemetryPoint, TileErrorKind> {
    let fields = root.fields().map_err(|_| TileErrorKind::NotObject)?;
    let mut found: [Option<Node<'_, '_>>; 5] = [None; 5];
    for (name, value) in fields {
        // Parsing already rejected duplicate keys, so at most one match each.
        if let Some(slot) = field_slot(name) {
            found[slot] = Some(value);
        }
    }
    let get = |i: usize| found[i].ok_or(TileErrorKind::MissingField(FIELDS[i].1));
    let (ts, latency, status, flags, trace) = (get(0)?, get(1)?, get(2)?, get(3)?, get(4)?);
    let int = |node: Node<'_, '_>, field| -> Result<i128, TileErrorKind> {
        if node.kind() != Kind::Number {
            return Err(TileErrorKind::FieldType(field));
        }
        node.as_i64()
            .map(i128::from)
            .or_else(|_| node.as_u64().map(i128::from))
            .map_err(|_| TileErrorKind::FieldRange(field))
    };
    let narrow =
        |value: i128, field| u32::try_from(value).map_err(|_| TileErrorKind::FieldRange(field));
    let ts_unix_ms = i64::try_from(int(ts, TelemetryField::TsUnixMs)?)
        .map_err(|_| TileErrorKind::FieldRange(TelemetryField::TsUnixMs))?;
    let latency_us = narrow(
        int(latency, TelemetryField::LatencyUs)?,
        TelemetryField::LatencyUs,
    )?;
    let status_code = u16::try_from(int(status, TelemetryField::StatusCode)?)
        .map_err(|_| TileErrorKind::FieldRange(TelemetryField::StatusCode))?;
    let flags = u16::try_from(int(flags, TelemetryField::Flags)?)
        .map_err(|_| TileErrorKind::FieldRange(TelemetryField::Flags))?;
    let trace = trace
        .string()
        .map_err(|_| TileErrorKind::FieldType(TelemetryField::TraceIdHex))?;
    let mut scratch = [0u8; 32];
    let trace_id_hex = trace
        .decode(&mut scratch)
        .ok()
        .and_then(|s| InlineStr32::new(s.as_str()))
        .ok_or(TileErrorKind::FieldRange(TelemetryField::TraceIdHex))?;
    Ok(TelemetryPoint {
        ts_unix_ms,
        latency_us,
        status_code,
        flags,
        trace_id_hex,
    })
}
