//! [`crate::conn`] forwarded to the Vesper build.
//!
//! The connection state is one Vesper `Tcb` handle. A user-supplied
//! [`CongestionController`] stays in Rust: the Vesper connection takes the
//! congestion window as an argument of `tcb_poll_transmit`, records every
//! other controller call (`on_mss`, `on_rtt_sample`, `on_event`) in order,
//! and this wrapper applies them after the call that made them. For a
//! segment that occupies sequence space, the wrapper asks the controller for
//! the departure time and reports the send, as the Rust build does at the
//! same point.

use super::binding::{self as b, Tcb};
use super::types::{CcCmd, TcpFail};
use super::wire::{ips_from, ips_to};
use crate::cc::{CongestionController, NewReno, SentPacket};
use crate::conn::{
    CloseReason, Config, Event, Received, Released, SendView, State, Stats, TcpError, Transmit,
};
use crate::rtt::RttEstimator;
use crate::seq::Seq;
use crate::wire::IpPair;

/// The messages of `TcpError::InvalidConfig`, indexed by the code the
/// Vesper `config_validate` returns.
pub(crate) const CONFIG_ERRORS: [&str; 5] = [
    "mss below 64",
    "recv_buffer must be in 1..=2^30",
    "window_scale above 14",
    "queues must be non-empty",
    "rto bounds",
];

fn err(e: TcpFail) -> TcpError {
    match e {
        TcpFail::Wire(w) => TcpError::Wire(w),
        TcpFail::NotForThisConnection => TcpError::NotForThisConnection,
        TcpFail::RingSizeMismatch => TcpError::RingSizeMismatch,
        TcpFail::InvalidState(s) => TcpError::InvalidState(s),
        TcpFail::ConnectionClosing => TcpError::ConnectionClosing,
        TcpFail::SendViewMissing { start, end } => TcpError::SendViewMissing { start, end },
        TcpFail::BufferTooSmall => TcpError::BufferTooSmall,
        TcpFail::InvalidConfig(code) => TcpError::InvalidConfig(
            CONFIG_ERRORS
                .get(usize::from(code))
                .copied()
                .unwrap_or("invalid configuration"),
        ),
        TcpFail::ReleaseExceedsUnread => TcpError::ReleaseExceedsUnread,
        TcpFail::Overflow => TcpError::Overflow,
    }
}

