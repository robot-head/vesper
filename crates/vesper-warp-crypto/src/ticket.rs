//! Linear nonce tickets, the per-key sealer that issues them, and the
//! matching opener. The ticket rules and the limits are Vesper
//! (`vesper/src/lib.ves`); this module keeps the owners.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::port::{b, err, scratch};
use crate::protect::{PacketKey, Unprotected};
use crate::schedule::DirectionalKeys;
use crate::{Cipher, CryptoError};

static NEXT_SEALER_ID: AtomicU64 = AtomicU64::new(1);
static LEAKED_TICKETS: AtomicU64 = AtomicU64::new(0);

/// Process-wide count of [`NonceTicket`]s dropped without being sealed or
/// abandoned. Their packet numbers stay burned; the count is for auditors.
pub fn leaked_nonce_tickets() -> u64 {
    LEAKED_TICKETS.load(Ordering::Relaxed)
}

/// The right to seal exactly one packet with one packet number under one
/// key. Issued only by [`Sealer::ticket`]; consumed only by
/// [`Sealer::seal`] or [`Sealer::abandon`]. Not `Clone`/`Copy`, so a nonce
/// can be used at most once.
#[must_use = "a NonceTicket must be sealed or abandoned; dropping it is counted as a leak"]
#[derive(Debug)]
pub struct NonceTicket {
    sealer: u64,
    generation: u64,
    pn: u64,
    live: bool,
}

impl NonceTicket {
    /// The full packet number this ticket authorises.
    pub fn packet_number(&self) -> u64 {
        self.pn
    }

    /// The key generation (number of key updates) it was issued under.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn consume(mut self) {
        self.live = false;
    }
}

impl Drop for NonceTicket {
    fn drop(&mut self) {
        if self.live {
            LEAKED_TICKETS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A failed seal: the ticket is handed back unconsumed.
#[must_use = "the returned ticket must be sealed again or abandoned"]
#[derive(Debug)]
pub struct SealError {
    pub ticket: NonceTicket,
    pub error: CryptoError,
}

/// Ticket accounting for one sealer. `outstanding` includes leaked tickets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketAudit {
    pub issued: u64,
    pub sealed: u64,
    pub abandoned: u64,
    pub outstanding: u64,
}

/// Per-key, per-direction packet sealer. Issues tickets for strictly
/// increasing packet numbers and enforces the RFC 9001 §6.6
/// confidentiality limit per key.
pub struct Sealer {
    state: b::repr::SealerSt,
    keys: DirectionalKeys,
}

impl Sealer {
    /// A sealer for Initial/Handshake/0-RTT keys (no key phase).
    pub fn new(keys: DirectionalKeys) -> Self {
        Self::with_phase(keys, None)
    }

    pub(crate) fn with_phase(keys: DirectionalKeys, key_phase: Option<bool>) -> Self {
        let phase = match key_phase {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        };
        let id = NEXT_SEALER_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            state: b::sealer_new(id, keys.packet.cipher().into(), phase),
            keys,
        }
    }

    /// Lowers (never raises) the per-key confidentiality limit, e.g. for a
    /// stricter local policy or tests.
    pub fn with_confidentiality_limit(mut self, limit: u64) -> Self {
        b::sealer_lower_limit(&mut self.state, limit);
        self
    }

    pub fn cipher(&self) -> Cipher {
        self.keys.packet.cipher()
    }

    /// The smallest packet number a new ticket may carry.
    pub fn next_packet_number(&self) -> u64 {
        self.state.next_pn
    }

    pub fn generation(&self) -> u64 {
        self.state.generation
    }

    /// Tickets still available under the current key.
    pub fn remaining_under_key(&self) -> u64 {
        b::sealer_remaining(&self.state)
    }

    pub fn audit(&self) -> TicketAudit {
        let s = &self.state;
        TicketAudit {
            issued: s.issued,
            sealed: s.sealed,
            abandoned: s.abandoned,
            outstanding: s.issued - s.sealed - s.abandoned,
        }
    }

    fn issue(&self, pn: u64) -> NonceTicket {
        NonceTicket {
            sealer: self.state.id,
            generation: self.state.generation,
            pn,
            live: true,
        }
    }

    /// Issues a ticket for `pn`, which must exceed every packet number
    /// already issued by this sealer and be `< 2^62`.
    pub fn ticket(&mut self, pn: u64) -> Result<NonceTicket, CryptoError> {
        b::sealer_ticket(&mut self.state, pn).map_err(err)?;
        Ok(self.issue(pn))
    }

    /// Issues a ticket for the next packet number.
    pub fn next_ticket(&mut self) -> Result<NonceTicket, CryptoError> {
        let pn = b::sealer_next_ticket(&mut self.state).map_err(err)?;
        Ok(self.issue(pn))
    }

    /// Seals one packet in place and consumes the ticket.
    ///
    /// `buf` holds the unprotected header (first byte at 0, truncated packet
    /// number at `pn_offset`, its length in the first byte's low two bits),
    /// then `payload_len` plaintext bytes, then at least [`crate::TAG_LEN`]
    /// spare bytes. Returns the protected packet length. On any error the
    /// buffer is untouched and the ticket is returned.
    pub fn seal(
        &mut self,
        ticket: NonceTicket,
        buf: &mut [u8],
        pn_offset: usize,
        payload_len: usize,
    ) -> Result<usize, SealError> {
        let sealed = b::sealer_seal(
            &mut self.state,
            ticket.sealer,
            ticket.generation,
            ticket.pn,
            self.keys.packet.lend(),
            self.keys.packet.iv(),
            buf,
            pn_offset as u64,
            payload_len as u64,
            &mut scratch(),
        );
        match sealed {
            Ok(len) => {
                let len = len as usize;
                // The sample was checked to lie inside the sealed packet.
                self.keys
                    .header
                    .apply(&mut buf[..len], pn_offset)
                    .expect("header protection after a checked seal");
                ticket.consume();
                Ok(len)
            }
            Err(e) => Err(SealError {
                ticket,
                error: err(e),
            }),
        }
    }

    /// Gives up a ticket without sealing; its packet number stays burned.
    /// A ticket from another sealer is handed back.
    pub fn abandon(&mut self, ticket: NonceTicket) -> Result<(), NonceTicket> {
        if !b::sealer_abandon(&mut self.state, ticket.sealer) {
            return Err(ticket);
        }
        ticket.consume();
        Ok(())
    }

    /// Installs the next-phase packet key (header key is unchanged, RFC
    /// 9001 §6). Outstanding tickets become stale. The caller moves the
    /// Vesper state with it (`onertt_rotated_send`).
    pub(crate) fn install_packet_key(&mut self, packet: PacketKey) {
        self.keys.packet = packet;
    }

    pub(crate) fn state(&self) -> &b::repr::SealerSt {
        &self.state
    }

    pub(crate) fn state_mut(&mut self) -> &mut b::repr::SealerSt {
        &mut self.state
    }

    pub(crate) fn key_phase(&self) -> Option<bool> {
        match self.state.key_phase.get() {
            0 => None,
            p => Some(p == 2),
        }
    }
}

/// A successfully opened packet. The plaintext is at
/// `buf[header_len..header_len + payload_len]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    pub packet_number: u64,
    pub header_len: usize,
    pub payload_len: usize,
    /// Unprotected first byte.
    pub first_byte: u8,
}

