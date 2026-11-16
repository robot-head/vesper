//! A small, synchronous Kafka wire-protocol client: request framing, the
//! classic and flexible (KIP-482) encodings, TLS (rustls), SASL (PLAIN,
//! SCRAM-SHA-256/512 through `rsasl`), v2 record batches with CRC-32C and
//! gzip/snappy/lz4/zstd compression, read-committed fetches that drop
//! aborted transactions, idempotent and transactional produce, and the
//! transaction and group coordinator APIs.
//!
//! Every response is parsed with bounds checks; malformed or truncated bytes
//! are typed errors, never panics. Only the API versions listed in
//! [`VERSIONS`] are spoken; a broker that does not offer one of them fails
//! the call with `Error::Kafka("unsupported api version")`.
use crate::Error;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ----------------------------------------------------------------------
// Public configuration
// ----------------------------------------------------------------------

/// SASL mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaslMechanism {
    Plain,
    ScramSha256,
    ScramSha512,
}
impl SaslMechanism {
    fn name(self) -> &'static str {
        match self {
            Self::Plain => "PLAIN",
            Self::ScramSha256 => "SCRAM-SHA-256",
            Self::ScramSha512 => "SCRAM-SHA-512",
        }
    }
}

/// SASL credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct KafkaSasl {
    pub mechanism: SaslMechanism,
    pub username: String,
    pub password: String,
}
impl std::fmt::Debug for KafkaSasl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaSasl")
            .field("mechanism", &self.mechanism)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// TLS settings: the broker certificate must chain to one of `ca_pem`'s
/// certificates and name `server_name` (default: the broker's host).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KafkaTls {
    /// PEM-encoded trust anchors.
    pub ca_pem: Vec<u8>,
    pub server_name: Option<String>,
}

/// Record batch compression for produced batches. Fetched batches in any of
/// these codecs are always decoded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KafkaCompression {
    #[default]
    None,
    Gzip,
    Snappy,
    Lz4,
    Zstd,
}
impl KafkaCompression {
    fn code(self) -> i16 {
        match self {
            Self::None => 0,
            Self::Gzip => 1,
            Self::Snappy => 2,
            Self::Lz4 => 3,
            Self::Zstd => 4,
        }
    }
}

/// Transport security of every broker connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KafkaSecurity {
    pub tls: Option<KafkaTls>,
    pub sasl: Option<KafkaSasl>,
}

/// A record as read or written. Headers are kept sorted by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KafkaRecord {
    pub key: Option<Vec<u8>>,
    pub value: Option<Vec<u8>>,
    pub headers: BTreeMap<String, Vec<u8>>,
    /// Milliseconds since the Unix epoch.
    pub timestamp: i64,
}

// ----------------------------------------------------------------------
// Errors
// ----------------------------------------------------------------------

/// Internal failure: a broker error code, a transport failure, or a
/// malformed response, with a message for `last_error`.
#[derive(Clone, Debug)]
pub(crate) enum Fail {
    Code(i16, String),
    Io(String),
    Proto(&'static str),
    Local(Error),
}
impl Fail {
    pub(crate) fn code(&self) -> Option<i16> {
        match self {
            Self::Code(c, _) => Some(*c),
            _ => None,
        }
    }
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Code(c, what) => format!("{what}: {} ({c})", code_name(*c)),
            Self::Io(m) => format!("transport: {m}"),
            Self::Proto(m) => format!("protocol: {m}"),
            Self::Local(e) => format!("{e}"),
        }
    }
    /// Worth retrying after a metadata or coordinator refresh / reconnect.
    pub(crate) fn retriable(&self) -> bool {
        match self {
            Self::Io(_) => true,
            Self::Code(c, _) => matches!(
                *c,
                3 | 5 | 6 | 7 | 13 | 14 | 15 | 16 | 19 | 20 | 51 | 74 | 75 | 88 | 89
            ),
            _ => false,
        }
    }
    /// The coordinator moved or is not ready: look it up again.
    pub(crate) fn coordinator_moved(&self) -> bool {
        matches!(self.code(), Some(14..=16))
    }
    pub(crate) fn fenced(&self) -> bool {
        matches!(self.code(), Some(47 | 90 | 82))
    }
    pub(crate) fn error(&self) -> Error {
        match self {
            Self::Local(e) => e.clone(),
            Self::Io(_) => Error::Kafka("unavailable"),
            Self::Proto(_) => Error::Kafka("malformed response"),
            Self::Code(c, _) => Error::Kafka(match *c {
                1 => "offset out of range",
                2 => "corrupt message",
                3 => "unknown topic or partition",
                10 | 18 => "batch too large",
                22 => "illegal generation",
                25 => "unknown member",
                27 => "rebalance in progress",
                29 => "topic authorization failed",
                30 => "group authorization failed",
                31 => "cluster authorization failed",
                33 => "unsupported sasl mechanism",
                34 => "illegal sasl state",
                35 => "unsupported api version",
                36 => "topic exists",
                45 => "out of order sequence",
                47 | 90 => "producer fenced",
                48 => "invalid transaction state",
                49 => "invalid producer id mapping",
                53 => "transactional id authorization failed",
                58 => "sasl authentication failed",
                59 => "unknown producer id",
                82 => "fenced instance id",
                c if Self::Code(c, String::new()).retriable() => "unavailable",
                _ => "broker error",
            }),
        }
    }
}
impl From<Error> for Fail {
    fn from(e: Error) -> Self {
        Self::Local(e)
    }
}
pub(crate) type KResult<T> = Result<T, Fail>;

