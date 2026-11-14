//! The QUIC handshake inside [`WarpEndpoint`](crate::endpoint::WarpEndpoint)
//! (wave-five integration A, issue #135).
//!
//! Lane 12's sans-I/O TLS driver ([`QuicTls`]) runs over lane 13's
//! per-space CRYPTO buffers ([`crate::stream::CryptoStream`]): received
//! CRYPTO frames are reassembled by the stream layer and handed to TLS in
//! order; TLS output is queued into the same buffers, so lost handshake
//! bytes are retransmitted as ranges like stream data. This module holds
//! the per-connection handshake state the endpoint keeps; the packet path
//! itself (long headers, per-space keys, ACKs and probes) is in
//! [`crate::endpoint`].

use std::collections::VecDeque;

use vesper_warp_crypto::{Opener, Sealer};
use vesper_warp_tls::{QuicTls, Space, TransportParameters};

use crate::PnSpace;
use crate::ack::ReceivedPackets;
use crate::stream::PeerParams;

pub use vesper_warp_tls::{self as tls, ClientSetup, ServerSetup, TlsError};

/// Smallest datagram that may carry a client Initial, and to which every
/// ack-eliciting Initial is padded (RFC 9000 §14.1).
pub const MIN_INITIAL_DATAGRAM: usize = 1200;

/// Smallest Destination Connection ID a client may put in its first
/// Initial (RFC 9000 §7.2).
pub const MIN_ORIGINAL_DCID: usize = 8;

/// Amplification factor before the server validates the client's address
/// (RFC 9000 §8.1).
pub const AMPLIFICATION_FACTOR: u64 = 3;

/// Where the connection IDs a server issues come from.
pub trait CidSource {
    /// Fills `out` with a fresh connection ID.
    fn next_cid(&mut self, out: &mut [u8]);
}

/// Connection IDs from rustls's CSPRNG (the aws-lc-rs provider lane 12
/// uses), so they are unpredictable (RFC 9000 §5.1).
#[derive(Debug, Default, Clone, Copy)]
pub struct RandomCids;

impl CidSource for RandomCids {
    fn next_cid(&mut self, out: &mut [u8]) {
        if vesper_warp_tls::config::provider()
            .secure_random
            .fill(out)
            .is_err()
        {
            // The provider's RNG does not fail in practice; a zeroed ID is
            // still unique-checked against the connection table.
            out.fill(0);
        }
    }
}

/// What a server endpoint needs to accept connections.
pub struct Listener {
    pub setup: ServerSetup,
    pub cids: Box<dyn CidSource>,
}

impl Listener {
    /// Accepts with `setup` and [`RandomCids`].
    pub fn new(setup: ServerSetup) -> Self {
        Self {
            setup,
            cids: Box::new(RandomCids),
        }
    }

    pub fn with_cids(setup: ServerSetup, cids: Box<dyn CidSource>) -> Self {
        Self { setup, cids }
    }
}

/// Where a connection's handshake stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakePhase {
    /// Opened with caller-supplied 1-RTT keys; there is no handshake.
    External,
    /// TLS is running.
    InProgress,
    /// TLS finished (RFC 9001 §4.1.1).
    Complete,
    /// The handshake is confirmed (RFC 9001 §4.1.2).
    Confirmed,
}

