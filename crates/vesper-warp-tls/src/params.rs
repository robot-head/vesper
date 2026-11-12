//! QUIC transport parameters (RFC 9000 §7.3, §7.4 and §18): encoding,
//! decoding of untrusted bytes, value validation and the connection-ID
//! authentication checks.

use std::fmt;

use vesper_warp_crypto::{MAX_CID_LEN, Side};

use crate::varint::{self, Reader, VARINT_MAX};

/// Transport parameter identifiers defined by RFC 9000 §18.2.
pub mod id {
    pub const ORIGINAL_DESTINATION_CONNECTION_ID: u64 = 0x00;
    pub const MAX_IDLE_TIMEOUT: u64 = 0x01;
    pub const STATELESS_RESET_TOKEN: u64 = 0x02;
    pub const MAX_UDP_PAYLOAD_SIZE: u64 = 0x03;
    pub const INITIAL_MAX_DATA: u64 = 0x04;
    pub const INITIAL_MAX_STREAM_DATA_BIDI_LOCAL: u64 = 0x05;
    pub const INITIAL_MAX_STREAM_DATA_BIDI_REMOTE: u64 = 0x06;
    pub const INITIAL_MAX_STREAM_DATA_UNI: u64 = 0x07;
    pub const INITIAL_MAX_STREAMS_BIDI: u64 = 0x08;
    pub const INITIAL_MAX_STREAMS_UNI: u64 = 0x09;
    pub const ACK_DELAY_EXPONENT: u64 = 0x0a;
    pub const MAX_ACK_DELAY: u64 = 0x0b;
    pub const DISABLE_ACTIVE_MIGRATION: u64 = 0x0c;
    pub const PREFERRED_ADDRESS: u64 = 0x0d;
    pub const ACTIVE_CONNECTION_ID_LIMIT: u64 = 0x0e;
    pub const INITIAL_SOURCE_CONNECTION_ID: u64 = 0x0f;
    pub const RETRY_SOURCE_CONNECTION_ID: u64 = 0x10;
}

/// Largest stream count a `initial_max_streams_*` parameter may carry
/// (RFC 9000 §4.6).
pub const MAX_STREAMS_LIMIT: u64 = 1 << 60;
/// Smallest valid `max_udp_payload_size` (RFC 9000 §18.2).
pub const MIN_UDP_PAYLOAD_SIZE: u64 = 1200;
/// Default and largest meaningful `max_udp_payload_size`.
pub const DEFAULT_MAX_UDP_PAYLOAD_SIZE: u64 = 65_527;
/// Largest valid `ack_delay_exponent`.
pub const MAX_ACK_DELAY_EXPONENT: u64 = 20;
/// `max_ack_delay` values at or above this are invalid (milliseconds).
pub const MAX_ACK_DELAY_LIMIT_MS: u64 = 1 << 14;
/// Stateless reset token length.
pub const RESET_TOKEN_LEN: usize = 16;

/// A QUIC v1 connection ID of 0 to 20 bytes, stored inline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId {
    len: u8,
    bytes: [u8; MAX_CID_LEN],
}