fn code_name(c: i16) -> &'static str {
    match c {
        -1 => "UNKNOWN_SERVER_ERROR",
        1 => "OFFSET_OUT_OF_RANGE",
        2 => "CORRUPT_MESSAGE",
        3 => "UNKNOWN_TOPIC_OR_PARTITION",
        5 => "LEADER_NOT_AVAILABLE",
        6 => "NOT_LEADER_OR_FOLLOWER",
        7 => "REQUEST_TIMED_OUT",
        10 => "MESSAGE_TOO_LARGE",
        14 => "COORDINATOR_LOAD_IN_PROGRESS",
        15 => "COORDINATOR_NOT_AVAILABLE",
        16 => "NOT_COORDINATOR",
        18 => "RECORD_LIST_TOO_LARGE",
        22 => "ILLEGAL_GENERATION",
        25 => "UNKNOWN_MEMBER_ID",
        27 => "REBALANCE_IN_PROGRESS",
        33 => "UNSUPPORTED_SASL_MECHANISM",
        34 => "ILLEGAL_SASL_STATE",
        35 => "UNSUPPORTED_VERSION",
        36 => "TOPIC_ALREADY_EXISTS",
        45 => "OUT_OF_ORDER_SEQUENCE_NUMBER",
        46 => "DUPLICATE_SEQUENCE_NUMBER",
        47 => "INVALID_PRODUCER_EPOCH",
        48 => "INVALID_TXN_STATE",
        49 => "INVALID_PRODUCER_ID_MAPPING",
        51 => "CONCURRENT_TRANSACTIONS",
        58 => "SASL_AUTHENTICATION_FAILED",
        59 => "UNKNOWN_PRODUCER_ID",
        79 => "MEMBER_ID_REQUIRED",
        82 => "FENCED_INSTANCE_ID",
        88 => "UNSTABLE_OFFSET_COMMIT",
        90 => "PRODUCER_FENCED",
        _ => "ERROR",
    }
}
fn check(code: i16, what: &str) -> KResult<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Fail::Code(code, what.to_string()))
    }
}

// ----------------------------------------------------------------------
// Encoding
// ----------------------------------------------------------------------

/// A request body writer. `flex` selects the compact (KIP-482) encodings.
pub(crate) struct W {
    pub(crate) buf: Vec<u8>,
    flex: bool,
}
impl W {
    pub(crate) fn new(flex: bool) -> Self {
        Self {
            buf: Vec::new(),
            flex,
        }
    }
    pub(crate) fn i8(&mut self, v: i8) -> &mut Self {
        self.buf.push(v as u8);
        self
    }
    pub(crate) fn bool(&mut self, v: bool) -> &mut Self {
        self.buf.push(u8::from(v));
        self
    }
    pub(crate) fn i16(&mut self, v: i16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub(crate) fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub(crate) fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub(crate) fn uvarint(&mut self, mut v: u64) -> &mut Self {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(b);
                return self;
            }
            self.buf.push(b | 0x80);
        }
    }
    pub(crate) fn varint(&mut self, v: i64) -> &mut Self {
        self.uvarint(((v << 1) ^ (v >> 63)) as u64)
    }
    fn len(&mut self, n: usize) -> KResult<&mut Self> {
        if self.flex {
            Ok(self.uvarint(n as u64 + 1))
        } else {
            Ok(self.i32(i32::try_from(n).map_err(|_| Fail::Local(Error::Overflow))?))
        }
    }
    pub(crate) fn str(&mut self, s: &str) -> KResult<&mut Self> {
        if self.flex {
            self.uvarint(s.len() as u64 + 1);
        } else {
            self.i16(i16::try_from(s.len()).map_err(|_| Fail::Local(Error::Overflow))?);
        }
        self.buf.extend_from_slice(s.as_bytes());
        Ok(self)
    }
    pub(crate) fn nstr(&mut self, s: Option<&str>) -> KResult<&mut Self> {
        match s {
            Some(s) => self.str(s),
            None if self.flex => Ok(self.uvarint(0)),
            None => Ok(self.i16(-1)),
        }
    }
    pub(crate) fn bytes(&mut self, b: &[u8]) -> KResult<&mut Self> {
        self.len(b.len())?;
        self.buf.extend_from_slice(b);
        Ok(self)
    }
    pub(crate) fn array(&mut self, n: usize) -> KResult<&mut Self> {
        self.len(n)
    }
    /// Empty tagged-field section (flexible versions only).
    pub(crate) fn tags(&mut self) -> &mut Self {
        if self.flex {
            self.uvarint(0);
        }
        self
    }
}

