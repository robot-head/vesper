//! The QUIC stream service HTTP/3 runs on.
//!
//! [`Transport`] is the small slice of a QUIC connection HTTP/3 needs:
//! open unidirectional streams, write/finish/read/reset streams, learn
//! about streams the peer opened, and close the connection with an
//! application error code. [`crate::warp::WarpTransport`] implements it for
//! a Warp endpoint connection.
//!
//! QUIC DATAGRAM frames (RFC 9221) are optional: the default methods say
//! the transport has none, and HTTP Datagrams then travel only as DATAGRAM
//! capsules on their stream (RFC 9297 §3.5).

use crate::error::Code;

/// Why a stream operation did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// Not possible yet: the handshake is running or a stream limit is
    /// reached. Retry on a later poll.
    NotReady,
    /// The stream's sending part was reset, or its receiving part was reset
    /// by the peer (RESET_STREAM), with this application code.
    Reset(u64),
    /// The stream does not exist (closed and forgotten, or never opened).
    UnknownStream,
    /// The connection is closed.
    Closed,
    /// The transport has no QUIC DATAGRAM support, or the peer did not
    /// offer it (no max_datagram_frame_size transport parameter).
    Unsupported,
    /// The datagram does not fit one QUIC DATAGRAM frame.
    TooLarge,
}

/// The outcome of a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadOutcome {
    pub len: usize,
    /// The final byte of the stream was read.
    pub fin: bool,
}

pub trait Transport {
    /// Opens a unidirectional stream of ours.
    fn open_uni(&mut self) -> Result<u64, TransportError>;
    /// Opens a bidirectional stream of ours (clients: request streams).
    fn open_bidi(&mut self) -> Result<u64, TransportError>;
    /// Buffers bytes; returns how many were accepted (possibly 0).
    fn write(&mut self, stream_id: u64, data: &[u8]) -> Result<usize, TransportError>;
    /// Ends the sending part after the bytes written.
    fn finish(&mut self, stream_id: u64) -> Result<(), TransportError>;
    /// Reads in-order bytes.
    fn read(&mut self, stream_id: u64, out: &mut [u8]) -> Result<ReadOutcome, TransportError>;
    /// Abandons the sending part (RESET_STREAM).
    fn reset(&mut self, stream_id: u64, code: Code);
    /// Asks the peer to stop sending (STOP_SENDING).
    fn stop_sending(&mut self, stream_id: u64, code: Code);
    /// The next stream the peer opened, if any.
    fn accept(&mut self) -> Option<u64>;
    /// Closes the connection with an application error (CONNECTION_CLOSE).
    fn close(&mut self, code: Code);

    /// The largest datagram payload the peer accepts, when both sides
    /// negotiated QUIC DATAGRAM frames; `None` otherwise.
    fn max_datagram_size(&mut self) -> Option<usize> {
        None
    }
    /// Queues one QUIC DATAGRAM frame (unreliable, unordered).
    fn send_datagram(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let _ = data;
        Err(TransportError::Unsupported)
    }
    /// The next received QUIC DATAGRAM payload, if any.
    fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        None
    }

    /// Hints a stream's RFC 9218 priority to the transport's own packet
    /// scheduler, so bytes already handed over are sent in the same order
    /// HTTP/3 chose. Transports without a scheduler ignore it.
    fn set_priority(&mut self, stream_id: u64, urgency: u8, incremental: bool) {
        let _ = (stream_id, urgency, incremental);
    }
}

/// Bit 1 of a QUIC stream ID: unidirectional.
pub const fn is_uni(stream_id: u64) -> bool {
    stream_id & 2 != 0
}

/// Bit 0 of a QUIC stream ID: server-initiated.
pub const fn is_server_initiated(stream_id: u64) -> bool {
    stream_id & 1 != 0
}