/// Per-key, per-direction opener for Initial/Handshake/0-RTT packets.
pub struct Opener {
    state: b::repr::OpenerSt,
    keys: DirectionalKeys,
}

impl Opener {
    pub fn new(keys: DirectionalKeys) -> Self {
        Self {
            state: b::opener_new(keys.packet.cipher().into()),
            keys,
        }
    }

    /// Lowers (never raises) the integrity limit.
    pub fn with_integrity_limit(mut self, limit: u64) -> Self {
        b::opener_lower_limit(&mut self.state, limit);
        self
    }

    /// Packets that failed authentication (RFC 9001 §6.6 counter).
    pub fn authentication_failures(&self) -> u64 {
        self.state.failures
    }

    pub fn largest_opened(&self) -> Option<u64> {
        self.state.has_largest.get().then_some(self.state.largest)
    }

    /// Step 2 of the receive boundary: removes header protection in place.
    /// No opener state changes.
    pub fn remove_header_protection(
        &self,
        buf: &mut [u8],
        pn_offset: usize,
    ) -> Result<Unprotected, CryptoError> {
        self.keys.header.remove(buf, pn_offset)
    }

    /// Step 4 of the receive boundary: opens `buf[hdr.header_len()..]` with
    /// the **full** reconstructed packet number. `buf` must be exactly one
    /// packet (coalesced packets split by the packet core). On error the
    /// payload bytes are unchanged; the only state change is the §6.6
    /// failure counter on authentication failure.
    pub fn open(
        &mut self,
        buf: &mut [u8],
        hdr: &Unprotected,
        full_pn: u64,
    ) -> Result<Opened, CryptoError> {
        let o = b::opener_open(
            &mut self.state,
            self.keys.packet.lend(),
            self.keys.packet.iv(),
            buf,
            &hdr.to_repr(),
            full_pn,
            &mut scratch(),
        )
        .map_err(err)?;
        Ok(Opened {
            packet_number: o.packet_number,
            header_len: o.header_len as usize,
            payload_len: o.payload_len as usize,
            first_byte: o.first_byte.get(),
        })
    }

    /// Steps 2–4 in one call: `reconstruct` maps the unprotected header to
    /// the full packet number (the packet core's job). On any error header
    /// protection is re-applied, so `buf` is byte-identical to its input.
    pub fn open_packet(
        &mut self,
        buf: &mut [u8],
        pn_offset: usize,
        reconstruct: impl FnOnce(&Unprotected) -> Option<u64>,
    ) -> Result<Opened, CryptoError> {
        b::opener_check_limit(&self.state).map_err(err)?;
        let hdr = self.remove_header_protection(buf, pn_offset)?;
        let result = match reconstruct(&hdr) {
            Some(pn) => self.open(buf, &hdr, pn),
            None => Err(CryptoError::PacketNumberReconstruction),
        };
        if result.is_err() {
            hdr.reapply(buf);
        }
        result
    }
}