/// A bounds-checked response reader.
pub(crate) struct R<'a> {
    b: &'a [u8],
    at: usize,
    flex: bool,
}
const MALFORMED: Fail = Fail::Proto("malformed response");
impl<'a> R<'a> {
    pub(crate) fn new(b: &'a [u8], flex: bool) -> Self {
        Self { b, at: 0, flex }
    }
    pub(crate) fn rest(&self) -> usize {
        self.b.len() - self.at
    }
    pub(crate) fn take(&mut self, n: usize) -> KResult<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or(MALFORMED)?;
        let s = self.b.get(self.at..end).ok_or(MALFORMED)?;
        self.at = end;
        Ok(s)
    }
    pub(crate) fn i8(&mut self) -> KResult<i8> {
        Ok(self.take(1)?[0] as i8)
    }
    pub(crate) fn bool(&mut self) -> KResult<bool> {
        Ok(self.take(1)?[0] != 0)
    }
    pub(crate) fn i16(&mut self) -> KResult<i16> {
        Ok(i16::from_be_bytes(
            self.take(2)?.try_into().map_err(|_| MALFORMED)?,
        ))
    }
    pub(crate) fn i32(&mut self) -> KResult<i32> {
        Ok(i32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| MALFORMED)?,
        ))
    }
    pub(crate) fn u32(&mut self) -> KResult<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| MALFORMED)?,
        ))
    }
    pub(crate) fn i64(&mut self) -> KResult<i64> {
        Ok(i64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| MALFORMED)?,
        ))
    }
    pub(crate) fn uvarint(&mut self) -> KResult<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.take(1)?[0];
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(MALFORMED)
    }
    pub(crate) fn varint(&mut self) -> KResult<i64> {
        let v = self.uvarint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    /// A length prefix: `None` for null. Bounded by the bytes left (each
    /// element takes at least one byte), so a hostile count cannot allocate.
    fn len(&mut self) -> KResult<Option<usize>> {
        let n = if self.flex {
            match self.uvarint()? {
                0 => return Ok(None),
                n => n - 1,
            }
        } else {
            match self.i32()? {
                -1 => return Ok(None),
                n if n < 0 => return Err(MALFORMED),
                n => n as u64,
            }
        };
        let n = usize::try_from(n).map_err(|_| MALFORMED)?;
        if n > self.rest() {
            return Err(MALFORMED);
        }
        Ok(Some(n))
    }
    pub(crate) fn array(&mut self) -> KResult<usize> {
        Ok(self.len()?.unwrap_or(0))
    }
    pub(crate) fn nstr(&mut self) -> KResult<Option<String>> {
        let n = if self.flex {
            match self.uvarint()? {
                0 => return Ok(None),
                n => usize::try_from(n - 1).map_err(|_| MALFORMED)?,
            }
        } else {
            match self.i16()? {
                -1 => return Ok(None),
                n if n < 0 => return Err(MALFORMED),
                n => n as usize,
            }
        };
        let s = self.take(n)?;
        Ok(Some(
            std::str::from_utf8(s).map_err(|_| MALFORMED)?.to_string(),
        ))
    }
    pub(crate) fn str(&mut self) -> KResult<String> {
        self.nstr()?.ok_or(MALFORMED)
    }
    pub(crate) fn nbytes(&mut self) -> KResult<Option<&'a [u8]>> {
        match self.len()? {
            None => Ok(None),
            Some(n) => self.take(n).map(Some),
        }
    }
    pub(crate) fn bytes(&mut self) -> KResult<&'a [u8]> {
        Ok(self.nbytes()?.unwrap_or(&[]))
    }
    pub(crate) fn tags(&mut self) -> KResult<()> {
        if !self.flex {
            return Ok(());
        }
        let n = self.uvarint()?;
        for _ in 0..n {
            self.uvarint()?;
            let size = usize::try_from(self.uvarint()?).map_err(|_| MALFORMED)?;
            self.take(size)?;
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------
// API keys and versions
// ----------------------------------------------------------------------

pub(crate) mod api {
    pub const PRODUCE: i16 = 0;
    pub const FETCH: i16 = 1;
    pub const LIST_OFFSETS: i16 = 2;
    pub const METADATA: i16 = 3;
    pub const OFFSET_COMMIT: i16 = 8;
    pub const OFFSET_FETCH: i16 = 9;
    pub const FIND_COORDINATOR: i16 = 10;
    pub const JOIN_GROUP: i16 = 11;
    pub const HEARTBEAT: i16 = 12;
    pub const LEAVE_GROUP: i16 = 13;
    pub const SYNC_GROUP: i16 = 14;
    pub const SASL_HANDSHAKE: i16 = 17;
    pub const API_VERSIONS: i16 = 18;
    pub const CREATE_TOPICS: i16 = 19;
    pub const DELETE_RECORDS: i16 = 21;
    pub const INIT_PRODUCER_ID: i16 = 22;
    pub const ADD_PARTITIONS_TO_TXN: i16 = 24;
    pub const ADD_OFFSETS_TO_TXN: i16 = 25;
    pub const END_TXN: i16 = 26;
    pub const TXN_OFFSET_COMMIT: i16 = 28;
    pub const SASL_AUTHENTICATE: i16 = 36;
}

/// The (api key, version, flexible) this client speaks. Transactions use
/// the KIP-98 / KIP-447 protocol (explicit AddPartitionsToTxn, EndTxn below
/// v5), which brokers keep serving next to the KIP-890 one.
pub(crate) const VERSIONS: &[(i16, i16, bool)] = &[
    (api::PRODUCE, 8, false),
    (api::FETCH, 11, false),
    (api::LIST_OFFSETS, 5, false),
    (api::METADATA, 8, false),
    (api::OFFSET_COMMIT, 7, false),
    (api::OFFSET_FETCH, 7, true),
    (api::FIND_COORDINATOR, 2, false),
    (api::JOIN_GROUP, 5, false),
    (api::HEARTBEAT, 3, false),
    (api::LEAVE_GROUP, 3, false),
    (api::SYNC_GROUP, 3, false),
    (api::SASL_HANDSHAKE, 1, false),
    (api::API_VERSIONS, 0, false),
    (api::CREATE_TOPICS, 4, false),
    (api::DELETE_RECORDS, 1, false),
    (api::INIT_PRODUCER_ID, 1, false),
    (api::ADD_PARTITIONS_TO_TXN, 2, false),
    (api::ADD_OFFSETS_TO_TXN, 2, false),
    (api::END_TXN, 2, false),
    (api::TXN_OFFSET_COMMIT, 3, true),
    (api::SASL_AUTHENTICATE, 1, false),
];
fn version_of(key: i16) -> KResult<(i16, bool)> {
    VERSIONS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, v, f)| (*v, *f))
        .ok_or(Fail::Proto("unknown api"))
}

// ----------------------------------------------------------------------
// Connections
// ----------------------------------------------------------------------

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}
impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(s) => s.get_ref(),
        }
    }
}
impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

