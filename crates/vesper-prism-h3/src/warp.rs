//! [`Transport`] over one Vesper Warp endpoint connection.
//!
//! The adapter borrows the endpoint for one poll and maps the Warp stream
//! API onto the HTTP/3 transport calls. Streams the peer opened are found
//! by probing the next expected peer stream IDs: Warp opens peer streams
//! implicitly and in order (RFC 9000 §3.2), and this does not depend on the
//! bounded stream-event queue, which can drop events.
//!
//! [`Transport::close`] records the HTTP/3 code
//! ([`PeerStreams::closed_with`]) and closes the connection with
//! [`WarpEndpoint::close`], so the peer receives it in an application
//! CONNECTION_CLOSE (RFC 9000 §19.19, RFC 9114 §8). Warp frees the slot
//! after its closing period.

use vesper_warp::endpoint::{Responder as WarpResponder, TxTap, WarpEndpoint, WarpError};
use vesper_warp::stream::{Priority, Role, StreamError, stream_id};
use vesper_warp_shard::SlotHandle;
use vesper_warp_umem::XskBackend;

use crate::error::Code;
use crate::transport::{ReadOutcome, Transport, TransportError};

/// Per-connection discovery state that outlives one adapter borrow.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PeerStreams {
    next_bidi: u64,
    next_uni: u64,
    closed: Option<Code>,
}

impl PeerStreams {
    /// The HTTP/3 code the connection was closed with, if it was.
    pub fn closed_with(&self) -> Option<Code> {
        self.closed
    }
}

/// A Warp connection seen as an HTTP/3 transport.
pub struct WarpTransport<'a, B: XskBackend, R: WarpResponder, T: TxTap> {
    pub endpoint: &'a mut WarpEndpoint<B, R, T>,
    pub conn: SlotHandle,
    pub peers: &'a mut PeerStreams,
    /// Our role on this connection (peer streams have the other one).
    pub role: Role,
    pub now_ns: u64,
}

fn map(e: WarpError) -> TransportError {
    match e {
        WarpError::Handshaking => TransportError::NotReady,
        WarpError::Stream(StreamError::StreamLimit { .. }) => TransportError::NotReady,
        WarpError::Stream(StreamError::Reset { error_code, .. }) => {
            TransportError::Reset(error_code)
        }
        WarpError::Stream(StreamError::UnknownStream(_)) => TransportError::UnknownStream,
        WarpError::Stream(_) => TransportError::UnknownStream,
        _ => TransportError::Closed,
    }
}

impl<B: XskBackend, R: WarpResponder, T: TxTap> WarpTransport<'_, B, R, T> {
    fn peer_role(&self) -> Role {
        match self.role {
            Role::Client => Role::Server,
            Role::Server => Role::Client,
        }
    }
}

impl<B: XskBackend, R: WarpResponder, T: TxTap> Transport for WarpTransport<'_, B, R, T> {
    fn open_uni(&mut self) -> Result<u64, TransportError> {
        self.endpoint.open_stream(self.conn, true).map_err(map)
    }

    fn open_bidi(&mut self) -> Result<u64, TransportError> {
        self.endpoint.open_stream(self.conn, false).map_err(map)
    }

    fn write(&mut self, stream_id: u64, data: &[u8]) -> Result<usize, TransportError> {
        self.endpoint
            .stream_write(self.conn, stream_id, data)
            .map_err(map)
    }

    fn finish(&mut self, stream_id: u64) -> Result<(), TransportError> {
        match self.endpoint.stream_finish(self.conn, stream_id) {
            Ok(()) | Err(WarpError::Stream(StreamError::Finished(_))) => Ok(()),
            Err(e) => Err(map(e)),
        }
    }

    fn read(&mut self, stream_id: u64, out: &mut [u8]) -> Result<ReadOutcome, TransportError> {
        self.endpoint
            .stream_read(self.conn, stream_id, out)
            .map(|r| ReadOutcome {
                len: r.len,
                fin: r.fin,
            })
            .map_err(map)
    }

    fn set_priority(&mut self, stream_id: u64, urgency: u8, incremental: bool) {
        let _ = self.endpoint.stream_set_priority(
            self.conn,
            stream_id,
            Priority {
                urgency: urgency.min(7),
                incremental,
            },
        );
    }

    fn reset(&mut self, stream_id: u64, code: Code) {
        let _ = self.endpoint.stream_reset(self.conn, stream_id, code.0);
    }

    fn stop_sending(&mut self, stream_id: u64, code: Code) {
        let _ = self
            .endpoint
            .stream_stop_sending(self.conn, stream_id, code.0);
    }

    fn accept(&mut self) -> Option<u64> {
        let peer = self.peer_role();
        let streams = self.endpoint.streams_mut(self.conn).ok()?;
        for uni in [true, false] {
            let next = if uni {
                &mut self.peers.next_uni
            } else {
                &mut self.peers.next_bidi
            };
            let id = stream_id(peer, uni, *next);
            if streams.recv_stream(id).is_some() {
                *next += 1;
                return Some(id);
            }
        }
        None
    }

    fn close(&mut self, code: Code) {
        if self.peers.closed.is_none() {
            self.peers.closed = Some(code);
            // Already closed (by the peer or a transport error): the
            // existing close stands.
            let _ = self.endpoint.close(self.conn, code.0, self.now_ns);
        }
    }
}