/// Connection-level events for the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionEvent {
    /// A server accepted a new connection from a client Initial.
    Accepted(vesper_warp_shard::SlotHandle),
    /// The handshake completed; 1-RTT keys are installed.
    HandshakeComplete(vesper_warp_shard::SlotHandle),
    /// The handshake is confirmed; Handshake keys are discarded.
    HandshakeConfirmed(vesper_warp_shard::SlotHandle),
    /// Client: the server accepted 0-RTT; data sent in it counts.
    ZeroRttAccepted(vesper_warp_shard::SlotHandle),
    /// Client: the server refused 0-RTT. Every stream was reset to a fresh
    /// state (RFC 9000 §7.4.1); the application sends its data again.
    ZeroRttRejected(vesper_warp_shard::SlotHandle),
    /// Client: a Retry was accepted and the handshake restarted with its
    /// token.
    Retried(vesper_warp_shard::SlotHandle),
    /// Client: a NEW_TOKEN token was stored for the next connection to the
    /// same server name.
    NewToken(vesper_warp_shard::SlotHandle),
    /// The peer's new address (server) or our new path (client) passed
    /// path validation (RFC 9000 §8.2).
    PathValidated(vesper_warp_shard::SlotHandle),
    /// Path validation failed; the connection went back to its last
    /// validated path, or stays where it was.
    PathFailed(vesper_warp_shard::SlotHandle),
    /// Server: the peer moved to a new address (RFC 9000 §9.3); its
    /// validation is in progress.
    PeerMigrated(vesper_warp_shard::SlotHandle),
    /// The connection moved to the server's preferred address (RFC 9000
    /// §9.6) after validating it: a client now sends there, a server now
    /// sends from there (wave seven).
    MovedToPreferredAddress(vesper_warp_shard::SlotHandle),
    /// The connection is gone and its slot was freed: after the closing or
    /// draining period, on idle timeout, on a stateless reset or a
    /// Version Negotiation failure. The handle is stale from now on.
    Closed {
        conn: vesper_warp_shard::SlotHandle,
        error: crate::endpoint::ConnectionError,
    },
}

/// The packet-number space a TLS encryption level maps to.
pub(crate) const fn pn_space(space: Space) -> PnSpace {
    match space {
        Space::Initial => PnSpace::Initial,
        Space::Handshake => PnSpace::Handshake,
        Space::Data => PnSpace::ApplicationData,
    }
}

/// The stream-layer view of negotiated transport parameters.
pub(crate) const fn peer_params(p: &TransportParameters) -> PeerParams {
    PeerParams {
        initial_max_data: p.initial_max_data,
        initial_max_stream_data_bidi_local: p.initial_max_stream_data_bidi_local,
        initial_max_stream_data_bidi_remote: p.initial_max_stream_data_bidi_remote,
        initial_max_stream_data_uni: p.initial_max_stream_data_uni,
        initial_max_streams_bidi: p.initial_max_streams_bidi,
        initial_max_streams_uni: p.initial_max_streams_uni,
    }
}

/// Stream limits before the peer's transport parameters arrive: nothing
/// may be sent.
pub(crate) const NO_CREDIT: PeerParams = PeerParams {
    initial_max_data: 0,
    initial_max_stream_data_bidi_local: 0,
    initial_max_stream_data_bidi_remote: 0,
    initial_max_stream_data_uni: 0,
    initial_max_streams_bidi: 0,
    initial_max_streams_uni: 0,
};

/// A long-header packet-number space (Initial or Handshake).
pub(crate) struct LongSpace {
    pub sealer: Option<Sealer>,
    pub opener: Option<Opener>,
    pub received: ReceivedPackets<{ crate::endpoint::ACK_RANGES }>,
    /// `(packet number, CRYPTO offset, length)` per sent packet carrying
    /// CRYPTO data, in packet-number order.
    pub sent: VecDeque<(u64, u64, u32)>,
    pub discarded: bool,
    /// Next packet number: kept across new keys, since a client must not
    /// reset packet numbers after Retry (RFC 9000 §17.2.5.3).
    pub next_pn: u64,
}

impl LongSpace {
    fn new(space: PnSpace, max_ack_delay_ns: u64, tracked: usize) -> Self {
        Self {
            sealer: None,
            opener: None,
            received: ReceivedPackets::new(space, max_ack_delay_ns),
            sent: VecDeque::with_capacity(tracked),
            discarded: false,
            next_pn: 0,
        }
    }

    /// Removes the record of packet `pn`, if it carried CRYPTO data.
    pub fn take(&mut self, pn: u64) -> Option<(u64, u32)> {
        let i = self.sent.partition_point(|(p, _, _)| *p < pn);
        match self.sent.get(i) {
            Some(&(p, off, len)) if p == pn => {
                self.sent.remove(i);
                Some((off, len))
            }
            _ => None,
        }
    }
}

/// The server's anti-amplification accounting (RFC 9000 §8.1).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Amplification {
    pub received: u64,
    pub sent: u64,
    pub validated: bool,
}