fn io_fail(e: std::io::Error) -> Fail {
    Fail::Io(e.to_string())
}

/// Settings every connection shares.
#[derive(Clone)]
pub(crate) struct ConnSettings {
    pub(crate) client_id: String,
    pub(crate) timeout: Duration,
    pub(crate) max_response: usize,
    pub(crate) tls: Option<(Arc<rustls::ClientConfig>, Option<String>)>,
    pub(crate) sasl: Option<KafkaSasl>,
}
impl ConnSettings {
    pub(crate) fn new(
        client_id: &str,
        timeout: Duration,
        max_response: usize,
        security: &KafkaSecurity,
    ) -> Result<Self, Error> {
        let tls = match &security.tls {
            None => None,
            Some(t) => Some((tls_config(&t.ca_pem)?, t.server_name.clone())),
        };
        Ok(Self {
            client_id: client_id.to_string(),
            timeout,
            max_response,
            tls,
            sasl: security.sasl.clone(),
        })
    }
}

fn tls_config(ca_pem: &[u8]) -> Result<Arc<rustls::ClientConfig>, Error> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    let mut roots = rustls::RootCertStore::empty();
    let mut any = false;
    for cert in CertificateDer::pem_slice_iter(ca_pem) {
        let cert = cert.map_err(|_| Error::Kafka("tls trust anchors"))?;
        roots
            .add(cert)
            .map_err(|_| Error::Kafka("tls trust anchors"))?;
        any = true;
    }
    if !any {
        return Err(Error::Kafka("tls trust anchors"));
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::Kafka("tls configuration"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// One authenticated broker connection.
pub(crate) struct Conn {
    stream: Stream,
    corr: i32,
    versions: BTreeMap<i16, (i16, i16)>,
    settings: ConnSettings,
}
impl Conn {
    pub(crate) fn open(addr: &str, settings: &ConnSettings) -> KResult<Self> {
        let deadline = Instant::now() + settings.timeout;
        let addrs: Vec<_> = addr
            .to_socket_addrs()
            .map_err(|e| Fail::Io(format!("resolve {addr}: {e}")))?
            .collect();
        let mut last = Fail::Io(format!("no address for {addr}"));
        let mut tcp = None;
        for a in addrs {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match TcpStream::connect_timeout(&a, left) {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last = Fail::Io(format!("connect {addr}: {e}")),
            }
        }
        let tcp = tcp.ok_or(last)?;
        tcp.set_nodelay(true).map_err(io_fail)?;
        tcp.set_read_timeout(Some(settings.timeout))
            .map_err(io_fail)?;
        tcp.set_write_timeout(Some(settings.timeout))
            .map_err(io_fail)?;
        let stream = match &settings.tls {
            None => Stream::Plain(tcp),
            Some((config, name)) => {
                let host = name.clone().unwrap_or_else(|| host_of(addr).to_string());
                let server = rustls::pki_types::ServerName::try_from(host)
                    .map_err(|_| Fail::Local(Error::Kafka("tls server name")))?;
                let conn = rustls::ClientConnection::new(Arc::clone(config), server)
                    .map_err(|e| Fail::Io(format!("tls: {e}")))?;
                let mut s = rustls::StreamOwned::new(conn, tcp);
                // Drive the handshake now so a bad certificate fails here.
                while s.conn.is_handshaking() {
                    s.conn
                        .complete_io(&mut s.sock)
                        .map_err(|e| Fail::Io(format!("tls handshake: {e}")))?;
                }
                Stream::Tls(Box::new(s))
            }
        };
        let mut conn = Self {
            stream,
            corr: 0,
            versions: BTreeMap::new(),
            settings: settings.clone(),
        };
        conn.api_versions()?;
        if let Some(sasl) = settings.sasl.clone() {
            conn.authenticate(&sasl)?;
        }
        Ok(conn)
    }

    fn api_versions(&mut self) -> KResult<()> {
        let body = self.call(api::API_VERSIONS, |_| Ok(()))?;
        let mut r = R::new(&body, false);
        let code = r.i16()?;
        check(code, "api versions")?;
        let n = r.array()?;
        for _ in 0..n {
            let key = r.i16()?;
            let min = r.i16()?;
            let max = r.i16()?;
            self.versions.insert(key, (min, max));
        }
        Ok(())
    }

    fn authenticate(&mut self, sasl: &KafkaSasl) -> KResult<()> {
        use rsasl::prelude::{Mechname, SASLClient, SASLConfig};
        let body = self.call(api::SASL_HANDSHAKE, |w| {
            w.str(sasl.mechanism.name())?;
            Ok(())
        })?;
        let mut r = R::new(&body, false);
        let code = r.i16()?;
        check(code, "sasl handshake")?;
        let config =
            SASLConfig::with_credentials(None, sasl.username.clone(), sasl.password.clone())
                .map_err(|_| Fail::Local(Error::Kafka("sasl configuration")))?;
        let client = SASLClient::new(config);
        let mech = Mechname::parse(sasl.mechanism.name().as_bytes())
            .map_err(|_| Fail::Local(Error::Kafka("sasl configuration")))?;
        let mut session = client
            .start_suggested(&[mech])
            .map_err(|_| Fail::Local(Error::Kafka("unsupported sasl mechanism")))?;
        let mut received: Option<Vec<u8>> = None;
        for _ in 0..8 {
            let mut out = std::io::Cursor::new(Vec::new());
            let state = session
                .step(received.as_deref(), &mut out)
                .map_err(|e| Fail::Io(format!("sasl: {e}")))?;
            if state.has_sent_message() {
                let message = out.into_inner();
                let body = self.call(api::SASL_AUTHENTICATE, |w| {
                    w.bytes(&message)?;
                    Ok(())
                })?;
                let mut r = R::new(&body, false);
                let code = r.i16()?;
                let msg = r.nstr()?;
                if code != 0 {
                    return Err(Fail::Code(
                        code,
                        format!("sasl authenticate: {}", msg.unwrap_or_default()),
                    ));
                }
                received = Some(r.bytes()?.to_vec());
                r.i64()?;
            }
            if state.is_finished() {
                return Ok(());
            }
        }
        Err(Fail::Proto("sasl exchange did not finish"))
    }

    /// Encode a request frame for `key` (header plus body).
    pub(crate) fn frame(
        &mut self,
        key: i16,
        body: impl FnOnce(&mut W) -> KResult<()>,
    ) -> KResult<(Vec<u8>, i32, bool)> {
        let (version, flex) = version_of(key)?;
        if key != api::API_VERSIONS
            && let Some(&(min, max)) = self.versions.get(&key)
            && !(min..=max).contains(&version)
        {
            return Err(Fail::Local(Error::Kafka("unsupported api version")));
        }
        if key != api::API_VERSIONS && !self.versions.contains_key(&key) {
            return Err(Fail::Local(Error::Kafka("unsupported api version")));
        }
        self.corr = self.corr.wrapping_add(1);
        let mut header = W::new(false);
        header.i16(key).i16(version).i32(self.corr);
        header.str(&self.settings.client_id)?;
        if flex {
            header.uvarint(0);
        }
        let mut w = W::new(flex);
        body(&mut w)?;
        let len = header.buf.len() + w.buf.len();
        let mut frame = Vec::with_capacity(4 + len);
        frame.extend_from_slice(
            &i32::try_from(len)
                .map_err(|_| Fail::Local(Error::Overflow))?
                .to_be_bytes(),
        );
        frame.extend_from_slice(&header.buf);
        frame.extend_from_slice(&w.buf);
        Ok((frame, self.corr, flex && key != api::API_VERSIONS))
    }

    pub(crate) fn send_frame(&mut self, frame: &[u8]) -> KResult<()> {
        self.stream.write_all(frame).map_err(io_fail)?;
        self.stream.flush().map_err(io_fail)
    }

    pub(crate) fn recv(&mut self, corr: i32, flex_header: bool) -> KResult<Vec<u8>> {
        let mut len = [0u8; 4];
        self.stream.read_exact(&mut len).map_err(io_fail)?;
        let len = i32::from_be_bytes(len);
        let len = usize::try_from(len).map_err(|_| MALFORMED)?;
        if len < 4 || len > self.settings.max_response {
            return Err(Fail::Proto("response size"));
        }
        let mut body = vec![0u8; len];
        self.stream.read_exact(&mut body).map_err(io_fail)?;
        let mut r = R::new(&body, flex_header);
        if r.i32()? != corr {
            return Err(Fail::Proto("correlation id"));
        }
        r.tags()?;
        let at = r.at;
        body.drain(..at);
        Ok(body)
    }

    /// Send one request and read its response body (after the header).
    pub(crate) fn call(
        &mut self,
        key: i16,
        body: impl FnOnce(&mut W) -> KResult<()>,
    ) -> KResult<Vec<u8>> {
        let (frame, corr, flex) = self.frame(key, body)?;
        self.send_frame(&frame)?;
        self.recv(corr, flex)
    }

    /// Change the read timeout (group joins block for the rebalance).
    pub(crate) fn set_read_timeout(&self, t: Duration) -> KResult<()> {
        self.stream.tcp().set_read_timeout(Some(t)).map_err(io_fail)
    }
}

fn host_of(addr: &str) -> &str {
    if let Some(rest) = addr.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        return &rest[..end];
    }
    addr.rsplit_once(':').map_or(addr, |(h, _)| h)
}

// ----------------------------------------------------------------------
// Record batches
// ----------------------------------------------------------------------

/// Producer identity stamped on a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProducerStamp {
    pub(crate) id: i64,
    pub(crate) epoch: i16,
    pub(crate) sequence: i32,
    pub(crate) transactional: bool,
}
impl ProducerStamp {
    pub(crate) const NONE: Self = Self {
        id: -1,
        epoch: -1,
        sequence: -1,
        transactional: false,
    };
}