// Offsets come back bounded by the slices passed in.
fn at(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl State {
    /// States in which sequence numbers of both sides are known.
    #[must_use]
    pub fn is_synchronized(self) -> bool {
        b::state_is_synchronized(self)
    }
}

/// A single TCP connection (one TCB).
#[derive(Debug)]
pub struct Connection<C: CongestionController = NewReno> {
    tcb: Tcb,
    cfg: Config,
    // A copy of the Vesper estimator, refreshed after every call that can
    // change it, so that `rtt()` can lend a reference.
    rtt: RttEstimator,
    cc: C,
}

impl Connection<NewReno> {
    /// A closed connection with the built-in NewReno controller.
    pub fn new(cfg: Config) -> Result<Connection<NewReno>, TcpError> {
        let cc = NewReno::new(u32::from(cfg.mss), cfg.initial_window_segments);
        Connection::with_controller(cfg, cc)
    }
}

impl<C: CongestionController> Connection<C> {
    /// A closed connection using `cc` for congestion control.
    pub fn with_controller(cfg: Config, cc: C) -> Result<Connection<C>, TcpError> {
        b::config_validate(cfg).map_err(err)?;
        let tcb = b::tcb_new(cfg);
        let rtt = tcb.as_ref().rtt();
        Ok(Connection { tcb, cfg, rtt, cc })
    }

    fn sync(&mut self) {
        self.rtt = self.tcb.as_ref().rtt();
    }

    // Apply the controller calls the last Vesper call recorded.
    fn apply_cc(&mut self) {
        for c in self.tcb.as_ref().cmds().iter() {
            match c {
                CcCmd::Mss(smss) => self.cc.on_mss(smss),
                CcCmd::Rtt(sample) => self.cc.on_rtt_sample(sample),
                CcCmd::Event { event, now } => self.cc.on_event(event, now),
            }
        }
    }

    // ----------------------------------------------------------------
    // Accessors
    // ----------------------------------------------------------------

    /// Current state.
    #[must_use]
    pub fn state(&self) -> State {
        self.tcb.as_ref().st()
    }

    /// Configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.tcb.as_ref().stats()
    }

    /// The congestion controller.
    #[must_use]
    pub fn controller(&self) -> &C {
        &self.cc
    }

    /// RTT estimator.
    #[must_use]
    pub fn rtt(&self) -> &RttEstimator {
        &self.rtt
    }

    /// Why the connection closed, if it closed abnormally.
    #[must_use]
    pub fn close_reason(&self) -> Option<CloseReason> {
        self.tcb.as_ref().close_reason()
    }

    /// Sender MSS (payload bytes per data segment).
    #[must_use]
    pub fn send_mss(&self) -> u32 {
        self.tcb.as_ref().smss()
    }

    /// Current peer receive window in bytes.
    #[must_use]
    pub fn send_window(&self) -> u32 {
        self.tcb.as_ref().snd_wnd()
    }

    /// Negotiated (send shift, receive shift) when window scaling is on.
    #[must_use]
    pub fn window_shifts(&self) -> (u8, u8) {
        let t = self.tcb.as_ref();
        (t.snd_wscale(), t.rcv_wscale())
    }

    /// Whether SACK was negotiated.
    #[must_use]
    pub fn sack_enabled(&self) -> bool {
        self.tcb.as_ref().sack_ok()
    }

    /// Whether timestamps were negotiated.
    #[must_use]
    pub fn timestamps_enabled(&self) -> bool {
        self.tcb.as_ref().ts_ok()
    }

    /// Stream offset one past the highest data byte acknowledged.
    #[must_use]
    pub fn acked_offset(&self) -> u64 {
        b::tcb_acked_offset(&self.tcb)
    }

    /// Stream offset of the next byte expected from the peer.
    #[must_use]
    pub fn recv_offset(&self) -> u64 {
        b::tcb_recv_offset(&self.tcb)
    }

    /// Bytes delivered to the application but not yet released.
    #[must_use]
    pub fn unread(&self) -> u64 {
        b::tcb_unread(&self.tcb)
    }

    /// Stream offset one past the last urgent byte signalled by the peer
    /// (RFC 9293 §3.1/RFC 6093 urgent-pointer semantics; the data itself is
    /// delivered inline).
    #[must_use]
    pub fn urgent_offset(&self) -> Option<u64> {
        self.tcb.as_ref().urgent_off()
    }

    /// Bytes in flight (RFC 6675 pipe).
    #[must_use]
    pub fn bytes_in_flight(&self) -> u32 {
        b::tcb_bytes_in_flight(&self.tcb)
    }

    /// RCV.NXT on the wire, once the peer's ISN is known.
    #[must_use]
    pub fn recv_next_seq(&self) -> Option<Seq> {
        b::tcb_recv_next_seq(&self.tcb).map(Seq)
    }

    /// (SND.UNA, SND.NXT) on the wire.
    #[must_use]
    pub fn send_seqs(&self) -> (Seq, Seq) {
        let (una, nxt) = b::tcb_send_seqs(&self.tcb);
        (Seq(una), Seq(nxt))
    }

    /// Number of out-of-order ranges currently buffered.
    #[must_use]
    pub fn ooo_ranges(&self) -> usize {
        at(b::tcb_ooo_ranges(&self.tcb))
    }

    // ----------------------------------------------------------------
    // User calls (RFC 9293 §3.10.1–§3.10.6)
    // ----------------------------------------------------------------

    /// Passive OPEN. `iss_base` is the caller's per-connection ISN secret
    /// component (RFC 6528 F(); this crate adds the RFC 9293 4 µs clock).
    pub fn listen(&mut self, iss_base: u32) -> Result<(), TcpError> {
        let r = b::tcb_listen(&mut self.tcb, iss_base);
        self.sync();
        r.map_err(err)
    }

    /// Active OPEN to `remote_port`. `ips` is oriented local → remote.
    pub fn connect(
        &mut self,
        now: u64,
        ips: IpPair,
        remote_port: u16,
        iss_base: u32,
    ) -> Result<(), TcpError> {
        let r = b::tcb_connect(&mut self.tcb, now, ips_to(ips), remote_port, iss_base);
        self.sync();
        r.map_err(err)
    }

    /// Make `n` more bytes of the caller's send buffer available.
    pub fn write(&mut self, n: u64) -> Result<(), TcpError> {
        b::tcb_write(&mut self.tcb, n).map_err(err)
    }

    /// Stream offset one past the last byte written.
    #[must_use]
    pub fn written(&self) -> u64 {
        b::tcb_written(&self.tcb)
    }

    /// CLOSE: send FIN after all written data.
    pub fn close(&mut self) -> Result<(), TcpError> {
        b::tcb_close(&mut self.tcb).map_err(err)
    }

    /// ABORT: send RST in synchronized states and close immediately.
    pub fn abort(&mut self) {
        b::tcb_abort(&mut self.tcb);
    }

    /// The application finished with `n` delivered bytes, reopening the
    /// receive window and freeing their ring space.
    pub fn recv_release(&mut self, n: u64) -> Result<(), TcpError> {
        b::tcb_recv_release(&mut self.tcb, n).map_err(err)
    }

    /// Next acknowledged send-buffer range to reclaim.
    pub fn take_released(&mut self) -> Option<Released> {
        b::tcb_take_released(&mut self.tcb).map(|(start, end)| Released { start, end })
    }

    /// Next pending notification.
    pub fn take_event(&mut self) -> Option<Event> {
        b::tcb_take_event(&mut self.tcb)
    }

    /// Earliest timer deadline.
    #[must_use]
    pub fn poll_timeout(&self) -> Option<u64> {
        b::tcb_poll_timeout(&self.tcb)
    }

    // ----------------------------------------------------------------
    // Segment arrival (RFC 9293 §3.10.7)
    // ----------------------------------------------------------------

    /// Process one incoming segment. `ips` is oriented as received (peer is
    /// `src`). `ring` must be the same caller-owned receive ring of
    /// `Config::recv_buffer` bytes on every call.
    pub fn on_segment<'a>(
        &mut self,
        now: u64,
        ips: IpPair,
        bytes: &'a [u8],
        ring: &'a mut [u8],
    ) -> Result<Received<'a>, TcpError> {
        let r = b::tcb_on_segment(&mut self.tcb, now, ips_to(ips), bytes, ring);
        self.apply_cc();
        self.sync();
        let r = r.map_err(err)?;
        let ring: &'a [u8] = ring;
        let (s0, r0) = (at(r.seg_start), at(r.ring_start));
        Ok(Received {
            chunks: [
                &bytes[s0..s0 + at(r.seg_len)],
                &ring[r0..r0 + at(r.ring_len1)],
                &ring[..at(r.ring_len2)],
            ],
            fin: r.fin,
        })
    }

    // ----------------------------------------------------------------
    // Timers
    // ----------------------------------------------------------------

    /// Fire every timer whose deadline is at or before `now`.
    pub fn on_timeout(&mut self, now: u64) {
        b::tcb_on_timeout(&mut self.tcb, now);
        self.apply_cc();
        self.sync();
    }

    // ----------------------------------------------------------------
    // Transmission
    // ----------------------------------------------------------------

    /// Write the next segment to send into `out`, if any. Call repeatedly
    /// until it returns `None`.
    pub fn poll_transmit(
        &mut self,
        now: u64,
        send: &SendView<'_>,
        out: &mut [u8],
    ) -> Result<Option<Transmit>, TcpError> {
        let cwnd = self.cc.cwnd_bytes();
        let tx = b::tcb_poll_transmit(&mut self.tcb, now, cwnd, send.base, send.bytes, out)
            .map_err(err)?;
        let Some(tx) = tx else {
            return Ok(None);
        };
        let mut edt_ns = tx.edt_ns;
        if tx.entry {
            // Departure time first: pacing delay must not count as RTT or
            // loss.
            edt_ns = self.cc.schedule_edt_ns(tx.bytes, now).max(now);
            if edt_ns != now {
                b::tcb_commit_edt(&mut self.tcb, edt_ns);
            }
            self.cc.on_sent(SentPacket {
                pn: tx.pn,
                sent_ns: edt_ns,
                bytes: tx.bytes,
                ack_eliciting: true,
                in_flight: true,
            });
        }
        Ok(Some(Transmit {
            len: at(tx.len),
            edt_ns,
            ips: ips_from(tx.ips),
        }))
    }
}
