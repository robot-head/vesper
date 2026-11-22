//! Extended CONNECT sessions (RFC 9220), HTTP Datagrams (RFC 9297) and
//! WebTransport over HTTP/3 (draft-ietf-webtrans-http3).
//!
//! All session state transitions, held-stream/datagram bounds, and capsule
//! framing live in Vesper (`vesper/src/session.ves`) and are driven through
//! [`crate::connection::ServerConnection`] and
//! [`crate::client::ClientConnection`].

/// What kind of protocol runs in an extended CONNECT session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// `:protocol` = `webtransport`: streams, datagrams, close capsules.
    WebTransport,
    /// Any other `:protocol`: a capsule stream and HTTP Datagrams only.
    Tunnel,
}

/// Something the application should know about a session.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// The peer opened a WebTransport stream in `session`.
    Stream {
        session: u64,
        stream_id: u64,
        bidi: bool,
    },
    /// An HTTP Datagram for `session` (WebTransport: the application
    /// payload; other protocols: the payload after the quarter stream ID).
    Datagram { session: u64, payload: Vec<u8> },
    /// The peer asked to wind the session down (DRAIN_WEBTRANSPORT_SESSION).
    Draining { session: u64 },
    /// The session ended: by a CLOSE_WEBTRANSPORT_SESSION capsule (`code`,
    /// `reason`), by a FIN on the CONNECT stream (code 0), or by a reset of
    /// it (`reset` holds the stream error code).
    Closed {
        session: u64,
        code: u32,
        reason: Vec<u8>,
        reset: Option<u64>,
    },
}

/// Why a session operation did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// No such session, or it is not established yet.
    NotEstablished,
    /// The session was closed (by either side).
    Closed,
    /// The operation needs a WebTransport session.
    NotWebTransport,
    /// No such WebTransport stream, or it belongs to another session.
    UnknownStream,
    /// The peer reset the stream (receiving) or sent STOP_SENDING
    /// (sending), with this code.
    StreamReset(u64),
    /// The datagram is larger than the peer accepts.
    TooLarge,
    /// A close reason over 1024 bytes or not UTF-8.
    BadReason,
    /// The transport cannot open a stream yet (stream credit).
    NotReady,
    /// The connection failed.
    Connection,
    /// The session belongs to another connection.
    Foreign,
}

/// How a datagram was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatagramPath {
    /// A QUIC DATAGRAM frame (RFC 9221): unreliable.
    Quic,
    /// A DATAGRAM capsule on the CONNECT stream: reliable and ordered.
    Capsule,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub sessions: u64,
    pub datagrams_received: u64,
    pub datagrams_sent: u64,
    /// Datagrams for no live session, or over the pending bound.
    pub datagrams_dropped: u64,
    pub streams_accepted: u64,
    pub streams_rejected: u64,
}