fn encode_record(out: &mut W, delta: i64, r: &KafkaRecord, base_ts: i64) -> KResult<()> {
    let mut body = W::new(false);
    body.i8(0);
    body.varint(r.timestamp.saturating_sub(base_ts));
    body.varint(delta);
    match &r.key {
        None => body.varint(-1),
        Some(k) => {
            body.varint(k.len() as i64);
            body.buf.extend_from_slice(k);
            &mut body
        }
    };
    match &r.value {
        None => body.varint(-1),
        Some(v) => {
            body.varint(v.len() as i64);
            body.buf.extend_from_slice(v);
            &mut body
        }
    };
    body.varint(r.headers.len() as i64);
    for (k, v) in &r.headers {
        body.varint(k.len() as i64);
        body.buf.extend_from_slice(k.as_bytes());
        body.varint(v.len() as i64);
        body.buf.extend_from_slice(v);
    }
    out.varint(body.buf.len() as i64);
    out.buf.extend_from_slice(&body.buf);
    Ok(())
}

/// Encode a v2 record batch (base offset 0).
pub(crate) fn encode_batch(
    records: &[KafkaRecord],
    compression: KafkaCompression,
    stamp: ProducerStamp,
) -> KResult<Vec<u8>> {
    if records.is_empty() {
        return Err(Fail::Local(Error::Kafka("empty batch")));
    }
    let count = i32::try_from(records.len()).map_err(|_| Fail::Local(Error::Overflow))?;
    let base_ts = records.iter().map(|r| r.timestamp).min().unwrap_or(0);
    let max_ts = records.iter().map(|r| r.timestamp).max().unwrap_or(0);
    let mut raw = W::new(false);
    for (i, r) in records.iter().enumerate() {
        encode_record(&mut raw, i as i64, r, base_ts)?;
    }
    let payload = compress(compression, &raw.buf)?;
    let mut tail = W::new(false);
    let mut attributes = compression.code();
    if stamp.transactional {
        attributes |= 1 << 4;
    }
    tail.i16(attributes)
        .i32(count - 1)
        .i64(base_ts)
        .i64(max_ts)
        .i64(stamp.id)
        .i16(stamp.epoch)
        .i32(stamp.sequence)
        .i32(count);
    tail.buf.extend_from_slice(&payload);
    let crc = crc32c::crc32c(&tail.buf);
    let mut batch = W::new(false);
    // batchLength counts from partitionLeaderEpoch to the end.
    let length = 4 + 1 + 4 + tail.buf.len();
    batch
        .i64(0)
        .i32(i32::try_from(length).map_err(|_| Fail::Local(Error::Overflow))?)
        .i32(-1)
        .i8(2);
    batch.buf.extend_from_slice(&crc.to_be_bytes());
    batch.buf.extend_from_slice(&tail.buf);
    Ok(batch.buf)
}