impl ConnectionId {
    /// Copies `cid`; longer than [`MAX_CID_LEN`] is an error.
    pub fn new(cid: &[u8]) -> Result<Self, TransportParameterError> {
        if cid.len() > MAX_CID_LEN {
            return Err(TransportParameterError::ConnectionIdTooLong(cid.len()));
        }
        let mut bytes = [0u8; MAX_CID_LEN];
        bytes[..cid.len()].copy_from_slice(cid);
        Ok(Self {
            len: cid.len() as u8,
            bytes,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl fmt::Debug for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConnectionId(")?;
        for b in self.as_bytes() {
            write!(f, "{b:02x}")?;
        }
        write!(f, ")")
    }
}

/// The server's `preferred_address` (RFC 9000 §18.2, §9.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreferredAddress {
    pub ipv4: [u8; 4],
    pub ipv4_port: u16,
    pub ipv6: [u8; 16],
    pub ipv6_port: u16,
    /// Must be non-empty.
    pub connection_id: ConnectionId,
    pub stateless_reset_token: [u8; RESET_TOKEN_LEN],
}

/// The RFC 9000 §18.2 transport parameters. Absent integer parameters
/// take their RFC defaults; [`TransportParameters::default`] is exactly
/// the set of defaults, which advertises no flow-control credit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportParameters {
    /// Server only: the Destination Connection ID of the client's first
    /// Initial packet.
    pub original_destination_connection_id: Option<ConnectionId>,
    /// Milliseconds; 0 disables the idle timeout.
    pub max_idle_timeout_ms: u64,
    /// Server only.
    pub stateless_reset_token: Option<[u8; RESET_TOKEN_LEN]>,
    pub max_udp_payload_size: u64,
    pub initial_max_data: u64,
    pub initial_max_stream_data_bidi_local: u64,
    pub initial_max_stream_data_bidi_remote: u64,
    pub initial_max_stream_data_uni: u64,
    pub initial_max_streams_bidi: u64,
    pub initial_max_streams_uni: u64,
    pub ack_delay_exponent: u64,
    /// Milliseconds.
    pub max_ack_delay_ms: u64,
    pub disable_active_migration: bool,
    /// Server only.
    pub preferred_address: Option<PreferredAddress>,
    pub active_connection_id_limit: u64,
    /// Required from both endpoints: the Source Connection ID of the
    /// sender's first Initial packet.
    pub initial_source_connection_id: Option<ConnectionId>,
    /// Server only, present exactly when the server sent a Retry.
    pub retry_source_connection_id: Option<ConnectionId>,
}

impl Default for TransportParameters {
    fn default() -> Self {
        Self {
            original_destination_connection_id: None,
            max_idle_timeout_ms: 0,
            stateless_reset_token: None,
            max_udp_payload_size: DEFAULT_MAX_UDP_PAYLOAD_SIZE,
            initial_max_data: 0,
            initial_max_stream_data_bidi_local: 0,
            initial_max_stream_data_bidi_remote: 0,
            initial_max_stream_data_uni: 0,
            initial_max_streams_bidi: 0,
            initial_max_streams_uni: 0,
            ack_delay_exponent: 3,
            max_ack_delay_ms: 25,
            disable_active_migration: false,
            preferred_address: None,
            active_connection_id_limit: 2,
            initial_source_connection_id: None,
            retry_source_connection_id: None,
        }
    }
}

/// Why a transport parameter list was rejected. Every variant is a
/// connection error of type `TRANSPORT_PARAMETER_ERROR` (0x08) when it
/// concerns the peer's parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportParameterError {
    /// The list ends inside a parameter id, length or value.
    Truncated,
    /// A parameter defined by RFC 9000 appears twice.
    Duplicate(u64),
    /// A value is out of range or its encoding does not fill its length.
    InvalidValue(u64),
    /// A client sent (or is about to send) a server-only parameter.
    ServerOnly(u64),
    /// A required parameter is absent.
    Missing(u64),
    /// A connection-ID parameter does not match the connection ID the
    /// packets carried (RFC 9000 §7.3).
    ConnectionIdMismatch(u64),
    /// A connection ID is longer than 20 bytes.
    ConnectionIdTooLong(usize),
}

impl TransportParameterError {
    /// The QUIC transport error code for this failure.
    pub const fn quic_error_code(self) -> u64 {
        crate::error::code::TRANSPORT_PARAMETER_ERROR
    }
}

impl fmt::Display for TransportParameterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for TransportParameterError {}

/// The connection IDs the receiver of a parameter list observed in packet
/// headers, against which RFC 9000 §7.3 authenticates the parameters.
#[derive(Clone, Copy, Debug)]
pub struct CidExpectations<'a> {
    /// Source Connection ID of the first Initial packet the peer sent.
    pub peer_initial_scid: &'a [u8],
    /// Client only: the Destination Connection ID of the client's first
    /// Initial packet.
    pub original_dcid: Option<&'a [u8]>,
    /// Client only: the Source Connection ID of the server's Retry packet,
    /// if the client processed one.
    pub retry_scid: Option<&'a [u8]>,
}