impl Amplification {
    /// Whether `bytes` more may be sent.
    pub fn allows(&self, bytes: u64) -> bool {
        self.validated
            || self.sent.saturating_add(bytes) <= self.received.saturating_mul(AMPLIFICATION_FACTOR)
    }
}

/// One connection's handshake state.
pub(crate) struct Hs {
    pub tls: QuicTls,
    /// Initial and Handshake.
    pub spaces: [LongSpace; 2],
    /// CRYPTO bytes handed to TLS, per TLS space.
    pub rx_offset: [u64; 3],
    /// CRYPTO bytes TLS produced, per TLS space.
    pub tx_offset: [u64; 3],
    /// TLS output that did not fit the CRYPTO send buffer yet.
    pub pending: [VecDeque<u8>; 3],
    /// Server only.
    pub amp: Option<Amplification>,
    local_cid: [u8; 20],
    local_cid_len: u8,
    /// Client: the server's Initial SCID was seen and adopted.
    pub peer_scid_known: bool,
    /// Client: the first Handshake packet was sent (Initial keys go).
    pub handshake_sent: bool,
    /// Server: a Handshake packet was processed (address validated,
    /// Initial keys go).
    pub handshake_received: bool,
    /// Client: a Handshake-space ACK arrived (RFC 9002
    /// `PeerCompletedAddressValidation`).
    pub handshake_acked: bool,
    /// A HANDSHAKE_DONE frame arrived in the packet being processed.
    pub done_received: bool,
    pub peer_params_applied: bool,
    /// Client: the server name, for the NEW_TOKEN cache.
    pub server_name: Option<Box<str>>,
    /// Client: the Destination Connection ID of its first Initial, for
    /// the Retry integrity tag (RFC 9001 §5.8).
    pub original_dcid: vesper_warp_shard::ConnectionId,
    /// Client: the token every Initial carries (from Retry or NEW_TOKEN).
    pub token: Option<Box<[u8]>>,
    /// Client: a Retry was accepted.
    pub retried: bool,
    /// Client: stream limits came from remembered 0-RTT parameters.
    pub zero_rtt_params: bool,
}

impl Hs {
    pub fn new(
        tls: QuicTls,
        local_cid: &[u8],
        max_ack_delay_ns: u64,
        tracked: usize,
        crypto_buffer: usize,
    ) -> Box<Self> {
        let mut cid = [0u8; 20];
        cid[..local_cid.len()].copy_from_slice(local_cid);
        let server = tls.side() == vesper_warp_tls::Side::Server;
        let pending = || VecDeque::with_capacity(crypto_buffer);
        Box::new(Self {
            tls,
            spaces: [
                LongSpace::new(PnSpace::Initial, max_ack_delay_ns, tracked),
                LongSpace::new(PnSpace::Handshake, max_ack_delay_ns, tracked),
            ],
            rx_offset: [0; 3],
            tx_offset: [0; 3],
            pending: [pending(), pending(), pending()],
            amp: server.then(Amplification::default),
            local_cid: cid,
            local_cid_len: local_cid.len() as u8,
            peer_scid_known: false,
            handshake_sent: false,
            handshake_received: false,
            handshake_acked: false,
            done_received: false,
            peer_params_applied: false,
            server_name: None,
            original_dcid: vesper_warp_shard::ConnectionId::from_slice(&[])
                .expect("an empty connection ID"),
            token: None,
            retried: false,
            zero_rtt_params: false,
        })
    }

    pub fn local_cid(&self) -> &[u8] {
        &self.local_cid[..usize::from(self.local_cid_len)]
    }

    pub fn is_server(&self) -> bool {
        self.amp.is_some()
    }

    pub fn phase(&self) -> HandshakePhase {
        if self.tls.is_handshake_confirmed() {
            HandshakePhase::Confirmed
        } else if self.tls.is_handshake_complete() {
            HandshakePhase::Complete
        } else {
            HandshakePhase::InProgress
        }
    }

    /// The long space of `space` (Initial or Handshake).
    pub fn long(&mut self, space: PnSpace) -> Option<&mut LongSpace> {
        match space {
            PnSpace::Initial => Some(&mut self.spaces[0]),
            PnSpace::Handshake => Some(&mut self.spaces[1]),
            PnSpace::ApplicationData => None,
        }
    }
}