/// One decoded batch.
#[derive(Clone, Debug)]
pub(crate) struct Batch {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) base_offset: i64,
    pub(crate) last_offset: i64,
    pub(crate) producer_id: i64,
    pub(crate) transactional: bool,
    /// `Some(commit)` for a control batch (commit or abort marker).
    pub(crate) control: Option<bool>,
    pub(crate) records: Vec<(i64, KafkaRecord)>,
}

/// Decode concatenated v2 batches; a truncated final batch (the broker cuts
/// at the size limit) is dropped. Records are only decompressed and decoded
/// for batches `keep` accepts (by base offset and producer id).
pub(crate) fn decode_batches(bytes: &[u8], max_decoded: usize) -> KResult<Vec<Batch>> {
    let mut out = Vec::new();
    let mut r = R::new(bytes, false);
    while r.rest() >= 12 {
        let start = r.at;
        let base_offset = r.i64()?;
        let length = r.i32()?;
        let length = usize::try_from(length).map_err(|_| Fail::Proto("batch length"))?;
        if length > r.rest() {
            // Partial trailing batch.
            break;
        }
        let body = r.take(length)?;
        let mut b = R::new(body, false);
        let _leader_epoch = b.i32()?;
        let magic = b.i8()?;
        if magic != 2 {
            return Err(Fail::Proto("legacy message format"));
        }
        let crc = b.u32()?;
        if crc32c::crc32c(&body[b.at..]) != crc {
            return Err(Fail::Proto("batch checksum"));
        }
        let attributes = b.i16()?;
        let last_delta = b.i32()?;
        let base_ts = b.i64()?;
        let _max_ts = b.i64()?;
        let producer_id = b.i64()?;
        let _epoch = b.i16()?;
        let _seq = b.i32()?;
        let count = b.i32()?;
        if count < 0 || last_delta < 0 {
            return Err(Fail::Proto("batch header"));
        }
        let transactional = attributes & (1 << 4) != 0;
        let is_control = attributes & (1 << 5) != 0;
        let codec = attributes & 0x7;
        let payload = &body[b.at..];
        let raw = decompress(codec, payload, max_decoded)?;
        let mut rr = R::new(&raw, false);
        let mut records = Vec::new();
        let mut control = None;
        for _ in 0..count {
            let len = rr.varint()?;
            let len = usize::try_from(len).map_err(|_| Fail::Proto("record length"))?;
            let rec = rr.take(len)?;
            let mut x = R::new(rec, false);
            x.i8()?;
            let ts_delta = x.varint()?;
            let off_delta = x.varint()?;
            let key = read_varbytes(&mut x)?;
            let value = read_varbytes(&mut x)?;
            let nh = x.varint()?;
            if nh < 0 || nh as usize > x.rest() {
                return Err(Fail::Proto("record headers"));
            }
            let mut headers = BTreeMap::new();
            for _ in 0..nh {
                let k = read_varbytes(&mut x)?.ok_or(Fail::Proto("record headers"))?;
                let k = String::from_utf8(k).map_err(|_| Fail::Proto("record headers"))?;
                let v = read_varbytes(&mut x)?.unwrap_or_default();
                headers.insert(k, v);
            }
            if x.rest() != 0 {
                return Err(Fail::Proto("record trailing bytes"));
            }
            let offset = base_offset
                .checked_add(off_delta)
                .ok_or(Fail::Proto("record offset"))?;
            if is_control {
                // Key: version i16, type i16 (0 abort, 1 commit).
                let k = key.as_deref().ok_or(Fail::Proto("control record"))?;
                let mut kr = R::new(k, false);
                kr.i16()?;
                control = Some(kr.i16()? == 1);
                continue;
            }
            records.push((
                offset,
                KafkaRecord {
                    key,
                    value,
                    headers,
                    timestamp: base_ts.saturating_add(ts_delta),
                },
            ));
        }
        if is_control && control.is_none() {
            return Err(Fail::Proto("control record"));
        }
        let _ = start;
        out.push(Batch {
            base_offset,
            last_offset: base_offset
                .checked_add(i64::from(last_delta))
                .ok_or(Fail::Proto("batch offset"))?,
            producer_id,
            transactional,
            control,
            records,
        });
    }
    Ok(out)
}