fn invalid(id: u64) -> TransportParameterError {
    TransportParameterError::InvalidValue(id)
}

fn is_server_only(pid: u64) -> bool {
    matches!(
        pid,
        id::ORIGINAL_DESTINATION_CONNECTION_ID
            | id::STATELESS_RESET_TOKEN
            | id::PREFERRED_ADDRESS
            | id::RETRY_SOURCE_CONNECTION_ID
    )
}

fn put_varint_param(out: &mut Vec<u8>, pid: u64, v: u64) {
    // Values are checked by `check_values` before encoding.
    let len = varint::encoded_len(v).expect("validated varint");
    varint::write(out, pid);
    varint::write(out, len as u64);
    varint::write(out, v);
}

fn put_bytes_param(out: &mut Vec<u8>, pid: u64, bytes: &[u8]) {
    varint::write(out, pid);
    varint::write(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

impl TransportParameters {
    /// Checks every value against its RFC 9000 §18.2 range and, for
    /// `sender == Side::Client`, that no server-only parameter is set.
    pub fn check_values(&self, sender: Side) -> Result<(), TransportParameterError> {
        let varints = [
            (id::MAX_IDLE_TIMEOUT, self.max_idle_timeout_ms),
            (id::MAX_UDP_PAYLOAD_SIZE, self.max_udp_payload_size),
            (id::INITIAL_MAX_DATA, self.initial_max_data),
            (
                id::INITIAL_MAX_STREAM_DATA_BIDI_LOCAL,
                self.initial_max_stream_data_bidi_local,
            ),
            (
                id::INITIAL_MAX_STREAM_DATA_BIDI_REMOTE,
                self.initial_max_stream_data_bidi_remote,
            ),
            (
                id::INITIAL_MAX_STREAM_DATA_UNI,
                self.initial_max_stream_data_uni,
            ),
            (id::INITIAL_MAX_STREAMS_BIDI, self.initial_max_streams_bidi),
            (id::INITIAL_MAX_STREAMS_UNI, self.initial_max_streams_uni),
            (id::ACK_DELAY_EXPONENT, self.ack_delay_exponent),
            (id::MAX_ACK_DELAY, self.max_ack_delay_ms),
            (
                id::ACTIVE_CONNECTION_ID_LIMIT,
                self.active_connection_id_limit,
            ),
        ];
        for (pid, v) in varints {
            if v > VARINT_MAX {
                return Err(invalid(pid));
            }
        }
        if self.max_udp_payload_size < MIN_UDP_PAYLOAD_SIZE {
            return Err(invalid(id::MAX_UDP_PAYLOAD_SIZE));
        }
        if self.initial_max_streams_bidi > MAX_STREAMS_LIMIT {
            return Err(invalid(id::INITIAL_MAX_STREAMS_BIDI));
        }
        if self.initial_max_streams_uni > MAX_STREAMS_LIMIT {
            return Err(invalid(id::INITIAL_MAX_STREAMS_UNI));
        }
        if self.ack_delay_exponent > MAX_ACK_DELAY_EXPONENT {
            return Err(invalid(id::ACK_DELAY_EXPONENT));
        }
        if self.max_ack_delay_ms >= MAX_ACK_DELAY_LIMIT_MS {
            return Err(invalid(id::MAX_ACK_DELAY));
        }
        if self.active_connection_id_limit < 2 {
            return Err(invalid(id::ACTIVE_CONNECTION_ID_LIMIT));
        }
        if let Some(pa) = &self.preferred_address
            && pa.connection_id.is_empty()
        {
            return Err(invalid(id::PREFERRED_ADDRESS));
        }
        if sender == Side::Client {
            let server_only = [
                (
                    id::ORIGINAL_DESTINATION_CONNECTION_ID,
                    self.original_destination_connection_id.is_some(),
                ),
                (
                    id::STATELESS_RESET_TOKEN,
                    self.stateless_reset_token.is_some(),
                ),
                (id::PREFERRED_ADDRESS, self.preferred_address.is_some()),
                (
                    id::RETRY_SOURCE_CONNECTION_ID,
                    self.retry_source_connection_id.is_some(),
                ),
            ];
            for (pid, present) in server_only {
                if present {
                    return Err(TransportParameterError::ServerOnly(pid));
                }
            }
        }
        Ok(())
    }

    /// Encodes the parameters as `sender` would send them. Parameters equal
    /// to their default are omitted, except connection IDs, which are
    /// written whenever set. Fails rather than encoding an invalid list.
    pub fn encode(&self, sender: Side) -> Result<Vec<u8>, TransportParameterError> {
        self.check_values(sender)?;
        if self.initial_source_connection_id.is_none() {
            return Err(TransportParameterError::Missing(
                id::INITIAL_SOURCE_CONNECTION_ID,
            ));
        }
        if sender == Side::Server && self.original_destination_connection_id.is_none() {
            return Err(TransportParameterError::Missing(
                id::ORIGINAL_DESTINATION_CONNECTION_ID,
            ));
        }
        let d = Self::default();
        let mut out = Vec::with_capacity(128);
        if let Some(cid) = &self.original_destination_connection_id {
            put_bytes_param(
                &mut out,
                id::ORIGINAL_DESTINATION_CONNECTION_ID,
                cid.as_bytes(),
            );
        }
        let varints = [
            (
                id::MAX_IDLE_TIMEOUT,
                self.max_idle_timeout_ms,
                d.max_idle_timeout_ms,
            ),
            (
                id::MAX_UDP_PAYLOAD_SIZE,
                self.max_udp_payload_size,
                d.max_udp_payload_size,
            ),
            (id::INITIAL_MAX_DATA, self.initial_max_data, 0),
            (
                id::INITIAL_MAX_STREAM_DATA_BIDI_LOCAL,
                self.initial_max_stream_data_bidi_local,
                0,
            ),
            (
                id::INITIAL_MAX_STREAM_DATA_BIDI_REMOTE,
                self.initial_max_stream_data_bidi_remote,
                0,
            ),
            (
                id::INITIAL_MAX_STREAM_DATA_UNI,
                self.initial_max_stream_data_uni,
                0,
            ),
            (
                id::INITIAL_MAX_STREAMS_BIDI,
                self.initial_max_streams_bidi,
                0,
            ),
            (id::INITIAL_MAX_STREAMS_UNI, self.initial_max_streams_uni, 0),
            (
                id::ACK_DELAY_EXPONENT,
                self.ack_delay_exponent,
                d.ack_delay_exponent,
            ),
            (id::MAX_ACK_DELAY, self.max_ack_delay_ms, d.max_ack_delay_ms),
            (
                id::ACTIVE_CONNECTION_ID_LIMIT,
                self.active_connection_id_limit,
                d.active_connection_id_limit,
            ),
        ];
        for (pid, v, default) in varints {
            if v != default {
                put_varint_param(&mut out, pid, v);
            }
        }
        if let Some(token) = &self.stateless_reset_token {
            put_bytes_param(&mut out, id::STATELESS_RESET_TOKEN, token);
        }
        if self.disable_active_migration {
            put_bytes_param(&mut out, id::DISABLE_ACTIVE_MIGRATION, &[]);
        }
        if let Some(pa) = &self.preferred_address {
            let mut v = Vec::with_capacity(41 + pa.connection_id.len());
            v.extend_from_slice(&pa.ipv4);
            v.extend_from_slice(&pa.ipv4_port.to_be_bytes());
            v.extend_from_slice(&pa.ipv6);
            v.extend_from_slice(&pa.ipv6_port.to_be_bytes());
            v.push(pa.connection_id.len() as u8);
            v.extend_from_slice(pa.connection_id.as_bytes());
            v.extend_from_slice(&pa.stateless_reset_token);
            put_bytes_param(&mut out, id::PREFERRED_ADDRESS, &v);
        }
        if let Some(cid) = &self.initial_source_connection_id {
            put_bytes_param(&mut out, id::INITIAL_SOURCE_CONNECTION_ID, cid.as_bytes());
        }
        if let Some(cid) = &self.retry_source_connection_id {
            put_bytes_param(&mut out, id::RETRY_SOURCE_CONNECTION_ID, cid.as_bytes());
        }
        Ok(out)
    }

    /// Decodes and value-checks a list sent by `sender`. Unknown and
    /// reserved (`31 * N + 27`) parameters are ignored, as RFC 9000 §7.4.2
    /// requires; duplicates of the parameters defined in §18.2 are errors.
    /// `initial_source_connection_id` (and, from a server,
    /// `original_destination_connection_id`) must be present.
    pub fn decode(bytes: &[u8], sender: Side) -> Result<Self, TransportParameterError> {
        let mut p = Self::default();
        let mut seen: u32 = 0;
        let mut r = Reader::new(bytes);
        while !r.is_empty() {
            let pid = r.varint().ok_or(TransportParameterError::Truncated)?;
            let len = r.varint().ok_or(TransportParameterError::Truncated)?;
            let len = usize::try_from(len).map_err(|_| TransportParameterError::Truncated)?;
            let value = r.bytes(len).ok_or(TransportParameterError::Truncated)?;
            if pid > id::RETRY_SOURCE_CONNECTION_ID {
                continue;
            }
            let bit = 1u32 << pid;
            if seen & bit != 0 {
                return Err(TransportParameterError::Duplicate(pid));
            }
            seen |= bit;
            if sender == Side::Client && is_server_only(pid) {
                return Err(TransportParameterError::ServerOnly(pid));
            }
            let as_varint = || {
                let mut vr = Reader::new(value);
                let v = vr.varint().ok_or(invalid(pid))?;
                if vr.is_empty() {
                    Ok(v)
                } else {
                    Err(invalid(pid))
                }
            };
            let as_cid = || ConnectionId::new(value).map_err(|_| invalid(pid));
            match pid {
                id::ORIGINAL_DESTINATION_CONNECTION_ID => {
                    p.original_destination_connection_id = Some(as_cid()?)
                }
                id::MAX_IDLE_TIMEOUT => p.max_idle_timeout_ms = as_varint()?,
                id::STATELESS_RESET_TOKEN => {
                    p.stateless_reset_token = Some(value.try_into().map_err(|_| invalid(pid))?);
                }
                id::MAX_UDP_PAYLOAD_SIZE => p.max_udp_payload_size = as_varint()?,
                id::INITIAL_MAX_DATA => p.initial_max_data = as_varint()?,
                id::INITIAL_MAX_STREAM_DATA_BIDI_LOCAL => {
                    p.initial_max_stream_data_bidi_local = as_varint()?
                }
                id::INITIAL_MAX_STREAM_DATA_BIDI_REMOTE => {
                    p.initial_max_stream_data_bidi_remote = as_varint()?
                }
                id::INITIAL_MAX_STREAM_DATA_UNI => p.initial_max_stream_data_uni = as_varint()?,
                id::INITIAL_MAX_STREAMS_BIDI => p.initial_max_streams_bidi = as_varint()?,
                id::INITIAL_MAX_STREAMS_UNI => p.initial_max_streams_uni = as_varint()?,
                id::ACK_DELAY_EXPONENT => p.ack_delay_exponent = as_varint()?,
                id::MAX_ACK_DELAY => p.max_ack_delay_ms = as_varint()?,
                id::DISABLE_ACTIVE_MIGRATION => {
                    if !value.is_empty() {
                        return Err(invalid(pid));
                    }
                    p.disable_active_migration = true;
                }
                id::PREFERRED_ADDRESS => p.preferred_address = Some(decode_preferred(value)?),
                id::ACTIVE_CONNECTION_ID_LIMIT => p.active_connection_id_limit = as_varint()?,
                id::INITIAL_SOURCE_CONNECTION_ID => {
                    p.initial_source_connection_id = Some(as_cid()?)
                }
                id::RETRY_SOURCE_CONNECTION_ID => p.retry_source_connection_id = Some(as_cid()?),
                _ => unreachable!("pid <= 0x10 is matched above"),
            }
        }
        p.check_values(sender)?;
        if p.initial_source_connection_id.is_none() {
            return Err(TransportParameterError::Missing(
                id::INITIAL_SOURCE_CONNECTION_ID,
            ));
        }
        if sender == Side::Server && p.original_destination_connection_id.is_none() {
            return Err(TransportParameterError::Missing(
                id::ORIGINAL_DESTINATION_CONNECTION_ID,
            ));
        }
        Ok(p)
    }

    /// RFC 9000 §7.3: authenticates the connection IDs in a decoded list
    /// sent by `sender` against the IDs seen in packet headers.
    pub fn authenticate_cids(
        &self,
        sender: Side,
        seen: &CidExpectations<'_>,
    ) -> Result<(), TransportParameterError> {
        let matches = |p: &Option<ConnectionId>, want: &[u8]| {
            p.as_ref().is_some_and(|c| c.as_bytes() == want)
        };
        if !matches(&self.initial_source_connection_id, seen.peer_initial_scid) {
            return Err(TransportParameterError::ConnectionIdMismatch(
                id::INITIAL_SOURCE_CONNECTION_ID,
            ));
        }
        if sender == Side::Server {
            let odcid = seen.original_dcid.unwrap_or_default();
            if !matches(&self.original_destination_connection_id, odcid) {
                return Err(TransportParameterError::ConnectionIdMismatch(
                    id::ORIGINAL_DESTINATION_CONNECTION_ID,
                ));
            }
            match (seen.retry_scid, &self.retry_source_connection_id) {
                (None, None) => {}
                (Some(want), got @ Some(_)) if matches(got, want) => {}
                (Some(_), None) => {
                    return Err(TransportParameterError::Missing(
                        id::RETRY_SOURCE_CONNECTION_ID,
                    ));
                }
                _ => {
                    return Err(TransportParameterError::ConnectionIdMismatch(
                        id::RETRY_SOURCE_CONNECTION_ID,
                    ));
                }
            }
        }
        Ok(())
    }
}

fn decode_preferred(v: &[u8]) -> Result<PreferredAddress, TransportParameterError> {
    let bad = || invalid(id::PREFERRED_ADDRESS);
    let mut r = Reader::new(v);
    let ipv4: [u8; 4] = r.bytes(4).ok_or_else(bad)?.try_into().map_err(|_| bad())?;
    let ipv4_port = u16::from_be_bytes(r.bytes(2).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
    let ipv6: [u8; 16] = r.bytes(16).ok_or_else(bad)?.try_into().map_err(|_| bad())?;
    let ipv6_port = u16::from_be_bytes(r.bytes(2).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
    let cid_len = usize::from(r.bytes(1).ok_or_else(bad)?[0]);
    if cid_len == 0 || cid_len > MAX_CID_LEN {
        return Err(bad());
    }
    let connection_id = ConnectionId::new(r.bytes(cid_len).ok_or_else(bad)?).map_err(|_| bad())?;
    let stateless_reset_token: [u8; RESET_TOKEN_LEN] = r
        .bytes(RESET_TOKEN_LEN)
        .ok_or_else(bad)?
        .try_into()
        .map_err(|_| bad())?;
    if !r.is_empty() {
        return Err(bad());
    }
    Ok(PreferredAddress {
        ipv4,
        ipv4_port,
        ipv6,
        ipv6_port,
        connection_id,
        stateless_reset_token,
    })
}