fn read_varbytes(r: &mut R<'_>) -> KResult<Option<Vec<u8>>> {
    let n = r.varint()?;
    if n < 0 {
        return Ok(None);
    }
    let n = usize::try_from(n).map_err(|_| MALFORMED)?;
    Ok(Some(r.take(n)?.to_vec()))
}

// ----------------------------------------------------------------------
// Compression
// ----------------------------------------------------------------------

const XERIAL_MAGIC: [u8; 8] = [0x82, b'S', b'N', b'A', b'P', b'P', b'Y', 0];
const XERIAL_BLOCK: usize = 32 * 1024;

fn compress(codec: KafkaCompression, raw: &[u8]) -> KResult<Vec<u8>> {
    let io = |e: std::io::Error| Fail::Io(format!("compress: {e}"));
    Ok(match codec {
        KafkaCompression::None => raw.to_vec(),
        KafkaCompression::Gzip => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(raw).map_err(io)?;
            e.finish().map_err(io)?
        }
        KafkaCompression::Snappy => {
            // The Java client's (xerial snappy-java) framing.
            let mut out = Vec::with_capacity(raw.len() / 2 + 32);
            out.extend_from_slice(&XERIAL_MAGIC);
            out.extend_from_slice(&1i32.to_be_bytes());
            out.extend_from_slice(&1i32.to_be_bytes());
            let mut enc = snap::raw::Encoder::new();
            for chunk in raw.chunks(XERIAL_BLOCK) {
                let block = enc
                    .compress_vec(chunk)
                    .map_err(|_| Fail::Local(Error::Kafka("snappy")))?;
                out.extend_from_slice(
                    &i32::try_from(block.len())
                        .map_err(|_| Fail::Local(Error::Overflow))?
                        .to_be_bytes(),
                );
                out.extend_from_slice(&block);
            }
            out
        }
        KafkaCompression::Lz4 => {
            let mut e = lz4_flex::frame::FrameEncoder::new(Vec::new());
            e.write_all(raw).map_err(io)?;
            e.finish().map_err(|_| Fail::Local(Error::Kafka("lz4")))?
        }
        KafkaCompression::Zstd => zstd::bulk::compress(raw, 3).map_err(io)?,
    })
}

fn read_bounded(mut r: impl Read, max: usize) -> KResult<Vec<u8>> {
    let mut out = Vec::new();
    let limit = u64::try_from(max).map_err(|_| Fail::Local(Error::Overflow))? + 1;
    (&mut r)
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|_| Fail::Proto("corrupt compressed batch"))?;
    if out.len() > max {
        return Err(Fail::Proto("decompressed batch too large"));
    }
    Ok(out)
}

fn decompress(codec: i16, payload: &[u8], max: usize) -> KResult<Vec<u8>> {
    match codec {
        0 => Ok(payload.to_vec()),
        1 => read_bounded(flate2::read::GzDecoder::new(payload), max),
        2 => {
            let mut dec = snap::raw::Decoder::new();
            let mut one = |block: &[u8], out: &mut Vec<u8>| -> KResult<()> {
                let n = snap::raw::decompress_len(block)
                    .map_err(|_| Fail::Proto("corrupt compressed batch"))?;
                if out.len().saturating_add(n) > max {
                    return Err(Fail::Proto("decompressed batch too large"));
                }
                let start = out.len();
                out.resize(start + n, 0);
                dec.decompress(block, &mut out[start..])
                    .map_err(|_| Fail::Proto("corrupt compressed batch"))?;
                Ok(())
            };
            let mut out = Vec::new();
            if payload.starts_with(&XERIAL_MAGIC) {
                let mut r = R::new(&payload[XERIAL_MAGIC.len()..], false);
                r.i32()?;
                r.i32()?;
                while r.rest() > 0 {
                    let n = usize::try_from(r.i32()?)
                        .map_err(|_| Fail::Proto("corrupt compressed batch"))?;
                    one(r.take(n)?, &mut out)?;
                }
            } else {
                one(payload, &mut out)?;
            }
            Ok(out)
        }
        3 => read_bounded(lz4_flex::frame::FrameDecoder::new(payload), max),
        4 => read_bounded(
            zstd::stream::read::Decoder::new(payload)
                .map_err(|_| Fail::Proto("corrupt compressed batch"))?,
            max,
        ),
        _ => Err(Fail::Proto("unknown compression codec")),
    }
}

// ----------------------------------------------------------------------
// Fetch filtering
// ----------------------------------------------------------------------

/// Data records of `batches` a read-committed consumer sees, given the
/// fetch response's aborted transactions `(producer id, first offset)`, in
/// offset order; and the offset after the last batch (control and aborted
/// batches included), if any batch was returned.
pub(crate) fn committed_records(
    batches: Vec<Batch>,
    aborted: &[(i64, i64)],
    read_committed: bool,
) -> (Vec<(i64, KafkaRecord)>, Option<i64>) {
    let mut aborted: Vec<(i64, i64)> = aborted.to_vec();
    aborted.sort_by_key(|(_, first)| *first);
    let mut pending = aborted.into_iter().peekable();
    let mut open: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    let mut end = None;
    for batch in batches {
        end = Some(batch.last_offset + 1);
        while let Some(&(pid, first)) = pending.peek() {
            if first <= batch.last_offset {
                open.insert(pid);
                pending.next();
            } else {
                break;
            }
        }
        if let Some(commit) = batch.control {
            if !commit {
                open.remove(&batch.producer_id);
            }
            continue;
        }
        if read_committed && batch.transactional && open.contains(&batch.producer_id) {
            continue;
        }
        out.extend(batch.records);
    }
    (out, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(i: u8) -> KafkaRecord {
        let mut headers = BTreeMap::new();
        headers.insert("h".to_string(), vec![i; 3]);
        KafkaRecord {
            key: i.is_multiple_of(2).then(|| vec![i]),
            value: Some(vec![i; usize::from(i) * 50]),
            headers,
            timestamp: 1_000 + i64::from(i),
        }
    }

    #[test]
    fn batches_round_trip_in_every_codec() {
        let records: Vec<_> = (0..40).map(rec).collect();
        for codec in [
            KafkaCompression::None,
            KafkaCompression::Gzip,
            KafkaCompression::Snappy,
            KafkaCompression::Lz4,
            KafkaCompression::Zstd,
        ] {
            let stamp = ProducerStamp {
                id: 7,
                epoch: 2,
                sequence: 11,
                transactional: true,
            };
            let mut bytes = encode_batch(&records, codec, stamp).unwrap();
            // Rebase to offset 100 as a broker would.
            bytes[..8].copy_from_slice(&100i64.to_be_bytes());
            let batches = decode_batches(&bytes, 1 << 20).unwrap();
            assert_eq!(batches.len(), 1, "{codec:?}");
            let b = &batches[0];
            assert_eq!((b.base_offset, b.last_offset), (100, 139));
            assert!(b.transactional && b.control.is_none());
            assert_eq!(b.producer_id, 7);
            let got: Vec<_> = b.records.iter().map(|(_, r)| r.clone()).collect();
            assert_eq!(got, records, "{codec:?}");
            assert_eq!(b.records[5].0, 105);
            // A bounded decoder refuses an oversized expansion.
            if codec != KafkaCompression::None {
                assert!(decode_batches(&bytes, 100).is_err(), "{codec:?}");
            }
            // Any flipped payload byte fails the checksum.
            let mut bad = bytes.clone();
            let n = bad.len();
            bad[n - 3] ^= 0x40;
            assert!(decode_batches(&bad, 1 << 20).is_err(), "{codec:?}");
            // A truncated tail batch is dropped, not an error.
            let mut two = bytes.clone();
            two.extend_from_slice(&bytes[..bytes.len() - 5]);
            assert_eq!(decode_batches(&two, 1 << 20).unwrap().len(), 1);
        }
    }

    #[test]
    fn raw_snappy_blocks_decode_too() {
        let raw = vec![9u8; 1000];
        let block = snap::raw::Encoder::new().compress_vec(&raw).unwrap();
        assert_eq!(decompress(2, &block, 4096).unwrap(), raw);
        assert!(decompress(2, &block, 999).is_err());
        assert!(decompress(9, &block, 4096).is_err());
    }

    #[test]
    fn malformed_bytes_are_errors_not_panics() {
        let good = encode_batch(&[rec(3)], KafkaCompression::None, ProducerStamp::NONE).unwrap();
        for cut in 0..good.len() {
            let _ = decode_batches(&good[..cut], 1 << 20);
        }
        let mut x: u64 = 0x1234_5678;
        for _ in 0..2000 {
            let mut b = good.clone();
            for _ in 0..3 {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let i = (x >> 33) as usize % b.len();
                b[i] ^= (x >> 13) as u8;
            }
            let _ = decode_batches(&b, 1 << 20);
        }
        // Readers reject hostile lengths without allocating.
        let mut r = R::new(&[0x7f, 0xff, 0xff, 0xff], false);
        assert!(r.array().is_err());
        let mut r = R::new(
            &[
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1,
            ],
            true,
        );
        assert!(r.uvarint().is_err());
    }

    #[test]
    fn aborted_transactions_are_filtered() {
        let mk = |base: i64, pid: i64, txn: bool, control: Option<bool>, n: i64| Batch {
            base_offset: base,
            last_offset: base + n - 1,
            producer_id: pid,
            transactional: txn,
            control,
            records: if control.is_some() {
                Vec::new()
            } else {
                (0..n).map(|i| (base + i, rec(i as u8))).collect()
            },
        };
        let batches = vec![
            mk(0, 1, true, None, 2),        // aborted
            mk(2, 2, true, None, 2),        // committed
            mk(4, 1, true, Some(false), 1), // abort marker for 1
            mk(5, 2, true, Some(true), 1),  // commit marker for 2
            mk(6, 1, true, None, 1),        // a later, committed txn of 1
            mk(7, 1, true, Some(true), 1),
            mk(8, -1, false, None, 1), // non-transactional
        ];
        let (recs, end) = committed_records(batches.clone(), &[(1, 0)], true);
        let offsets: Vec<i64> = recs.iter().map(|(o, _)| *o).collect();
        assert_eq!(offsets, vec![2, 3, 6, 8]);
        assert_eq!(end, Some(9));
        let (recs, _) = committed_records(batches, &[(1, 0)], false);
        assert_eq!(recs.len(), 6);
    }

    #[test]
    fn varints_and_compact_strings_round_trip() {
        let mut w = W::new(true);
        for v in [0i64, 1, -1, 63, -64, 300, i64::MAX, i64::MIN] {
            w.varint(v);
        }
        w.str("héllo")
            .unwrap()
            .nstr(None)
            .unwrap()
            .bytes(&[1, 2])
            .unwrap()
            .tags();
        let mut r = R::new(&w.buf, true);
        for v in [0i64, 1, -1, 63, -64, 300, i64::MAX, i64::MIN] {
            assert_eq!(r.varint().unwrap(), v);
        }
        assert_eq!(r.str().unwrap(), "héllo");
        assert_eq!(r.nstr().unwrap(), None);
        assert_eq!(r.bytes().unwrap(), &[1, 2]);
        r.tags().unwrap();
        assert_eq!(r.rest(), 0);
        assert_eq!(host_of("127.0.0.1:9092"), "127.0.0.1");
        assert_eq!(host_of("[::1]:9092"), "::1");
        assert_eq!(host_of("broker"), "broker");
    }
}
